use crate::store::{Status, TaskRef};
use crate::Ctx;
use anyhow::{anyhow, bail, Result};

/// Where the task identity came from, in resolution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// #1 explicit --task flag
    Flag,
    /// #2 cwd worktree pointer (.git/worktrees/<n>/rein-task)
    WorktreePointer,
    /// #3 REIN_TASK env
    Env,
    /// #4 store current file (single interactive mode)
    CurrentFile,
}

/// Resolution order: --task flag → worktree pointer → REIN_TASK → current file.
pub fn resolve_task(ctx: &Ctx, flag: Option<&str>) -> Result<(TaskRef, Source)> {
    if let Some(q) = flag {
        return Ok((ctx.store.find(q)?, Source::Flag));
    }
    if let Some(id) = ctx.repo.read_task_pointer() {
        let task = ctx
            .store
            .find_by_id(&id)
            .ok_or_else(|| anyhow!("worktree is bound to missing task '{}'", id))?;
        return Ok((task, Source::WorktreePointer));
    }
    if let Ok(id) = std::env::var("REIN_TASK") {
        if !id.is_empty() {
            let task = ctx
                .store
                .find_by_id(&id)
                .or_else(|| ctx.store.find(&id).ok())
                .ok_or_else(|| anyhow!("REIN_TASK points to missing task '{}'", id))?;
            return Ok((task, Source::Env));
        }
    }
    if let Some(id) = ctx.store.read_current() {
        let task = ctx
            .store
            .find_by_id(&id)
            .ok_or_else(|| anyhow!("current points to missing task '{}' (run `rein doctor`)", id))?;
        return Ok((task, Source::CurrentFile));
    }
    bail!("no task resolved: pass --task <id>, run inside a bound worktree, or `rein use <task>`")
}

/// Mutation-command guard: refuse the current-file fallback when several tasks
/// are active — a worker in the wrong cwd would silently mutate the wrong task.
pub fn gate_mutation(ctx: &Ctx, source: Source) -> Result<()> {
    if source != Source::CurrentFile {
        return Ok(());
    }
    let active: Vec<TaskRef> = ctx
        .store
        .list_tasks()
        .into_iter()
        .filter(|t| t.status == Status::Active)
        .collect();
    if active.len() >= 2 {
        bail!(
            "ambiguous: {} active tasks ({}) — run inside the task's worktree or pass --task <id>",
            active.len(),
            active
                .iter()
                .map(|t| t.slug.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}
