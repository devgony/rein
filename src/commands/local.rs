use crate::gitx::Repo;
use crate::resolve::{self, Source};
use crate::state::{self, TaskState};
use crate::store::{Status, Store};
use crate::task::TaskDoc;
use crate::util;
use crate::Ctx;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

pub(crate) const SKILL_MD: &str = r#"---
description: Run the current LLM task document, implement unchecked tasks, update status via rein commands, and append execution notes.
disable-model-invocation: true
---

Run `rein todo` to list the current task's unchecked items. Each line is `<id>` then the item text, grouped under its `## section`. Read the full document with `rein current --path` only when you need the Goal or Notes for context.

Rules:

1. Execute only the unchecked items `rein todo` prints.
2. Never edit checkboxes or Agent Log in the Markdown directly. Use:
   - `rein check <item-id>` after a task is implemented and verified
   - `rein log "<text>" --item <item-id>` to record progress on a specific item — `--item` is required and the entry is tagged so it shows under that item in `rein ui`
   - `rein note "<text>"` to append an Agent Log entry not tied to any specific item
   - `rein fail <item-id> --reason "<text>"` when blocked — resolves the item (it drops out of `rein todo`, so a re-run won't re-attempt it); `rein retry <item-id>` reopens it
3. Preserve `<!-- task:... -->` ID comments when editing other sections.
4. Run relevant tests before checking validation items.
5. If a PR is attached, run `rein push` when finished.
"#;

pub(crate) fn run_task_prompt() -> &'static str {
    SKILL_MD
}

pub fn init(skill: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let (store, key, created) = Store::resolve_or_create_key(&repo)?;
    store.ensure_layout(&repo)?;
    if created {
        println!("registered store key in git config rein.store ({})", key);
    }
    println!("store: {}", store.root.display());
    if skill {
        let skill_path = write_skill(&repo.workdir)?;
        println!("skill: {}", skill_path.display());
        let agent_skill_path = write_agent_skill(&repo.workdir)?;
        println!("agent skill: {}", agent_skill_path.display());
    }
    Ok(())
}

/// Write the bundled `run-rein-task` skill under `base/.claude/skills/`, returning
/// its path. Used by `init --skill` and `rein run` (to seed the skill in a fresh
/// worktree). Overwrites — call `ensure_skill` to write only when missing.
pub(crate) fn write_skill(base: &std::path::Path) -> Result<std::path::PathBuf> {
    let path = base.join(".claude/skills/run-rein-task/SKILL.md");
    util::atomic_write(&path, SKILL_MD)?;
    Ok(path)
}

/// Write the bundled `run-rein-task` skill where Codex/agent runners discover
/// project skills.
pub(crate) fn write_agent_skill(base: &std::path::Path) -> Result<std::path::PathBuf> {
    let path = base.join(".agents/skills/run-rein-task/SKILL.md");
    util::atomic_write(&path, SKILL_MD)?;
    Ok(path)
}

/// Claude Code's config dir: `$CLAUDE_CONFIG_DIR` or `~/.claude`. `None` if HOME
/// is unset and the env isn't given. Skills live under `skills/`, transcripts
/// under `projects/`.
pub(crate) fn claude_config_dir() -> Option<std::path::PathBuf> {
    std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|h| std::path::PathBuf::from(h).join(".claude"))
        })
}

/// Personal (user-level) location of the bundled skill, where Claude Code
/// discovers it for every project regardless of cwd.
fn user_skill_path() -> Option<std::path::PathBuf> {
    Some(claude_config_dir()?.join("skills/run-rein-task/SKILL.md"))
}

/// Install the bundled skill at the user level if absent, so `rein run` resolves
/// `/run-rein-task` in any worktree without adding files to the repo. Returns the
/// path if it wrote one; leaves an existing (possibly customized) skill untouched.
pub(crate) fn ensure_user_skill() -> Result<Option<std::path::PathBuf>> {
    let Some(path) = user_skill_path() else {
        return Ok(None);
    };
    if path.exists() {
        return Ok(None);
    }
    util::atomic_write(&path, SKILL_MD)?;
    Ok(Some(path))
}

/// Create an inbox task draft in `store`. Returns its id and document path.
/// Store-only (no repo) so the TUI can create in any discovered project.
pub fn create_task(store: &Store, title: &str, shared: bool) -> Result<(String, std::path::PathBuf)> {
    let slug = store.unique_slug(&util::slugify(title));
    let id = format!("task-{}-{}", util::today_compact(), slug);
    let doc = TaskDoc::template(&id, title, "inbox", &util::now_iso(), shared);
    let path = store.status_dir(Status::Inbox).join(format!("{}.md", slug));
    if path.exists() {
        bail!("file already exists: {}", path.display());
    }
    store.write_doc(&path, &doc)?;
    let st = TaskState {
        path: format!("inbox/{}.md", slug),
        ..Default::default()
    };
    state::save(store, &id, &st)?;
    Ok((id, path))
}

pub fn new(ctx: &Ctx, title: &str, shared: bool) -> Result<()> {
    let (id, path) = create_task(&ctx.store, title, shared)?;
    println!("{}", id);
    println!("{}", path.display());
    Ok(())
}

pub fn list(ctx: &Ctx, status: Option<&str>) -> Result<()> {
    let filter = match status {
        Some(s) => Some(Status::parse(s).with_context(|| format!("unknown status '{}'", s))?),
        None => None,
    };
    for t in ctx.store.list_tasks() {
        if let Some(f) = filter {
            if t.status != f {
                continue;
            }
        }
        println!(
            "{:<9} {:<28} {:<40} {}",
            t.status.as_str(),
            t.slug,
            t.id,
            t.doc.front.title
        );
    }
    Ok(())
}

pub fn open(ctx: &Ctx, task: Option<&str>) -> Result<()> {
    let path = match task {
        Some(q) => ctx.store.find(q)?.path,
        None => {
            let tasks = ctx.store.list_tasks();
            if tasks.is_empty() {
                bail!("no tasks — create one with `rein new <title>`");
            }
            match crate::ui::pick_task(&tasks)? {
                Some(p) => p,
                None => return Ok(()), // user aborted picker
            }
        }
    };
    edit_file(&path)?;
    // assign stable IDs to whatever items the user just wrote in $EDITOR
    if let Some(t) = ctx.store.list_tasks().into_iter().find(|t| t.path == path) {
        crate::commands::assign_ids(&ctx.store, &t)?;
    }
    Ok(())
}

pub fn edit_file(path: &Path) -> Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("failed to run $EDITOR ({})", editor))?;
    if !status.success() {
        bail!("editor exited with {}", status);
    }
    Ok(())
}

pub fn current(ctx: &Ctx, path: bool) -> Result<()> {
    let (task, _source) = resolve::resolve_task(ctx, None)?;
    if path {
        println!("{}", task.path.display());
    } else {
        println!("{}", task.id);
    }
    Ok(())
}

pub fn use_task(ctx: &Ctx, query: &str) -> Result<()> {
    let task = ctx.store.find(query)?;
    // symmetric with resolution: a bound worktree rebinds its pointer,
    // anywhere else writes the single-mode current file.
    if ctx.repo.read_task_pointer().is_some() {
        ctx.repo.write_task_pointer(&task.id)?;
        println!("worktree now bound to {}", task.id);
    } else {
        ctx.store.write_current(&task.id)?;
        println!("current is now {}", task.id);
    }
    Ok(())
}

pub fn root(ctx: &Ctx) -> Result<()> {
    println!("{}", ctx.store.root.display());
    Ok(())
}

pub fn status(ctx: &Ctx) -> Result<()> {
    println!("store: {}", ctx.store.root.display());
    let resolved = resolve::resolve_task(ctx, None).ok();
    match &resolved {
        Some((task, source)) => {
            let how = match source {
                Source::Flag => "--task",
                Source::WorktreePointer => "worktree",
                Source::Env => "REIN_TASK",
                Source::CurrentFile => "current",
            };
            println!("task: {} ({}, via {})", task.id, task.status.as_str(), how);
        }
        None => println!("task: none"),
    }
    let tasks = ctx.store.list_tasks();
    for s in [Status::Inbox, Status::Active, Status::Done, Status::Canceled] {
        let count = tasks.iter().filter(|t| t.status == s).count();
        println!("{:<9} {}", s.as_str(), count);
    }
    for t in tasks.iter().filter(|t| t.status == Status::Active) {
        let st = state::load(&ctx.store, &t.id);
        println!(
            "active: {} branch={} worktree={}",
            t.slug,
            st.branch.as_deref().unwrap_or("-"),
            st.worktree.as_deref().unwrap_or("-")
        );
    }
    // checklist of the resolved task, with the integer ids `rein check` accepts.
    // Numbers are computed deterministically (same as assignment) without writing.
    if let Some((task, _)) = &resolved {
        let (assigned, _) = crate::task::ensure_item_ids(&task.doc.body);
        let items = crate::task::scan_items(&assigned);
        if !items.is_empty() {
            println!("items ({}):", task.slug);
            for it in items {
                println!(
                    "  [{}] {:<3} {}",
                    if it.checked { "x" } else { " " },
                    it.id.as_deref().unwrap_or("-"),
                    it.text
                );
            }
        }
    }
    Ok(())
}

/// `rein todo [--task <id>] [--all]` — print the resolved task's unchecked
/// checklist items (id + text, grouped by section) so the skill can get the
/// to-do list directly instead of reading and parsing the whole document.
/// Query-only: IDs are computed deterministically (same as assignment) without
/// writing, so a following `rein check <id>` lands on the same number.
pub fn todo(ctx: &Ctx, flag: Option<&str>, all: bool) -> Result<()> {
    let (task, _) = resolve::resolve_task(ctx, flag)?;
    let (assigned, _) = crate::task::ensure_item_ids(&task.doc.body);
    let items = crate::task::scan_items(&assigned);
    let sections = crate::task::item_sections(&assigned);
    let mut last: Option<&str> = None;
    for (it, section) in items.iter().zip(sections.iter()) {
        // resolved items (done or failed) drop out of the default todo list so a
        // re-run never re-attempts a blocked item; --all surfaces them, marked.
        if !all && (it.checked || it.failed) {
            continue;
        }
        if Some(section.as_str()) != last {
            if !section.is_empty() {
                println!("## {}", section);
            }
            last = Some(section.as_str());
        }
        let id = it.id.as_deref().unwrap_or("-");
        if all {
            let mark = if it.failed {
                "!"
            } else if it.checked {
                "x"
            } else {
                " "
            };
            println!("{}\t[{}] {}", id, mark, it.text);
        } else {
            println!("{}\t{}", id, it.text);
        }
    }
    Ok(())
}

/// Rebuild state/ from task files (directory + frontmatter are truth),
/// fix frontmatter status drift, drop orphan state files, validate current.
pub fn doctor(ctx: &Ctx) -> Result<()> {
    ctx.store.ensure_layout(&ctx.repo)?;
    let tasks = ctx.store.list_tasks();
    let mut seen = Vec::new();
    for t in &tasks {
        // directory is truth for status
        if t.doc.front.status != t.status.as_str() {
            let mut doc = t.doc.clone();
            doc.front.status = t.status.as_str().to_string();
            doc.touch();
            ctx.store.write_doc(&t.path, &doc)?;
            println!("fixed status: {} -> {}", t.slug, t.status.as_str());
        }
        // assign any missing item IDs (heals docs edited outside sync)
        let fresh = ctx.store.find_by_id(&t.id).unwrap_or_else(|| t.clone());
        crate::commands::assign_ids(&ctx.store, &fresh)?;
        let rel = t
            .path
            .strip_prefix(&ctx.store.root)
            .unwrap_or(&t.path)
            .to_string_lossy()
            .to_string();
        let mut st = state::load(&ctx.store, &t.id); // keep synced hashes if present
        st.path = rel;
        state::save(&ctx.store, &t.id, &st)?;
        seen.push(t.id.clone());
    }
    // orphan state files
    let state_dir = ctx.store.root.join("state");
    if let Ok(entries) = fs::read_dir(&state_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if p.extension().and_then(|e| e.to_str()) == Some("json")
                && !seen.iter().any(|id| id == stem)
            {
                fs::remove_file(&p)?;
                println!("removed orphan state: {}", stem);
            }
        }
    }
    // stale current pointer
    if let Some(id) = ctx.store.read_current() {
        if !seen.iter().any(|s| s == &id) {
            ctx.store.clear_current()?;
            println!("cleared stale current: {}", id);
        }
    }
    println!("doctor: {} tasks ok", tasks.len());
    Ok(())
}
