use crate::store::Store;
use crate::util;
use anyhow::Result;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;

/// Per-task derived state. One file per task: "task = one owner = one writer",
/// so plain atomic temp+rename writes need no lock on the hot path.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskState {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_synced_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_synced_hash: Option<String>,
    /// Session id of the most recent `rein run`, used to locate its transcript.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_session: Option<String>,
    /// Agent backend that created `run_session` (`claude` or `codex`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_agent: Option<String>,
    /// Log file for backends that rein backgrounds directly (currently Codex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_log: Option<String>,
    /// Exit-code file written by rein's background wrapper.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_status: Option<String>,
}

fn default_version() -> u32 {
    1
}

pub fn state_path(store: &Store, task_id: &str) -> PathBuf {
    store.root.join("state").join(format!("{}.json", task_id))
}

pub fn load(store: &Store, task_id: &str) -> TaskState {
    fs::read_to_string(state_path(store, task_id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(store: &Store, task_id: &str, state: &TaskState) -> Result<()> {
    let mut st = state.clone();
    st.version = 1;
    util::atomic_write(&state_path(store, task_id), &serde_json::to_string_pretty(&st)?)
}

pub fn remove(store: &Store, task_id: &str) -> Result<()> {
    let p = state_path(store, task_id);
    if p.exists() {
        fs::remove_file(p)?;
    }
    Ok(())
}

/// Store-level lock serializing network sync commands (issue/pull/pull-inbox/push).
/// Held for the duration of the command; mutation commands never take it.
pub struct SyncLock {
    file: File,
}

impl SyncLock {
    pub fn acquire(store: &Store) -> Result<SyncLock> {
        fs::create_dir_all(&store.root)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(store.root.join("sync.lock"))?;
        file.lock_exclusive()?;
        Ok(SyncLock { file })
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}
