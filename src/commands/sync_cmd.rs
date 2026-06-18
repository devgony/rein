use crate::gh::Gh;
use crate::resolve;
use crate::state::{self, SyncLock};
use crate::store::{Status, TaskRef};
use crate::sync::{self, SyncPlan};
use crate::task::{self, TaskDoc};
use crate::util;
use crate::Ctx;
use anyhow::{bail, Context, Result};

/// Assign item IDs at the issue/push/pull touchpoints (shared local helper).
fn ensure_ids_saved(ctx: &Ctx, task: &TaskRef) -> Result<TaskRef> {
    crate::commands::assign_ids(&ctx.store, task)
}

/// `rein issue <task> [--project <name>]` — publish a local doc as a new GitHub
/// issue, optionally filing it onto a Project board.
pub fn issue(ctx: &Ctx, query: &str, project: Option<&str>) -> Result<()> {
    let _lock = SyncLock::acquire(&ctx.store)?;
    let task = ctx.store.find(query)?;
    if let Some(n) = task.doc.front.github_issue {
        bail!("'{}' is already issue #{} — use `rein push`", task.slug, n);
    }
    let task = ensure_ids_saved(ctx, &task)?;
    let block = task::issue_projection(&task.doc);

    let gh = Gh::in_dir(&ctx.repo.workdir);
    gh.ensure_label();
    let number = gh.issue_create(&task.doc.front.title, &block, project)?;

    let mut doc = task.doc.clone();
    doc.front.github_issue = Some(number);
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;

    let mut st = state::load(&ctx.store, &task.id);
    st.issue_synced_hash = Some(sync::hash_block(&block));
    state::save(&ctx.store, &task.id, &st)?;
    println!("issue #{} created for {}", number, task.id);
    Ok(())
}

/// `rein pull-inbox` — import/refresh all rein-labeled issues. Idempotent:
/// the marker task ID (or the issue number) is the identity.
pub fn pull_inbox(ctx: &Ctx) -> Result<()> {
    let _lock = SyncLock::acquire(&ctx.store)?;
    let gh = Gh::in_dir(&ctx.repo.workdir);
    let issues = gh.issue_list_rein()?;
    let mut imported = 0;
    let mut updated = 0;
    for ri in issues {
        let marker_id = task::extract_managed(&ri.body).map(|(id, _, _)| id);
        // identity: marker ID first, then issue number
        let existing = marker_id
            .as_ref()
            .and_then(|id| ctx.store.find_by_id(id))
            .or_else(|| ctx.store.find_by_issue(ri.number));
        match existing {
            Some(local) => {
                if pull_into(ctx, &local, &ri.body, false)? {
                    updated += 1;
                }
            }
            None => {
                import_new(ctx, &ri, marker_id)?;
                imported += 1;
            }
        }
    }
    println!("pull-inbox: {} imported, {} updated", imported, updated);
    Ok(())
}

fn import_new(ctx: &Ctx, ri: &crate::gh::RemoteIssue, marker_id: Option<String>) -> Result<()> {
    let slug = ctx.store.unique_slug(&util::slugify(&ri.title));
    // marker carries the ID; only human-created issues get a fresh one
    let id = marker_id.unwrap_or_else(|| format!("task-{}-{}", util::today_compact(), slug));
    let now = util::now_iso();
    let inner = task::extract_managed(&ri.body)
        .map(|(_, _, inner)| inner)
        .unwrap_or_else(|| ri.body.trim().to_string());
    let mut doc = TaskDoc::template(&id, &ri.title, "inbox", &now, true);
    if !inner.is_empty() {
        doc.body = task::body_from_remote(&inner, &doc.body);
    }
    doc.front.github_issue = Some(ri.number);
    let path = ctx.store.status_dir(Status::Inbox).join(format!("{}.md", slug));
    ctx.store.write_doc(&path, &doc)?;

    let block = task::issue_projection(&doc);
    let mut st = state::load(&ctx.store, &id);
    st.path = format!("inbox/{}.md", slug);
    // remote is the base we just adopted
    st.issue_synced_hash = task::extract_managed(&ri.body)
        .map(|(_, b, _)| sync::hash_block(&b))
        .or(Some(sync::hash_block(&block)));
    state::save(&ctx.store, &id, &st)?;
    println!("imported #{} as {}", ri.number, slug);
    Ok(())
}

/// Apply remote → local for one task using the 3-way table.
/// Returns true if the local doc changed. `strict` errors on remote-missing.
fn pull_into(ctx: &Ctx, task: &TaskRef, remote_body: &str, strict: bool) -> Result<bool> {
    sync::check_ownership(remote_body, &task.id)?;
    let task = ensure_ids_saved(ctx, task)?;
    let local_block = task::issue_projection(&task.doc);
    let local_hash = sync::hash_block(&local_block);
    let remote_block = sync::remote_block(remote_body);
    if remote_block.is_none() && strict {
        bail!("issue body has no managed section for '{}'", task.slug);
    }
    let remote_hash = remote_block.as_deref().map(sync::hash_block);
    let st = state::load(&ctx.store, &task.id);

    match sync::plan(st.issue_synced_hash.as_deref(), &local_hash, remote_hash.as_deref()) {
        SyncPlan::UpToDate | SyncPlan::Push => Ok(false), // pull never writes remote
        SyncPlan::Pull => {
            let (_, _, inner) =
                task::extract_managed(remote_body).context("managed section disappeared")?;
            let mut doc = task.doc.clone();
            doc.body = task::body_from_remote(&inner, &doc.body);
            doc.touch();
            ctx.store.write_doc(&task.path, &doc)?;
            let mut st = st;
            st.issue_synced_hash = remote_hash;
            state::save(&ctx.store, &task.id, &st)?;
            sync::clear_conflict(&ctx.store, &task);
            Ok(true)
        }
        SyncPlan::Conflict => {
            sync::write_conflict(
                &ctx.store,
                &task,
                &local_block,
                remote_block.as_deref().unwrap_or(""),
            )?;
            Err(sync::conflict_error(&task, "issue"))
        }
    }
}

/// `rein pull` — pull the resolved task's issue.
pub fn pull(ctx: &Ctx) -> Result<()> {
    let _lock = SyncLock::acquire(&ctx.store)?;
    let (task, _) = resolve::resolve_task(ctx, None)?;
    let number = task
        .doc
        .front
        .github_issue
        .with_context(|| format!("'{}' has no attached issue", task.slug))?;
    let gh = Gh::in_dir(&ctx.repo.workdir);
    let remote_body = gh.issue_view_body(number)?;
    let changed = pull_into(ctx, &task, &remote_body, true)?;
    println!(
        "pull: {}",
        if changed { "updated from remote" } else { "up to date" }
    );
    Ok(())
}

/// `rein push [--resolved]` — push the resolved task to its issue and/or PR.
pub fn push(ctx: &Ctx, resolved: bool) -> Result<()> {
    let (task, _) = resolve::resolve_task(ctx, None)?;
    push_task(ctx, &task, resolved)
}

/// Push a specific task (used by the TUI `p` binding).
pub fn push_task(ctx: &Ctx, task: &TaskRef, resolved: bool) -> Result<()> {
    let _lock = SyncLock::acquire(&ctx.store)?;
    let task = task.clone();
    let front = &task.doc.front;
    if front.github_issue.is_none() && front.github_pr.is_none() {
        bail!("'{}' has neither issue nor PR attached", task.slug);
    }
    let task = ensure_ids_saved(ctx, &task)?;
    let gh = Gh::in_dir(&ctx.repo.workdir);

    if let Some(number) = task.doc.front.github_issue {
        push_surface(ctx, &task, &gh, Surface::Issue(number), resolved)?;
    }
    if let Some(number) = task.doc.front.github_pr {
        push_surface(ctx, &task, &gh, Surface::Pr(number), resolved)?;
    }
    Ok(())
}

/// Push the managed section to the task's issue only (TUI `i` on a task that
/// already has an issue). Mirrors `push_task` but targets a single surface so
/// `i` and `r` publish to their own surfaces independently.
pub fn push_issue(ctx: &Ctx, task: &TaskRef) -> Result<()> {
    let _lock = SyncLock::acquire(&ctx.store)?;
    let task = ensure_ids_saved(ctx, task)?;
    let number = task
        .doc
        .front
        .github_issue
        .with_context(|| format!("'{}' has no attached issue", task.slug))?;
    let gh = Gh::in_dir(&ctx.repo.workdir);
    push_surface(ctx, &task, &gh, Surface::Issue(number), false)
}

/// Push the managed section to the task's PR only (TUI `r` on a task that
/// already has a PR).
pub fn push_pr(ctx: &Ctx, task: &TaskRef) -> Result<()> {
    let _lock = SyncLock::acquire(&ctx.store)?;
    let task = ensure_ids_saved(ctx, task)?;
    let number = task
        .doc
        .front
        .github_pr
        .with_context(|| format!("'{}' has no attached PR", task.slug))?;
    let gh = Gh::in_dir(&ctx.repo.workdir);
    push_surface(ctx, &task, &gh, Surface::Pr(number), false)
}

enum Surface {
    Issue(u64),
    Pr(u64),
}

fn push_surface(ctx: &Ctx, task: &TaskRef, gh: &Gh, surface: Surface, resolved: bool) -> Result<()> {
    let (name, number, local_block, base) = match &surface {
        Surface::Issue(n) => (
            "issue",
            *n,
            task::issue_projection(&task.doc),
            state::load(&ctx.store, &task.id).issue_synced_hash,
        ),
        Surface::Pr(n) => (
            "PR",
            *n,
            task::pr_projection(&task.doc),
            state::load(&ctx.store, &task.id).pr_synced_hash,
        ),
    };
    let remote_body = match &surface {
        Surface::Issue(n) => gh.issue_view_body(*n)?,
        Surface::Pr(n) => gh.pr_view_body(*n)?,
    };
    sync::check_ownership(&remote_body, &task.id)?;
    let local_hash = sync::hash_block(&local_block);
    let remote_block = sync::remote_block(&remote_body);
    let remote_hash = remote_block.as_deref().map(sync::hash_block);

    let plan = if resolved {
        SyncPlan::Push
    } else {
        sync::plan(base.as_deref(), &local_hash, remote_hash.as_deref())
    };
    match plan {
        SyncPlan::UpToDate => {
            println!("{} #{}: up to date", name, number);
            Ok(())
        }
        SyncPlan::Pull => {
            bail!(
                "{} #{} changed remotely and local is unchanged — run `rein pull` first",
                name,
                number
            )
        }
        SyncPlan::Push => {
            let new_body = task::replace_managed(&remote_body, &local_block);
            match &surface {
                Surface::Issue(n) => gh.issue_edit_body(*n, &new_body)?,
                Surface::Pr(n) => gh.pr_edit_body(*n, &new_body)?,
            }
            let mut st = state::load(&ctx.store, &task.id);
            match &surface {
                Surface::Issue(_) => st.issue_synced_hash = Some(local_hash),
                Surface::Pr(_) => st.pr_synced_hash = Some(local_hash),
            }
            state::save(&ctx.store, &task.id, &st)?;
            sync::clear_conflict(&ctx.store, task);
            println!("{} #{}: pushed", name, number);
            Ok(())
        }
        SyncPlan::Conflict => {
            sync::write_conflict(
                &ctx.store,
                task,
                &local_block,
                remote_block.as_deref().unwrap_or(""),
            )?;
            Err(sync::conflict_error(task, name))
        }
    }
}

pub fn attach_issue(ctx: &Ctx, number: u64) -> Result<()> {
    let (task, _) = resolve::resolve_task(ctx, None)?;
    if let Some(existing) = ctx.store.find_by_issue(number) {
        if existing.id != task.id {
            bail!("issue #{} is already attached to '{}'", number, existing.slug);
        }
    }
    let mut doc = task.doc.clone();
    doc.front.github_issue = Some(number);
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!(
        "attached issue #{} to {} — run `rein pull` to adopt remote or `rein push --resolved` to overwrite",
        number, task.slug
    );
    Ok(())
}

pub fn attach_pr(ctx: &Ctx, number: u64) -> Result<()> {
    let (task, _) = resolve::resolve_task(ctx, None)?;
    let mut doc = task.doc.clone();
    doc.front.github_pr = Some(number);
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!("attached PR #{} to {} — run `rein push` to publish the managed section", number, task.slug);
    Ok(())
}
