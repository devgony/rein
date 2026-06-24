use crate::gh::Gh;
use crate::gitx::{self, Repo};
use crate::resolve;
use crate::state;
use crate::store::{Status, Store, TaskRef};
use crate::task;
use crate::util;
use crate::Ctx;
use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Default agent command for `rein run`. `claude --bg` dispatches a tracked
/// background session (visible in `claude agents`, attach with `claude attach
/// <id>`) and returns immediately. Launched via `sh -c`, so it can read the
/// `REIN_*` env vars rein exports; override with `REIN_RUN_CMD` or git `rein.run`.
///
/// `--name "$REIN_TITLE"` pins the session's display name (and terminal title)
/// to `rein:<branch>:<open task numbers>` (see `run`), so a glance at `claude
/// agents` says which branch and which open items the session is working. A
/// custom command can use `$REIN_TITLE` the same way (or set its own `--name`).
const DEFAULT_RUN_CMD: &str =
    "claude --bg --dangerously-skip-permissions --name \"$REIN_TITLE\" /run-rein-task";

/// Default LLM command for `rein summary`. `claude -p` runs Claude Code in
/// non-interactive print mode: rein pipes the prompt (the task's items) on stdin
/// and reads the completion from stdout. No tools are needed (pure text), so no
/// skip-permissions. Override with `REIN_SUMMARY_CMD` or git `rein.summary`.
const DEFAULT_SUMMARY_CMD: &str = "claude -p";

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

    let branch_name = branch.map(|b| b.to_string()).unwrap_or_else(|| task.slug.clone());

    if worktree {
        setup_worktree(ctx, &task, &branch_name, &mut st)?;
    } else if branch.is_some() {
        setup_branch(ctx, &task, &branch_name, &mut st)?;
        // single mode: current pointer
        ctx.store.write_current(&task.id)?;
    } else {
        // single mode claims no new branch — the work happens on whatever branch
        // is already checked out, so record *that* in the frontmatter (and state)
        // rather than leaving `branch:` blank. Detached HEAD → nothing to record.
        if let Some(current) = ctx.repo.current_branch() {
            st.branch = Some(current.clone());
            set_branch_frontmatter(ctx, &task, &current)?;
        }
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
        let branch = (!worktree).then(|| task.slug.clone());
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
        .unwrap_or_else(|| task.slug.clone());
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
    // When the task is already linked to an issue, the issue holds the full
    // managed task description — so the PR body is just `resolves #<n>` (GitHub's
    // closing keyword auto-closes the issue on merge and cross-links the two),
    // avoiding a duplicate of the body that already lives on the issue. With no
    // issue, the PR body carries the full projection and seeds the sync baseline.
    let (body, pr_synced) = match task.doc.front.github_issue {
        Some(issue) => (format!("resolves #{}", issue), None),
        None => {
            let projection = task::pr_projection(&task.doc);
            let hash = crate::sync::hash_block(&projection);
            (projection, Some(hash))
        }
    };
    let number = gh.pr_create_draft(&task.doc.front.title, &body, branch_name)?;
    let mut doc = task.doc.clone();
    doc.front.github_pr = Some(number);
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    st.pr_synced_hash = pr_synced;
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

/// Launch an agent on a task in its own working directory, in the background.
///
/// rein knows where each task lives — it runs the configured command (default:
/// Claude Code) with cwd set to the task's worktree (or the main repo if it only
/// has a branch), exporting `REIN_TASK`/`REIN_SLUG`/`REIN_BRANCH`/`REIN_DIR` so
/// the agent resolves the task regardless of cwd. `rein run` waits for the
/// command and surfaces its output, so the command must self-background and
/// return promptly: the default `claude --bg` dispatches a tracked session and
/// returns at once; a custom `REIN_RUN_CMD` should do the same. The agent's
/// transcript lands in its own standard location (e.g. `~/.claude/projects/…`).
pub fn run(ctx: &Ctx, query: Option<&str>) -> Result<String> {
    let task = match query {
        Some(q) => ctx.store.find(q)?,
        None => resolve::resolve_task(ctx, None)?.0,
    };
    let mut st = state::load(&ctx.store, &task.id);
    let worktree = st.worktree.as_deref().map(PathBuf::from).filter(|p| p.exists());
    let dir = worktree.clone().unwrap_or_else(|| ctx.repo.workdir.clone());
    let branch = st
        .branch
        .clone()
        .or_else(|| task.doc.front.branch.clone())
        .unwrap_or_default();

    // session title `rein:<branch>:<open task numbers>` — the open (unchecked,
    // unfailed) item numbers, range-folded like `1~12,14,16`. IDs are computed
    // query-only (no write), the same way `rein todo` numbers them, so the title
    // matches the ids the agent sees. Falls back to the slug when no branch is
    // recorded, and drops the trailing `:` when there is nothing open.
    let (assigned, _) = task::ensure_item_ids(&task.doc.body);
    let open_ids: Vec<String> = task::scan_items(&assigned)
        .into_iter()
        .filter(|it| !it.checked && !it.failed)
        .filter_map(|it| it.id)
        .collect();
    let label = if branch.is_empty() { task.slug.clone() } else { branch.clone() };
    let numbers = task::number_ranges(&open_ids);
    let title = if numbers.is_empty() {
        format!("rein:{}", label)
    } else {
        format!("rein:{}:{}", label, numbers)
    };

    let cmd = std::env::var("REIN_RUN_CMD")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| ctx.repo.config_get("rein.run"))
        .unwrap_or_else(|| DEFAULT_RUN_CMD.to_string());

    // a fresh worktree only has the run-rein-task skill if it was committed; install
    // rein's bundled copy at the user level (~/.claude/skills) when absent so the
    // default `/run-rein-task` resolves in any worktree without touching the repo
    if let Some(p) = crate::commands::local::ensure_user_skill().unwrap_or(None) {
        println!("installed run-rein-task skill at {}", p.display());
    }

    // run the command and surface its output; `claude --bg` returns at once after
    // dispatching the session and prints `backgrounded · <id> · <name>` + hints
    let out = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(&dir)
        .env("REIN_TASK", &task.id)
        .env("REIN_SLUG", &task.slug)
        .env("REIN_BRANCH", &branch)
        .env("REIN_DIR", &dir)
        .env("REIN_TITLE", &title)
        .output()
        .context("failed to launch run command")?;
    let reported = strip_ansi(&format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ));

    // capture the daemon-assigned session id so `rein logs` can point back at it
    if let Some(id) = parse_session_id(&reported) {
        st.run_session = Some(id);
        state::save(&ctx.store, &task.id, &st)?;
    }

    // return the summary (the caller decides how to show it — stdout or a TUI popup)
    let mut msg = format!("running {} in {}", task.id, dir.display());
    if worktree.is_none() {
        msg.push_str("\nnote: no worktree — running in the main repo (edits are not isolated)");
    }
    let reported = reported.trim_end();
    if !reported.is_empty() {
        msg.push('\n');
        msg.push_str(reported);
    }
    Ok(msg)
}

/// Point at the background session of a task's most recent `rein run`, using
/// Claude Code's own viewers (it tracks the session, transcript, and liveness).
pub fn logs(ctx: &Ctx, query: Option<&str>) -> Result<()> {
    let task = match query {
        Some(q) => ctx.store.find(q)?,
        None => resolve::resolve_task(ctx, None)?.0,
    };
    let session = state::load(&ctx.store, &task.id)
        .run_session
        .with_context(|| format!("no run recorded for '{}' — run `rein run` first", task.slug))?;
    println!("session {}", session);
    println!("  claude attach {}   # watch live / resume", session);
    println!("  claude logs {}     # recent output", session);
    println!("  claude agents        # all background sessions");
    Ok(())
}

/// Pull the short session id out of `claude --bg`'s `backgrounded · <id> · …`
/// line (the first 8-hex token), so `rein logs` can reference it later.
fn parse_session_id(s: &str) -> Option<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|t| t.len() == 8 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_string)
}

/// Strip ANSI CSI escape sequences so captured agent output is safe to print and
/// parse. Operates on bytes (the escapes are ASCII; multibyte chars are copied verbatim).
fn strip_ansi(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b && i + 1 < b.len() && b[i + 1] == b'[' {
            i += 2;
            while i < b.len() && !b[i].is_ascii_alphabetic() {
                i += 1;
            }
            i += 1; // consume the terminating letter
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// `rein log <text> --item <item-id> [--task <doc>]` — append an Agent-Log entry tied to a
/// checklist item. The item id is mandatory (so a long task list never loses the
/// association the way a hand-written `Task<id>:` prefix does) and the entry is
/// written as `Task<id>: <text>`, the convention `rein ui`'s per-item log filter
/// matches. `--task` selects the document like the other mutation verbs; without
/// it the document is resolved implicitly (worktree / REIN_TASK / current). Use
/// `rein note` for an entry not about any specific item.
pub fn log(ctx: &Ctx, text: &str, item_id: &str, flag: Option<&str>) -> Result<()> {
    let task = resolve_for_mutation(ctx, flag)?;
    // validate the item exists before logging, so a typo'd id fails loudly with
    // the available ids instead of silently tagging a non-existent item
    if !task::scan_items(&task.doc.body)
        .iter()
        .any(|i| i.id.as_deref() == Some(item_id))
    {
        return Err(item_error_hint(
            ctx,
            &task,
            item_id,
            anyhow!("item '{}' not found", item_id),
        ));
    }
    let mut doc = task.doc.clone();
    doc.body = task::append_log(
        &doc.body,
        &format!("{} Task{}: {}", util::now_iso(), item_id, text),
    );
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!("logged Task{} in {}", item_id, task.slug);
    Ok(())
}

/// `rein note <text> [--task <doc>]` — append a general Agent-Log entry not tied
/// to any checklist item (a cross-cutting observation, a decision, a heads-up).
/// `--task` selects the document like the other mutation verbs; without it the
/// resolved task is used.
pub fn note(ctx: &Ctx, text: &str, flag: Option<&str>) -> Result<()> {
    let task = resolve_for_mutation(ctx, flag)?;
    let mut doc = task.doc.clone();
    doc.body = task::append_log(&doc.body, &format!("{} {}", util::now_iso(), text));
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!("noted in {}", task.slug);
    Ok(())
}

/// `rein title <text> [--task <doc>]` — set the frontmatter title (LLM-safe: rein
/// owns the write, the caller passes text). Single line — newlines are folded to
/// spaces so the frontmatter stays valid.
pub fn set_title(ctx: &Ctx, text: &str, flag: Option<&str>) -> Result<()> {
    let title = text.trim().replace('\n', " ");
    if title.is_empty() {
        bail!("title is empty");
    }
    let task = resolve_for_mutation(ctx, flag)?;
    let mut doc = task.doc.clone();
    doc.front.title = title;
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!("title set for {}", task.slug);
    Ok(())
}

/// `rein goal <text> [--task <doc>]` — set the `## Goal` section (LLM-safe: rein
/// owns the rewrite). The body is replaced wholesale; the heading and the rest of
/// the document are preserved.
pub fn set_goal(ctx: &Ctx, text: &str, flag: Option<&str>) -> Result<()> {
    let goal = text.trim();
    if goal.is_empty() {
        bail!("goal is empty");
    }
    let task = resolve_for_mutation(ctx, flag)?;
    let mut doc = task.doc.clone();
    doc.body = task::set_section_content(&doc.body, "## Goal", goal);
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    println!("goal set for {}", task.slug);
    Ok(())
}

/// `rein summary [task]` — have an LLM summarize the task's checklist items into a
/// concise title + Goal, then apply both through rein's own safe-write path (the
/// same logic `rein title`/`rein goal` use) — the LLM only returns text, it never
/// edits the Markdown. Useful right after `rein new <branch>`, when the title and
/// Goal are still the branch-name placeholder but the items hold the real plan.
pub fn summary(ctx: &Ctx, query: Option<&str>) -> Result<String> {
    let task = match query {
        Some(q) => ctx.store.find(q)?,
        None => resolve::resolve_task(ctx, None)?.0,
    };
    let items = task::scan_items(&task.doc.body);
    if items.is_empty() {
        bail!(
            "'{}' has no checklist items to summarize — add some under ## Tasks first",
            task.slug
        );
    }
    let prompt = summary_prompt(&items);
    let out = run_summary_llm(ctx, &prompt)?;
    let (title, goal) = parse_summary(&out)?;

    // apply both in a single write, via the same logic `rein title`/`rein goal`
    // own — the LLM produced text, rein produces the bytes
    let mut doc = task.doc.clone();
    doc.front.title = title.clone();
    doc.body = task::set_section_content(&doc.body, "## Goal", &goal);
    doc.touch();
    ctx.store.write_doc(&task.path, &doc)?;
    // return the result; the caller decides how to show it (CLI stdout or TUI popup)
    Ok(format!("summarized {} → title: {}", task.slug, title))
}

/// Build the LLM prompt: the checklist item texts plus a strict output contract.
fn summary_prompt(items: &[task::Item]) -> String {
    let mut p = String::from(
        "You are summarizing a software task for a task tracker, from its checklist items.\n\
         Write a concise one-line title naming the overall task, and a 1-3 sentence Goal\n\
         describing the objective. Respond with EXACTLY these two fields and nothing else:\n\
         TITLE: <one line>\n\
         GOAL: <one to three sentences>\n\n\
         Checklist items:\n",
    );
    for it in items {
        p.push_str("- ");
        p.push_str(it.text.trim());
        p.push('\n');
    }
    p
}

/// Run the configured summary command (`REIN_SUMMARY_CMD` → git `rein.summary` →
/// default `claude -p`), piping `prompt` on stdin and returning its stdout.
fn run_summary_llm(ctx: &Ctx, prompt: &str) -> Result<String> {
    let cmd = std::env::var("REIN_SUMMARY_CMD")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| ctx.repo.config_get("rein.summary"))
        .unwrap_or_else(|| DEFAULT_SUMMARY_CMD.to_string());
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(&ctx.repo.workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to launch summary command")?;
    // best-effort write: a command that doesn't drain stdin would otherwise EPIPE
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "summary command failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse the LLM's `TITLE:`/`GOAL:` contract. The title is the first `TITLE:`
/// line; the goal is everything from `GOAL:` onward (so it may span lines). Both
/// prefixes are matched case-insensitively and tolerate leading whitespace.
fn parse_summary(out: &str) -> Result<(String, String)> {
    let mut title: Option<String> = None;
    let mut goal_lines: Vec<String> = Vec::new();
    let mut in_goal = false;
    for line in out.lines() {
        let trimmed = line.trim_start();
        let upper = trimmed.to_uppercase();
        if title.is_none() && upper.starts_with("TITLE:") {
            title = Some(trimmed[6..].trim().to_string());
            in_goal = false;
        } else if upper.starts_with("GOAL:") {
            in_goal = true;
            let rest = trimmed[5..].trim();
            if !rest.is_empty() {
                goal_lines.push(rest.to_string());
            }
        } else if in_goal {
            goal_lines.push(line.to_string());
        }
    }
    let title = title
        .filter(|t| !t.is_empty())
        .context("summary output had no non-empty TITLE: line")?;
    let goal = goal_lines.join("\n").trim().to_string();
    if goal.is_empty() {
        bail!("summary output had no GOAL: content");
    }
    Ok((title, goal))
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
    // the blocker reason still lands in the (local, non-projected) Agent Log,
    // tagged `Task<id>` so it shows under the item in the TUI's per-item log
    // (the same convention `rein log` uses) rather than being orphaned
    doc.body = task::append_log(
        &doc.body,
        &format!("{} Task{}: FAIL {}", util::now_iso(), item_id, reason),
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
    doc.body = task::append_log(&doc.body, &format!("{} Task{}: RETRY", util::now_iso(), item_id));
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
    if task.doc.front.github_issue.is_some() {
        return Ok(());
    }
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

// ---------------------------------------------------------------------------
// delete (hard removal)
// ---------------------------------------------------------------------------

/// `rein delete <task>` — permanently remove a task: its document, derived
/// state, any conflict backups, and its worktree. A local, destructive
/// operation with no GitHub side effects (use `cancel` to also close a linked
/// issue). Refuses a dirty worktree unless `--force`, mirroring `cancel`.
pub fn delete(ctx: &Ctx, query: &str, force: bool) -> Result<()> {
    let task = ctx.store.find(query)?;
    // pre-flight before any removal so a refusal leaves the task fully intact
    let wt = worktree_info(ctx, &task.id)?;
    if let Some(info) = &wt {
        if info.dirty && !force {
            bail!(
                "worktree {} has uncommitted changes — use --force to discard, or commit first",
                info.path.display()
            );
        }
    }

    // remove the worktree first (git needs the repo intact), then the records
    if let Some(info) = wt {
        ctx.repo.worktree_remove(&info.path, force)?;
        println!("removed worktree {}", info.path.display());
    }
    std::fs::remove_file(&task.path)
        .with_context(|| format!("failed to remove {}", task.path.display()))?;
    state::remove(&ctx.store, &task.id)?;
    // drop any conflict backups keyed by this task's slug (best-effort)
    for suffix in [".local.md", ".remote.md"] {
        let backup = ctx.store.conflicts_dir().join(format!("{}{}", task.slug, suffix));
        if backup.exists() {
            let _ = std::fs::remove_file(&backup);
        }
    }
    if ctx.store.read_current().as_deref() == Some(task.id.as_str()) {
        ctx.store.clear_current()?;
    }
    println!("deleted {}", task.id);
    Ok(())
}
