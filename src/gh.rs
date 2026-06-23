use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Thin `gh` CLI transport. `REIN_GH` overrides the binary (used by e2e tests
/// to inject a fake gh).
pub struct Gh {
    bin: String,
    /// Directory to run `gh` in. `gh` infers the target repo from its cwd, so
    /// this must be the task's repo — essential once the dashboard drives
    /// actions across projects from an arbitrary launch directory.
    cwd: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub struct RemoteIssue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
}

impl Gh {
    pub fn new() -> Gh {
        let bin = std::env::var("REIN_GH").unwrap_or_else(|_| "gh".to_string());
        Gh { bin, cwd: None }
    }

    /// `gh` bound to a repo directory, so its target repo is unambiguous
    /// regardless of where the rein process was launched.
    pub fn in_dir(dir: &Path) -> Gh {
        Gh {
            cwd: Some(dir.to_path_buf()),
            ..Gh::new()
        }
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to run '{}' — is gh installed?", self.bin))?;
        if let Some(body) = stdin {
            child
                .stdin
                .take()
                .context("no stdin")?
                .write_all(body.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "{} {} failed: {}",
                self.bin,
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    fn parse_number_from_url(output: &str, segment: &str) -> Result<u64> {
        output
            .split_whitespace()
            .filter_map(|tok| {
                let tok = tok.trim_end_matches('/');
                let (head, num) = tok.rsplit_once('/')?;
                if head.ends_with(segment) {
                    num.parse().ok()
                } else {
                    None
                }
            })
            .next_back()
            .ok_or_else(|| anyhow!("could not parse number from gh output: {}", output.trim()))
    }

    pub fn ensure_label(&self) {
        let _ = self.run(
            &[
                "label",
                "create",
                "rein",
                "--description",
                "rein managed task",
                "--force",
            ],
            None,
        );
    }

    pub fn issue_create(&self, title: &str, body: &str, project: Option<&str>) -> Result<u64> {
        let mut args = vec![
            "issue", "create", "--title", title, "--body-file", "-", "--label", "rein",
        ];
        // `--project <title|number>` files the new issue onto a GitHub Project
        // board (Projects v2); omitted when no project was chosen.
        if let Some(p) = project {
            args.extend(["--project", p]);
        }
        let out = self.run(&args, Some(body))?;
        Self::parse_number_from_url(&out, "/issues")
    }

    /// Titles of the owner's GitHub Projects (v2), for the optional issue→project
    /// picker. Best-effort: an empty list if `gh` lacks the `project` scope or
    /// the command errors. `owner` scopes to a user/org login when known.
    pub fn project_titles(&self, owner: Option<&str>) -> Vec<String> {
        let mut args = vec!["project", "list", "--format", "json"];
        if let Some(o) = owner {
            args.extend(["--owner", o]);
        }
        let Ok(out) = self.run(&args, None) else {
            return Vec::new();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) else {
            return Vec::new();
        };
        // `gh project list --format json` may return either a top-level array or
        // a `{ "projects": [...] }` object depending on the gh version — accept both.
        let arr = v
            .as_array()
            .cloned()
            .or_else(|| v.get("projects").and_then(|x| x.as_array()).cloned())
            .unwrap_or_default();
        arr.iter()
            .filter_map(|p| p.get("title").and_then(|t| t.as_str()).map(str::to_string))
            .collect()
    }

    pub fn issue_view_body(&self, number: u64) -> Result<String> {
        let out = self.run(&["issue", "view", &number.to_string(), "--json", "body"], None)?;
        let v: serde_json::Value = serde_json::from_str(&out).context("bad gh issue view JSON")?;
        Ok(v["body"].as_str().unwrap_or_default().to_string())
    }

    pub fn issue_edit_body(&self, number: u64, body: &str) -> Result<()> {
        self.run(
            &["issue", "edit", &number.to_string(), "--body-file", "-"],
            Some(body),
        )
        .map(|_| ())
    }

    pub fn issue_list_rein(&self) -> Result<Vec<RemoteIssue>> {
        let out = self.run(
            &[
                "issue",
                "list",
                "--label",
                "rein",
                "--state",
                "open",
                "--json",
                "number,title,body",
            ],
            None,
        )?;
        serde_json::from_str(&out).context("bad gh issue list JSON")
    }

    pub fn issue_close(&self, number: u64, not_planned: bool, comment: &str) -> Result<()> {
        let n = number.to_string();
        let mut args = vec!["issue", "close", n.as_str(), "--comment", comment];
        if not_planned {
            args.extend(["--reason", "not planned"]);
        }
        self.run(&args, None).map(|_| ())
    }

    pub fn issue_comment(&self, number: u64, body: &str) -> Result<()> {
        self.run(
            &["issue", "comment", &number.to_string(), "--body", body],
            None,
        )
        .map(|_| ())
    }

    pub fn pr_create_draft(&self, title: &str, body: &str, head: &str) -> Result<u64> {
        let out = self.run(
            &[
                "pr",
                "create",
                "--draft",
                "--title",
                title,
                "--body-file",
                "-",
                "--head",
                head,
            ],
            Some(body),
        )?;
        Self::parse_number_from_url(&out, "/pull")
    }

    pub fn pr_view_body(&self, number: u64) -> Result<String> {
        let out = self.run(&["pr", "view", &number.to_string(), "--json", "body"], None)?;
        let v: serde_json::Value = serde_json::from_str(&out).context("bad gh pr view JSON")?;
        Ok(v["body"].as_str().unwrap_or_default().to_string())
    }

    pub fn pr_edit_body(&self, number: u64, body: &str) -> Result<()> {
        self.run(
            &["pr", "edit", &number.to_string(), "--body-file", "-"],
            Some(body),
        )
        .map(|_| ())
    }
}

impl Default for Gh {
    fn default() -> Self {
        Self::new()
    }
}
