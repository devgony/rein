use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A discovered git repository, possibly a linked worktree.
#[derive(Debug, Clone)]
pub struct Repo {
    /// Top-level working directory of the current worktree.
    pub workdir: PathBuf,
    /// Git dir of the current worktree (`.git` or `.git/worktrees/<name>`).
    pub git_dir: PathBuf,
    /// Common dir shared by all worktrees.
    pub common_dir: PathBuf,
}

pub fn git_in(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

impl Repo {
    pub fn discover(cwd: &Path) -> Result<Repo> {
        let out = git_in(
            cwd,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ],
        )
        .map_err(|_| anyhow!("not inside a git repository"))?;
        let mut lines = out.lines();
        let mut next = || -> Result<PathBuf> {
            Ok(PathBuf::from(
                lines.next().ok_or_else(|| anyhow!("unexpected rev-parse output"))?,
            ))
        };
        Ok(Repo {
            workdir: next()?,
            git_dir: next()?,
            common_dir: next()?,
        })
    }

    pub fn config_get(&self, key: &str) -> Option<String> {
        git_in(&self.workdir, &["config", "--get", key])
            .ok()
            .filter(|s| !s.is_empty())
    }

    pub fn config_set(&self, key: &str, val: &str) -> Result<()> {
        git_in(&self.workdir, &["config", key, val]).map(|_| ())
    }

    pub fn remote_url(&self) -> Option<String> {
        self.config_get("remote.origin.url")
    }

    pub fn worktree_add(&self, path: &Path, branch: &str) -> Result<()> {
        git_in(
            &self.workdir,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                path.to_str().context("non-utf8 worktree path")?,
            ],
        )
        .map(|_| ())
    }

    pub fn worktree_remove(&self, path: &Path, force: bool) -> Result<()> {
        let p = path.to_str().context("non-utf8 worktree path")?;
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(p);
        git_in(&self.workdir, &args).map(|_| ())
    }

    pub fn branch_create_and_switch(&self, branch: &str) -> Result<()> {
        git_in(&self.workdir, &["switch", "-c", branch]).map(|_| ())
    }

    /// Whether a local branch already exists — used to turn the predictable
    /// `git worktree add -b`/`switch -c` collision into an actionable message.
    pub fn branch_exists(&self, branch: &str) -> bool {
        git_in(
            &self.workdir,
            &["show-ref", "--verify", "--quiet", &format!("refs/heads/{}", branch)],
        )
        .is_ok()
    }

    /// Best-effort base branch a PR would target: the remote's default head if
    /// known, else a local `main`/`master`. `None` when nothing obvious exists.
    pub fn default_branch(&self) -> Option<String> {
        if let Ok(s) = git_in(&self.workdir, &["rev-parse", "--abbrev-ref", "origin/HEAD"]) {
            if let Some(b) = s.strip_prefix("origin/") {
                if !b.is_empty() {
                    return Some(b.to_string());
                }
            }
        }
        ["main", "master"]
            .into_iter()
            .find(|b| self.branch_exists(b))
            .map(str::to_string)
    }

    /// Number of commits on `branch` not reachable from `base` (`base..branch`).
    pub fn commits_ahead(&self, base: &str, branch: &str) -> Result<usize> {
        let out = git_in(
            &self.workdir,
            &["rev-list", "--count", &format!("{}..{}", base, branch)],
        )?;
        Ok(out.trim().parse().unwrap_or(0))
    }

    /// Push `branch` to origin and set upstream, so `gh pr create --head` finds
    /// it on the remote.
    pub fn push_branch(&self, branch: &str) -> Result<()> {
        git_in(&self.workdir, &["push", "-u", "origin", branch])
            .context("git push failed")
            .map(|_| ())
    }

    /// Path of the per-worktree task pointer file (truth of task↔worktree binding).
    pub fn task_pointer(&self) -> PathBuf {
        self.git_dir.join("rein-task")
    }

    pub fn read_task_pointer(&self) -> Option<String> {
        std::fs::read_to_string(self.task_pointer())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn write_task_pointer(&self, task_id: &str) -> Result<()> {
        crate::util::atomic_write(&self.task_pointer(), &format!("{}\n", task_id))
    }
}

pub fn is_dirty(worktree: &Path) -> Result<bool> {
    Ok(!git_in(worktree, &["status", "--porcelain"])?.is_empty())
}
