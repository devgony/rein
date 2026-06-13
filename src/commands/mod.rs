pub mod exec;
pub mod local;
pub mod sync_cmd;

use crate::store::{Store, TaskRef};
use crate::task;
use anyhow::{Context, Result};

/// Assign stable integer item IDs to any items lacking one and persist if
/// changed. This is the shared local touchpoint that heals documents edited
/// outside GitHub sync (open after $EDITOR, mutations, doctor). Operates on the
/// store alone — no repo needed — so the cross-project dashboard can heal any
/// task it touches.
pub fn assign_ids(store: &Store, task: &TaskRef) -> Result<TaskRef> {
    let (body, changed) = task::ensure_item_ids(&task.doc.body);
    if !changed {
        return Ok(task.clone());
    }
    let mut doc = task.doc.clone();
    doc.body = body;
    doc.touch();
    store.write_doc(&task.path, &doc)?;
    store
        .find_by_id(&task.id)
        .context("task vanished while assigning IDs")
}
