use crate::gh::Gh;
use crate::gitx::{self, Repo};
use crate::resolve;
use crate::state;
use crate::store::{Status, Store, TaskRef};
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
        setup_worktree(ctx, &task, &branch_name, &mut st)?;
    } else {
        if branch.is_some() {
            setup_branch(ctx, &task, &branch_name, &mut st)?;
        }
        // single mode: current pointer
        ctx.store.write_current(&task.id)?;
    }

    if draft_pr {
        if !worktree && branch.is_none() {
            bail!("--draft-pr requires --worktree or --branch");
        }
        let fresh = ctx.store.find_by_id(&task.id).context("task missing")?;
        let number = create_draft_pr(ctx, &fresh, &branch_name, &mut st)?;
        println!("draft PR: #{}", number);
    } else if let Some(issue) = task.doc.front.github_issue {
        let gh = Gh::in_dir(&ctx.repo.workdir);
        let _ = gh.issue_comment(issue, &format!("Started on branch `{}`", branch_name));
    }

    state::save(&ctx.store, &task.id, &st)?;
    println!("started {}", task.id);
    Ok(())
}

/// Open a draft PR for a task, from the dashboard or `rein pr`. Inbox tasks are
/// claimed and set up exactly like `start` (worktree or main-repo branch per
/// `worktree`); active tasks reuse their bound branch, or get one set up if they
/// have none. Refuses if a PR is already attached.
pub fn create_pr(ctx: &Ctx, query: Option<&str>, worktree: bool) -> Result<()> {
    let task = match query {
        Some(q) => ctx.store.find(q)?,
        None => resolve::resolve_task(ctx, None)?.0,
    };
    if let Some(pr) = task.doc.front.github_pr {
        bail!("'{}' already has PR #{}", task.slug, pr);
    }
    // inbox → delegate to start, which claims + sets up + opens the PR in one go
    if task.status == Status::Inbox {
        let branch = (!worktree).then(|| format!("rein/{}", task.slug));
        return start(ctx, &task.slug, worktree, branch.as_deref(), true);
    }
    if task.status != Status::Active {
        bail!(
            "'{}' is {} — only inbox/active tasks can open a PR",
            task.slug,
            task.status.as_str()
        );
    }
    // active: reuse the bound branch, else set one up per the chosen mode
    let mut st = state::load(&ctx.store, &task.id);
    let branch_name = task
        .doc
        .front
        .branch
        .clone()
        .or_else(|| st.branch.clone())
        .unwrap_or_else(|| format!("rein/{}", task.slug));
    if st.branch.is_none() {
        if worktree {
            setup_worktree(ctx, &task, &branch_name, &mut st)?;
        } else {
            setup_branch(ctx, &task, &branch_name, &mut st)?;
        }
    }
    let fresh = ctx.store.find_by_id(&task.id).context("task missing")?;
    let number = create_draft_pr(ctx, &fresh, &branch_name, &mut st)?;
    state::save(&ctx.store, &task.id, &st)?;
    println!("draft PR: #{}", number);
    Ok(())
}

/// Worktrees live under the rein store (`<store>/worktrees/<slug>`) rather than
/// beside the repo, so the project's parent dir stays clean and cleanup happens
/// from a path the engine owns. `done`/`cancel` remove via the stored
/// `st.worktree`, so this location is free to change without touching them.
fn worktree_path(store: &Store, slug: &str) -> PathBuf {
    store.root.join("worktrees").join(slug)
}

/// Add a worktree for `task` on `branch_name`, bind the task↔worktree pointer,
/// and record the branch/worktree in `st`. Shared by `start` and `create_pr`.
fn setup_worktree(
    ctx: &Ctx,
    task: &TaskRef,
    branch_name: &str,
    st: &mut state::TaskState,
) -> Result<()> {
    ensure_branch_free(ctx, branch_name)?;
    let wt_path = worktree_path(&ctx.store, &task.slug);
    ctx.repo
        .worktree_add(&wt_path, branch_name)
        .context("git worktree add failed")?;
    // bind task to the new worktree via its git-dir pointer
    let wt_repo = Repo::discover(&wt_path)?;
    wt_repo.write_task_pointer(&task.id)?;
    st.branch = Some(branch_name.to_string());
    st.worktree = Some(wt_path.to_string_lossy().to_string());
    set_branch_frontmatter(ctx, task, branch_name)?;
    println!("worktree: {}", wt_path.display());
    Ok(())
}

/// Create `branch_name` in the main repo, switch to it, and record it in `st`.
/// Shared by `start` and `create_pr` (single/branch mode).
fn setup_branch(
    ctx: &Ctx,
    task: &TaskRef,
    branch_name: &str,
    st: &mut state::TaskState,
) -> Result<()> {
    ensure_branch_free(ctx, branch_name)?;
    ctx.repo.branch_create_and_switch(branch_name)?;
    st.branch = Some(branch_name.to_string());
    set_branch_frontmatter(ctx, task, branch_name)?;
    Ok(())
}

/// Turn the predictable "branch already exists" collision (usually a leftover
/// from an earlier run for the same task) into an actionable message, before
/// `git worktree add -b` / `git switch -c` fails with a bare git fatal.
fn ensure_branch_free(ctx: &Ctx, branch_name: &str) -> Result<()> {
    if ctx.repo.branch_exists(branch_name) {
        bail!(
            "branch '{branch_name}' already exists — likely left by an earlier run. \
             Delete it with `git branch -D {branch_name}` (or finish/cancel that task), then retry."
        );
    }
    Ok(())
}

/// Create the draft PR on `branch_name`, record `github_pr` in the doc, seed the
/// PR sync hash in `st`, and ping the linked issue. Shared by `start`/`create_pr`.
fn create_draft_pr(
    ctx: &Ctx,
    task: &TaskRef,
    branch_name: &str,
    st: &mut state::TaskState,
) -> Result<u64> {
    // GitHub rejects a PR with no diff; warn instead of letting `gh` fail cryptically
    if let Some(base) = ctx.repo.default_branch() {
        if base != branch_name && ctx.repo.commits_ahead(&base, branch_name)? == 0 {
            bail!(
                "no commits on '{branch_name}' yet — nothing to open a PR against '{base}'. \
                 Commit your work first, then run `rein pr` again."
            );
        }
    }
    // push the branch so `gh pr create --head` can find it on the remote
    ctx.repo.push_branch(branch_name)?;
    let gh = Gh::in_dir(&ctx.repo.workdir);
    let body = task::pr_projection(&task.doc);
    let number = gh.pr_create_draft(&task.doc.front.title, &body, branch_name)?;
    let mut doc = task.doc.clone();
    doc.front.github_pr = Some(number);
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    st.pr_synced_hash = Some(crate::sync::hash_block(&body));
    if let Some(issue) = doc.front.github_issue {
        let _ = gh.issue_comment(issue, &format!("Started in PR #{}", number));
    }
    Ok(number)
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
    crate::commands::assign_ids(&ctx.store, &task)
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
    let mut doc = task.doc.clone();
    // resolve the item as failed: checked box (terminal, drops out of `todo`)
    // plus a visible ~~strike~~ ❌; this also validates the item exists.
    doc.body = match task::set_failed(&doc.body, item_id) {
        Ok(body) => body,
        Err(e) => return Err(item_error_hint(ctx, &task, item_id, e)),
    };
    // the blocker reason still lands in the (local, non-projected) Agent Log
    doc.body = task::append_log(
        &doc.body,
        &format!("{} FAIL {}: {}", util::now_iso(), item_id, reason),
    );
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!(
        "failed {} in {} (resolved as not-done; `rein retry {}` to reopen)",
        item_id, task.slug, item_id
    );
    Ok(())
}

/// Reopen a failed item — the inverse of `fail`.
pub fn retry(ctx: &Ctx, item_id: &str, flag: Option<&str>) -> Result<()> {
    let task = resolve_for_mutation(ctx, flag)?;
    let mut doc = task.doc.clone();
    doc.body = match task::clear_failed(&doc.body, item_id) {
        Ok(body) => body,
        Err(e) => return Err(item_error_hint(ctx, &task, item_id, e)),
    };
    doc.body = task::append_log(&doc.body, &format!("{} RETRY {}", util::now_iso(), item_id));
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!("reopened {} in {}", item_id, task.slug);
    Ok(())
}

// ---------------------------------------------------------------------------
// move (free-form state transition)
// ---------------------------------------------------------------------------

/// Relocate a task to any status — a plain directory move with frontmatter and
/// state-path sync, no GitHub or worktree side effects. Store-only so the TUI
/// can move tasks in any discovered project. The rich forward verbs (`start`,
/// `done`, `cancel`) own the side effects; this is the escape hatch that makes
/// every transition reversible (e.g. reopen a done task back to active).
pub fn relocate(store: &Store, task: &TaskRef, to: Status) -> Result<PathBuf> {
    let new_path = store.move_task(task, to)?;
    let mut st = state::load(store, &task.id);
    st.path = new_path
        .strip_prefix(&store.root)
        .unwrap_or(&new_path)
        .to_string_lossy()
        .to_string();
    state::save(store, &task.id, &st)?;
    Ok(new_path)
}

/// `rein move <task> <status>` — move a task between any two states.
pub fn move_to(ctx: &Ctx, query: &str, status: &str) -> Result<()> {
    let to = Status::parse(status)
        .with_context(|| format!("unknown status '{}' (inbox|active|done|canceled)", status))?;
    let task = ctx.store.find(query)?;
    if task.status == to {
        bail!("'{}' is already {}", task.slug, to.as_str());
    }
    relocate(&ctx.store, &task, to)?;
    println!("moved {} {} → {}", task.slug, task.status.as_str(), to.as_str());
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
    let gh = Gh::in_dir(&ctx.repo.workdir);
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
        let gh = Gh::in_dir(&ctx.repo.workdir);
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
