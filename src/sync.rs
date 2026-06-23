use crate::store::{Store, TaskRef};
use crate::task;
use crate::util;
use anyhow::{bail, Result};
use std::fs;

/// 3-way comparison outcome for one sync surface (issue or PR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPlan {
    UpToDate,
    Push,
    Pull,
    Conflict,
}

/// base: synced hash at last successful push/pull. local/remote: current hashes
/// of the managed block. `issue.updated_at` is deliberately not used.
pub fn plan(base: Option<&str>, local_hash: &str, remote_hash: Option<&str>) -> SyncPlan {
    let local_changed = base != Some(local_hash);
    let remote_changed = match remote_hash {
        // no remote managed section yet → treat as remote unchanged (first push)
        None => false,
        Some(r) => base != Some(r),
    };
    match (local_changed, remote_changed) {
        (false, false) => SyncPlan::UpToDate,
        (true, false) => SyncPlan::Push,
        (false, true) => SyncPlan::Pull,
        (true, true) => SyncPlan::Conflict,
    }
}

pub fn hash_block(block: &str) -> String {
    util::sha256_tag(block.trim())
}

pub fn conflict_paths(store: &Store, task: &TaskRef) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = store.conflicts_dir();
    (
        dir.join(format!("{}.local.md", task.slug)),
        dir.join(format!("{}.remote.md", task.slug)),
    )
}

pub fn write_conflict(store: &Store, task: &TaskRef, local_block: &str, remote_block: &str) -> Result<()> {
    let (lp, rp) = conflict_paths(store, task);
    fs::create_dir_all(store.conflicts_dir())?;
    util::atomic_write(&lp, &format!("{}\n", local_block.trim()))?;
    util::atomic_write(&rp, &format!("{}\n", remote_block.trim()))?;
    Ok(())
}

pub fn clear_conflict(store: &Store, task: &TaskRef) {
    let (lp, rp) = conflict_paths(store, task);
    let _ = fs::remove_file(lp);
    let _ = fs::remove_file(rp);
}

/// Extract the managed block from a remote body, if any.
pub fn remote_block(remote_body: &str) -> Option<String> {
    task::extract_managed(remote_body).map(|(_, block, _)| block)
}

/// A sync conflict (local and remote both changed since the last sync). A typed
/// error so callers — notably the TUI — can detect it (`downcast_ref`) and offer
/// a force-push, rather than string-matching the message. Its `Display` is the
/// same guidance the CLI has always printed, so behavior is unchanged.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub surface: String,
    pub slug: String,
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "conflict on {} for '{}': local and remote both changed since last sync.\n\
             Backups written to conflicts/. Resolve the local document, then run \
             `rein push --resolved` (TUI: press `f` to force-push).",
            self.surface, self.slug
        )
    }
}

impl std::error::Error for Conflict {}

pub fn conflict_error(task: &TaskRef, surface: &str) -> anyhow::Error {
    anyhow::Error::new(Conflict {
        surface: surface.to_string(),
        slug: task.slug.clone(),
    })
}

/// Guard: a managed block in the remote body must belong to this task.
pub fn check_ownership(remote_body: &str, task_id: &str) -> Result<()> {
    if let Some((id, _, _)) = task::extract_managed(remote_body) {
        if id != task_id {
            bail!(
                "remote managed section belongs to '{}', not '{}' — refusing to overwrite",
                id,
                task_id
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_way_table() {
        let base = "sha256:aaa";
        assert_eq!(plan(Some(base), base, Some(base)), SyncPlan::UpToDate);
        assert_eq!(plan(Some(base), "sha256:bbb", Some(base)), SyncPlan::Push);
        assert_eq!(plan(Some(base), base, Some("sha256:ccc")), SyncPlan::Pull);
        assert_eq!(
            plan(Some(base), "sha256:bbb", Some("sha256:ccc")),
            SyncPlan::Conflict
        );
        // missing base (lost state) with both sides present → conflict fallback
        assert_eq!(
            plan(None, "sha256:bbb", Some("sha256:ccc")),
            SyncPlan::Conflict
        );
        // no remote section yet → first push
        assert_eq!(plan(None, "sha256:bbb", None), SyncPlan::Push);
    }
}
