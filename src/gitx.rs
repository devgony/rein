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

/// One entry of `git worktree list --porcelain`. The first entry git prints is
/// the repo's primary (main) worktree.
#[derive(Debug, Clone, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Checked-out commit (`None` for a bare main worktree).
    pub head: Option<String>,
    /// Short branch name (`None` when detached or bare).
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
    /// The primary worktree — the first one git lists; can't be removed/locked.
    pub is_main: bool,
}

/// Parse the blank-line-separated blocks of `git worktree list --porcelain`.
/// Split out from the git call so it can be unit-tested without a repo.
pub fn parse_worktrees(out: &str) -> Vec<Worktree> {
    let mut list: Vec<Worktree> = Vec::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            let is_main = list.is_empty();
            list.push(Worktree {
                path: PathBuf::from(p),
                head: None,
                branch: None,
                bare: false,
                detached: false,
                locked: false,
                prunable: false,
                is_main,
            });
        } else if let Some(w) = list.last_mut() {
            if let Some(h) = line.strip_prefix("HEAD ") {
                w.head = Some(h.to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                w.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "bare" {
                w.bare = true;
            } else if line == "detached" {
                w.detached = true;
            } else if line == "locked" || line.starts_with("locked ") {
                w.locked = true;
            } else if line == "prunable" || line.starts_with("prunable ") {
                w.prunable = true;
            }
        }
    }
    list
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

    /// Every worktree of this repo (main + linked), via the porcelain listing.
    pub fn worktree_list(&self) -> Result<Vec<Worktree>> {
        let out = git_in(&self.workdir, &["worktree", "list", "--porcelain"])?;
        Ok(parse_worktrees(&out))
    }

    /// Add a worktree at `path` on `branch`: creates the branch (`-b`) when it
    /// doesn't exist yet, otherwise checks out the existing one. The stricter
    /// `worktree_add` (always `-b`) backs the task-claim flow, where reusing an
    /// existing branch should be an error.
    pub fn worktree_add_branch(&self, path: &Path, branch: &str) -> Result<()> {
        let p = path.to_str().context("non-utf8 worktree path")?;
        let args: Vec<&str> = if self.branch_exists(branch) {
            vec!["worktree", "add", p, branch]
        } else {
            vec!["worktree", "add", "-b", branch, p]
        };
        git_in(&self.workdir, &args).map(|_| ())
    }

    /// Lock or unlock a linked worktree (git refuses to lock the main one).
    pub fn worktree_lock(&self, path: &Path, lock: bool) -> Result<()> {
        let p = path.to_str().context("non-utf8 worktree path")?;
        let sub = if lock { "lock" } else { "unlock" };
        git_in(&self.workdir, &["worktree", sub, p]).map(|_| ())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_worktree_list() {
        let out = "\
worktree /repo
HEAD abc123
branch refs/heads/main

worktree /repo/.wt/feat
HEAD def456
branch refs/heads/feat
locked

worktree /repo/.wt/detached
HEAD 0a0a0a
detached

worktree /repo/.wt/gone
HEAD 111222
branch refs/heads/gone
prunable gitdir file points to non-existent location
";
        let wts = parse_worktrees(out);
        assert_eq!(wts.len(), 4);

        // first entry is the main worktree
        assert_eq!(wts[0].path, PathBuf::from("/repo"));
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert!(wts[0].is_main);
        assert!(!wts[0].locked);

        // a locked, branch-backed linked worktree
        assert_eq!(wts[1].branch.as_deref(), Some("feat"));
        assert!(wts[1].locked);
        assert!(!wts[1].is_main);

        // detached: no branch, but a HEAD
        assert_eq!(wts[2].branch, None);
        assert!(wts[2].detached);
        assert_eq!(wts[2].head.as_deref(), Some("0a0a0a"));

        // prunable carries a reason after the keyword
        assert!(wts[3].prunable);
        assert_eq!(wts[3].branch.as_deref(), Some("gone"));
    }

    #[test]
    fn parses_bare_main_worktree() {
        let out = "worktree /repo\nbare\n";
        let wts = parse_worktrees(out);
        assert_eq!(wts.len(), 1);
        assert!(wts[0].bare);
        assert!(wts[0].is_main);
        assert_eq!(wts[0].head, None);
        assert_eq!(wts[0].branch, None);
    }
}
