pub mod exec;
pub mod local;
pub mod sync_cmd;

use crate::store::TaskRef;
use crate::{task, Ctx};
use anyhow::{Context, Result};

/// Assign stable integer item IDs to any items lacking one and persist if
/// changed. This is the shared local touchpoint that heals documents edited
/// outside GitHub sync (open after $EDITOR, mutations, doctor).
pub fn assign_ids(ctx: &Ctx, task: &TaskRef) -> Result<TaskRef> {
    let (body, changed) = task::ensure_item_ids(&task.doc.body);
    if !changed {
        return Ok(task.clone());
    }
    let mut doc = task.doc.clone();
    doc.body = body;
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    ctx.store
        .find_by_id(&task.id)
        .context("task vanished while assigning IDs")
}
