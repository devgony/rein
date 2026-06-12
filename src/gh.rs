use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::io::Write;
use std::process::{Command, Stdio};

/// Thin `gh` CLI transport. `REIN_GH` overrides the binary (used by e2e tests
/// to inject a fake gh).
pub struct Gh {
    bin: String,
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
        Gh { bin }
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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

    pub fn issue_create(&self, title: &str, body: &str) -> Result<u64> {
        let out = self.run(
            &[
                "issue", "create", "--title", title, "--body-file", "-", "--label", "rein",
            ],
            Some(body),
        )?;
        Self::parse_number_from_url(&out, "/issues")
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
