use crate::gitx::Repo;
use crate::task::TaskDoc;
use crate::util;
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub const STATUSES: [&str; 4] = ["inbox", "active", "done", "canceled"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Inbox,
    Active,
    Done,
    Canceled,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Inbox => "inbox",
            Status::Active => "active",
            Status::Done => "done",
            Status::Canceled => "canceled",
        }
    }
    pub fn parse(s: &str) -> Option<Status> {
        match s {
            "inbox" => Some(Status::Inbox),
            "active" => Some(Status::Active),
            "done" => Some(Status::Done),
            "canceled" => Some(Status::Canceled),
            _ => None,
        }
    }
}

/// A task located in the store.
#[derive(Debug, Clone)]
pub struct TaskRef {
    pub id: String,
    pub slug: String,
    pub status: Status,
    pub path: PathBuf,
    pub doc: TaskDoc,
}

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
}

/// A store paired with the project it belongs to — used by the cross-project
/// dashboard to label tasks and locate each task's repo for git/gh actions.
#[derive(Debug, Clone)]
pub struct StoreInfo {
    pub store: Store,
    /// Human-friendly project name (remote `owner/repo`, else repo dir name).
    pub project: String,
    /// The repo working directory, derived from `meta.json`'s common_dir.
    /// `None` if the hint is missing or the path can't be inferred.
    pub repo_dir: Option<PathBuf>,
}

fn data_home() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/share")
}

/// Pull `owner/repo` out of a git remote URL (https or ssh forms).
fn owner_repo_from_remote(remote: &str) -> Option<String> {
    let tail = remote
        .trim()
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or_else(|| remote.trim().trim_end_matches('/'));
    // ssh: git@host:owner/repo  |  https: https://host/owner/repo
    let path = tail.rsplit_once(':').map(|(_, p)| p).unwrap_or(tail);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() >= 2 {
        Some(format!("{}/{}", segs[segs.len() - 2], segs[segs.len() - 1]))
    } else {
        None
    }
}

impl Store {
    /// Resolve the store for a repo. `REIN_ROOT` overrides; otherwise the key
    /// is the UUID in `git config rein.store` (error if missing → `rein init`).
    pub fn resolve(repo: &Repo) -> Result<Store> {
        if let Ok(root) = std::env::var("REIN_ROOT") {
            if !root.is_empty() {
                return Ok(Store {
                    root: PathBuf::from(root),
                });
            }
        }
        let key = repo
            .config_get("rein.store")
            .ok_or_else(|| anyhow!("repo not initialized for rein — run `rein init`"))?;
        Ok(Store {
            root: data_home().join("rein").join(key),
        })
    }

    /// Compute the store path for init (may not exist yet), generating a key
    /// in git config when absent.
    pub fn resolve_or_create_key(repo: &Repo) -> Result<(Store, String, bool)> {
        if let Ok(root) = std::env::var("REIN_ROOT") {
            if !root.is_empty() {
                let key = repo.config_get("rein.store").unwrap_or_default();
                return Ok((
                    Store {
                        root: PathBuf::from(root),
                    },
                    key,
                    false,
                ));
            }
        }
        match repo.config_get("rein.store") {
            Some(key) => Ok((
                Store {
                    root: data_home().join("rein").join(&key),
                },
                key,
                false,
            )),
            None => {
                let key = uuid::Uuid::new_v4().to_string();
                repo.config_set("rein.store", &key)?;
                Ok((
                    Store {
                        root: data_home().join("rein").join(&key),
                    },
                    key,
                    true,
                ))
            }
        }
    }

    /// Directory holding every per-repo store (`<data home>/rein/`).
    pub fn rein_dir() -> PathBuf {
        data_home().join("rein")
    }

    /// Read this store's project hints from `meta.json` (common_dir, remote)
    /// and derive a display name. Never fails — falls back to the store key.
    pub fn info(&self) -> StoreInfo {
        let (common_dir, remote) = match fs::read_to_string(self.root.join("meta.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            Some(v) => (
                v.get("common_dir")
                    .and_then(|x| x.as_str())
                    .map(PathBuf::from),
                v.get("remote").and_then(|x| x.as_str()).map(str::to_string),
            ),
            None => (None, None),
        };
        // common_dir is the `.git` dir; its parent is the repo working tree.
        let repo_dir = common_dir
            .as_ref()
            .and_then(|c| c.parent().map(PathBuf::from));
        let project = remote
            .as_deref()
            .and_then(owner_repo_from_remote)
            .or_else(|| {
                repo_dir
                    .as_ref()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
            .or_else(|| {
                self.root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "?".to_string());
        StoreInfo {
            store: self.clone(),
            project,
            repo_dir,
        }
    }

    /// Enumerate every store under a given rein directory (testable core).
    pub fn discover_in(rein_dir: &Path) -> Vec<StoreInfo> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(rein_dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            // a store is a directory holding meta.json (skip stray files)
            if p.is_dir() && p.join("meta.json").is_file() {
                out.push(Store { root: p }.info());
            }
        }
        out.sort_by(|a, b| a.project.cmp(&b.project));
        out
    }

    /// Enumerate every per-repo store on this machine for the global dashboard.
    pub fn discover_all() -> Vec<StoreInfo> {
        Self::discover_in(&Self::rein_dir())
    }

    pub fn ensure_layout(&self, repo: &Repo) -> Result<()> {
        for d in ["inbox", "active", "done", "canceled", "conflicts", "state"] {
            fs::create_dir_all(self.root.join(d))?;
        }
        let meta = self.root.join("meta.json");
        if !meta.exists() {
            let value = serde_json::json!({
                "version": 1,
                "common_dir": repo.common_dir.to_string_lossy(),
                "remote": repo.remote_url(),
            });
            util::atomic_write(&meta, &serde_json::to_string_pretty(&value)?)?;
        }
        Ok(())
    }

    pub fn status_dir(&self, status: Status) -> PathBuf {
        self.root.join(status.as_str())
    }

    pub fn conflicts_dir(&self) -> PathBuf {
        self.root.join("conflicts")
    }

    fn read_task(path: &Path, status: Status) -> Option<TaskRef> {
        let content = fs::read_to_string(path).ok()?;
        let doc = TaskDoc::parse(&content).ok()?;
        let slug = path.file_stem()?.to_str()?.to_string();
        Some(TaskRef {
            id: doc.front.id.clone(),
            slug,
            status,
            path: path.to_path_buf(),
            doc,
        })
    }

    fn md_files(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                out.extend(Self::md_files(&p));
            } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(p);
            }
        }
        out.sort();
        out
    }

    pub fn list_tasks(&self) -> Vec<TaskRef> {
        let mut out = Vec::new();
        for status in [
            Status::Inbox,
            Status::Active,
            Status::Done,
            Status::Canceled,
        ] {
            for path in Self::md_files(&self.status_dir(status)) {
                if let Some(t) = Self::read_task(&path, status) {
                    out.push(t);
                }
            }
        }
        out
    }

    pub fn active_count(&self) -> usize {
        Self::md_files(&self.status_dir(Status::Active)).len()
    }

    /// Find a task by exact slug, exact id, or unique prefix of either.
    pub fn find(&self, query: &str) -> Result<TaskRef> {
        let tasks = self.list_tasks();
        if let Some(t) = tasks.iter().find(|t| t.slug == query || t.id == query) {
            return Ok(t.clone());
        }
        let matches: Vec<&TaskRef> = tasks
            .iter()
            .filter(|t| t.slug.starts_with(query) || t.id.starts_with(query))
            .collect();
        match matches.len() {
            0 => bail!("no task matches '{}'", query),
            1 => Ok(matches[0].clone()),
            _ => bail!(
                "ambiguous task '{}': {}",
                query,
                matches
                    .iter()
                    .map(|t| t.slug.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    pub fn find_by_id(&self, id: &str) -> Option<TaskRef> {
        self.list_tasks().into_iter().find(|t| t.id == id)
    }

    pub fn find_by_issue(&self, number: u64) -> Option<TaskRef> {
        self.list_tasks()
            .into_iter()
            .find(|t| t.doc.front.github_issue == Some(number))
    }

    /// A slug not used by any existing task file (suffix -2, -3, ... on clash).
    pub fn unique_slug(&self, base: &str) -> String {
        let taken: Vec<String> = self.list_tasks().into_iter().map(|t| t.slug).collect();
        if !taken.contains(&base.to_string()) {
            return base.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{}-{}", base, n);
            if !taken.contains(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    pub fn write_doc(&self, path: &Path, doc: &TaskDoc) -> Result<()> {
        util::atomic_write(path, &doc.serialize())
    }

    /// Atomic claim/move between status dirs. Errors if source vanished (lost race).
    pub fn move_task(&self, task: &TaskRef, to: Status) -> Result<PathBuf> {
        let dest_dir = if to == Status::Done {
            self.status_dir(to).join(util::month_dir())
        } else {
            self.status_dir(to)
        };
        fs::create_dir_all(&dest_dir)?;
        let dest = dest_dir.join(format!("{}.md", task.slug));
        fs::rename(&task.path, &dest).with_context(|| {
            format!(
                "failed to move task '{}' (already claimed or missing?)",
                task.slug
            )
        })?;
        // directory is truth; keep frontmatter status in sync
        let content = fs::read_to_string(&dest)?;
        if let Ok(mut doc) = TaskDoc::parse(&content) {
            doc.front.status = to.as_str().to_string();
            doc.touch();
            util::atomic_write(&dest, &doc.serialize())?;
        }
        Ok(dest)
    }

    // current pointer (single mode)
    pub fn current_file(&self) -> PathBuf {
        self.root.join("current")
    }

    pub fn read_current(&self) -> Option<String> {
        fs::read_to_string(self.current_file())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn write_current(&self, id: &str) -> Result<()> {
        util::atomic_write(&self.current_file(), &format!("{}\n", id))
    }

    pub fn clear_current(&self) -> Result<()> {
        let f = self.current_file();
        if f.exists() {
            fs::remove_file(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_repo_parses_https_and_ssh() {
        assert_eq!(
            owner_repo_from_remote("https://github.com/acme/web.git").as_deref(),
            Some("acme/web")
        );
        assert_eq!(
            owner_repo_from_remote("git@github.com:acme/web.git").as_deref(),
            Some("acme/web")
        );
        assert_eq!(
            owner_repo_from_remote("https://gitlab.com/group/sub/proj").as_deref(),
            Some("sub/proj")
        );
        assert_eq!(owner_repo_from_remote("not-a-url"), None);
    }

    #[test]
    fn discover_in_enumerates_stores_with_project_names() {
        let tmp = tempfile::tempdir().unwrap();
        let rein = tmp.path().join("rein");

        // store A: remote-derived name
        let a = rein.join("aaaa");
        fs::create_dir_all(&a).unwrap();
        fs::write(
            a.join("meta.json"),
            r#"{"version":1,"common_dir":"/home/me/web/.git","remote":"git@github.com:acme/web.git"}"#,
        )
        .unwrap();

        // store B: no remote → falls back to the repo dir name
        let b = rein.join("bbbb");
        fs::create_dir_all(&b).unwrap();
        fs::write(
            b.join("meta.json"),
            r#"{"version":1,"common_dir":"/home/me/tools/.git","remote":null}"#,
        )
        .unwrap();

        // a stray file (not a store dir) is ignored
        fs::write(rein.join("loose.txt"), "x").unwrap();

        let infos = Store::discover_in(&rein);
        assert_eq!(infos.len(), 2);
        // sorted by project name: "acme/web" < "tools"
        assert_eq!(infos[0].project, "acme/web");
        assert_eq!(
            infos[0].repo_dir.as_deref(),
            Some(Path::new("/home/me/web"))
        );
        assert_eq!(infos[1].project, "tools");
        assert_eq!(
            infos[1].repo_dir.as_deref(),
            Some(Path::new("/home/me/tools"))
        );
    }

    #[test]
    fn discover_in_missing_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Store::discover_in(&tmp.path().join("nope")).is_empty());
    }
}
