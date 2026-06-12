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

fn data_home() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/share")
}

impl Store {
    /// Resolve the store for a repo. `REIN_ROOT` overrides; otherwise the key
    /// is the UUID in `git config rein.store` (error if missing → `rein init`).
    pub fn resolve(repo: &Repo) -> Result<Store> {
        if let Ok(root) = std::env::var("REIN_ROOT") {
            if !root.is_empty() {
                return Ok(Store { root: PathBuf::from(root) });
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
                return Ok((Store { root: PathBuf::from(root) }, key, false));
            }
        }
        match repo.config_get("rein.store") {
            Some(key) => Ok((
                Store { root: data_home().join("rein").join(&key) },
                key,
                false,
            )),
            None => {
                let key = uuid::Uuid::new_v4().to_string();
                repo.config_set("rein.store", &key)?;
                Ok((
                    Store { root: data_home().join("rein").join(&key) },
                    key,
                    true,
                ))
            }
        }
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
        for status in [Status::Inbox, Status::Active, Status::Done, Status::Canceled] {
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
