use crate::gh::Gh;
use crate::gitx::{self, Repo};
use crate::resolve;
use crate::state;
use crate::store::{Status, TaskRef};
use crate::task;
use crate::util;
use crate::Ctx;
use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;

/// inbox→active claim + optional worktree/branch/draft-PR.
pub fn start(
    ctx: &Ctx,
    query: &str,
    worktree: bool,
    branch: Option<&str>,
    draft_pr: bool,
) -> Result<()> {
    let task = ctx.store.find(query)?;
    match task.status {
        Status::Inbox => {}
        Status::Active => bail!("'{}' is already active", task.slug),
        _ => bail!("'{}' is {} — only inbox tasks can be started", task.slug, task.status.as_str()),
    }
    // atomic rename = claim; loser of a race errors here
    let new_path = ctx.store.move_task(&task, Status::Active)?;
    let task = ctx
        .store
        .find_by_id(&task.id)
        .context("task vanished after claim")?;

    let mut st = state::load(&ctx.store, &task.id);
    st.path = new_path
        .strip_prefix(&ctx.store.root)
        .unwrap_or(&new_path)
        .to_string_lossy()
        .to_string();

    let branch_name = branch
        .map(|b| b.to_string())
        .unwrap_or_else(|| format!("rein/{}", task.slug));

    if worktree {
        let wt_path = worktree_path(&ctx.repo, &task.slug);
        ctx.repo
            .worktree_add(&wt_path, &branch_name)
            .context("git worktree add failed")?;
        // bind task to the new worktree via its git-dir pointer
        let wt_repo = Repo::discover(&wt_path)?;
        wt_repo.write_task_pointer(&task.id)?;
        st.branch = Some(branch_name.clone());
        st.worktree = Some(wt_path.to_string_lossy().to_string());
        set_branch_frontmatter(ctx, &task, &branch_name)?;
        println!("worktree: {}", wt_path.display());
    } else {
        if branch.is_some() {
            ctx.repo.branch_create_and_switch(&branch_name)?;
            st.branch = Some(branch_name.clone());
            set_branch_frontmatter(ctx, &task, &branch_name)?;
        }
        // single mode: current pointer
        ctx.store.write_current(&task.id)?;
    }

    if draft_pr {
        if !worktree && branch.is_none() {
            bail!("--draft-pr requires --worktree or --branch");
        }
        let gh = Gh::new();
        let task = ctx.store.find_by_id(&task.id).context("task missing")?;
        let body = task::pr_projection(&task.doc);
        let number = gh.pr_create_draft(&task.doc.front.title, &body, &branch_name)?;
        let mut doc = task.doc.clone();
        doc.front.github_pr = Some(number);
        doc.touch();
        ctx.store.write_doc(&task.path, &doc)?;
        st.pr_synced_hash = Some(crate::sync::hash_block(&body));
        println!("draft PR: #{}", number);
        if let Some(issue) = doc.front.github_issue {
            let _ = gh.issue_comment(issue, &format!("Started in PR #{}", number));
        }
    } else if let Some(issue) = task.doc.front.github_issue {
        let gh = Gh::new();
        let _ = gh.issue_comment(issue, &format!("Started on branch `{}`", branch_name));
    }

    state::save(&ctx.store, &task.id, &st)?;
    println!("started {}", task.id);
    Ok(())
}

fn worktree_path(repo: &Repo, slug: &str) -> PathBuf {
    let name = repo
        .workdir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    let parent = repo
        .workdir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| repo.workdir.clone());
    parent.join(format!("{}-wt", name)).join(slug)
}

fn set_branch_frontmatter(ctx: &Ctx, task: &TaskRef, branch: &str) -> Result<()> {
    let task = ctx.store.find_by_id(&task.id).context("task missing")?;
    let mut doc = task.doc.clone();
    doc.front.branch = Some(branch.to_string());
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)
}

// ---------------------------------------------------------------------------
// LLM-safe mutations
// ---------------------------------------------------------------------------

fn resolve_for_mutation(ctx: &Ctx, flag: Option<&str>) -> Result<TaskRef> {
    let (task, source) = resolve::resolve_task(ctx, flag)?;
    resolve::gate_mutation(ctx, source)?;
    // local touchpoint: assign item IDs so checking works without GitHub sync
    crate::commands::assign_ids(ctx, &task)
}

/// Turn a bare "item not found" into actionable guidance.
fn item_error_hint(ctx: &Ctx, task: &TaskRef, item_id: &str, base: anyhow::Error) -> anyhow::Error {
    if ctx.store.find(item_id).is_ok() {
        return anyhow!(
            "'{}' is a task, not an item — pass an item number (see `rein status`), \
             e.g. `rein check 1 --task {}`",
            item_id,
            item_id
        );
    }
    let avail: Vec<String> = task::scan_items(&task.doc.body)
        .iter()
        .filter_map(|i| i.id.clone())
        .collect();
    if avail.is_empty() {
        anyhow!("{} — '{}' has no checklist items", base, task.slug)
    } else {
        anyhow!("{}. available items in {}: {}", base, task.slug, avail.join(", "))
    }
}

pub fn check(ctx: &Ctx, item_id: &str, flag: Option<&str>, checked: bool) -> Result<()> {
    let task = resolve_for_mutation(ctx, flag)?;
    let mut doc = task.doc.clone();
    doc.body = match task::set_checked(&doc.body, item_id, checked) {
        Ok(body) => body,
        Err(e) => return Err(item_error_hint(ctx, &task, item_id, e)),
    };
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!(
        "{} {} in {}",
        if checked { "checked" } else { "unchecked" },
        item_id,
        task.slug
    );
    Ok(())
}

pub fn log(ctx: &Ctx, text: &str, flag: Option<&str>) -> Result<()> {
    let task = resolve_for_mutation(ctx, flag)?;
    let mut doc = task.doc.clone();
    doc.body = task::append_log(&doc.body, &format!("{} {}", util::now_iso(), text));
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!("logged to {}", task.slug);
    Ok(())
}

pub fn fail(ctx: &Ctx, item_id: &str, reason: &str, flag: Option<&str>) -> Result<()> {
    let task = resolve_for_mutation(ctx, flag)?;
    // validate the item exists; the box stays unchecked, the blocker goes to the log
    if !task::scan_items(&task.doc.body)
        .iter()
        .any(|i| i.id.as_deref() == Some(item_id))
    {
        return Err(item_error_hint(ctx, &task, item_id, anyhow!("item '{}' not found", item_id)));
    }
    let mut doc = task.doc.clone();
    doc.body = task::append_log(
        &doc.body,
        &format!("{} FAIL {}: {}", util::now_iso(), item_id, reason),
    );
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!("recorded blocker on {} in {}", item_id, task.slug);
    Ok(())
}

// ---------------------------------------------------------------------------
// done / cancel
// ---------------------------------------------------------------------------

struct WorktreeInfo {
    path: PathBuf,
    dirty: bool,
}

fn worktree_info(ctx: &Ctx, task_id: &str) -> Result<Option<WorktreeInfo>> {
    let st = state::load(&ctx.store, task_id);
    let Some(wt) = st.worktree else {
        return Ok(None);
    };
    let path = PathBuf::from(wt);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(WorktreeInfo {
        dirty: gitx::is_dirty(&path)?,
        path,
    }))
}

pub fn done(ctx: &Ctx, query: Option<&str>, keep_worktree: bool) -> Result<()> {
    let task = match query {
        Some(q) => ctx.store.find(q)?,
        None => resolve::resolve_task(ctx, None)?.0,
    };
    if matches!(task.status, Status::Done | Status::Canceled) {
        bail!("'{}' is already {}", task.slug, task.status.as_str());
    }
    // pre-flight before any side effect: no half-done states
    let wt = worktree_info(ctx, &task.id)?;
    if let Some(info) = &wt {
        if !keep_worktree && info.dirty {
            bail!(
                "worktree {} has uncommitted changes — commit/stash and retry, or use --keep-worktree",
                info.path.display()
            );
        }
    }

    ctx.store.move_task(&task, Status::Done)?;

    let front = &task.doc.front;
    let gh = Gh::new();
    if let Some(pr) = front.github_pr {
        if let Err(e) = final_pr_update(ctx, &task, pr, &gh) {
            eprintln!("warning: failed to update PR #{}: {}", pr, e);
        }
    }
    if let Some(issue) = front.github_issue {
        if let Err(e) = gh.issue_close(issue, false, "Completed via rein.") {
            eprintln!("warning: failed to close issue #{}: {}", issue, e);
        }
    }

    if let Some(info) = wt {
        if !keep_worktree {
            ctx.repo.worktree_remove(&info.path, false)?;
            println!("removed worktree {}", info.path.display());
        }
    }

    if ctx.store.read_current().as_deref() == Some(task.id.as_str()) {
        ctx.store.clear_current()?;
    }
    let mut st = state::load(&ctx.store, &task.id);
    if !keep_worktree {
        st.worktree = None;
    }
    if let Some(t) = ctx.store.find_by_id(&task.id) {
        st.path = t
            .path
            .strip_prefix(&ctx.store.root)
            .unwrap_or(&t.path)
            .to_string_lossy()
            .to_string();
    }
    state::save(&ctx.store, &task.id, &st)?;
    println!("done {}", task.id);
    Ok(())
}

fn final_pr_update(ctx: &Ctx, task: &TaskRef, pr: u64, gh: &Gh) -> Result<()> {
    let doc = ctx
        .store
        .find_by_id(&task.id)
        .map(|t| t.doc)
        .unwrap_or_else(|| task.doc.clone());
    let block = task::pr_projection(&doc);
    let remote = gh.pr_view_body(pr)?;
    crate::sync::check_ownership(&remote, &task.id)?;
    gh.pr_edit_body(pr, &task::replace_managed(&remote, &block))?;
    let mut st = state::load(&ctx.store, &task.id);
    st.pr_synced_hash = Some(crate::sync::hash_block(&block));
    state::save(&ctx.store, &task.id, &st)
}

pub fn cancel(ctx: &Ctx, query: Option<&str>, keep_worktree: bool, force: bool) -> Result<()> {
    let task = match query {
        Some(q) => ctx.store.find(q)?,
        None => resolve::resolve_task(ctx, None)?.0,
    };
    if matches!(task.status, Status::Done | Status::Canceled) {
        bail!("'{}' is already {}", task.slug, task.status.as_str());
    }
    let wt = worktree_info(ctx, &task.id)?;
    if let Some(info) = &wt {
        if !keep_worktree && info.dirty && !force {
            bail!(
                "worktree {} has uncommitted changes — use --force to discard, or --keep-worktree",
                info.path.display()
            );
        }
    }

    ctx.store.move_task(&task, Status::Canceled)?;

    if let Some(issue) = task.doc.front.github_issue {
        let gh = Gh::new();
        if let Err(e) = gh.issue_close(issue, true, "Canceled via rein.") {
            eprintln!("warning: failed to close issue #{}: {}", issue, e);
        }
    }

    if let Some(info) = wt {
        if !keep_worktree {
            ctx.repo.worktree_remove(&info.path, force)?;
            println!("removed worktree {}", info.path.display());
        }
    }
    if ctx.store.read_current().as_deref() == Some(task.id.as_str()) {
        ctx.store.clear_current()?;
    }
    let mut st = state::load(&ctx.store, &task.id);
    if !keep_worktree {
        st.worktree = None;
    }
    if let Some(t) = ctx.store.find_by_id(&task.id) {
        st.path = t
            .path
            .strip_prefix(&ctx.store.root)
            .unwrap_or(&t.path)
            .to_string_lossy()
            .to_string();
    }
    state::save(&ctx.store, &task.id, &st)?;
    println!("canceled {}", task.id);
    Ok(())
}
