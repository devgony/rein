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

const SKILL_MD: &str = r#"---
description: Run the current LLM task document, implement unchecked tasks, update status via rein commands, and append execution notes.
disable-model-invocation: true
---

Run `rein current --path` to find the active task document, then read it.

Rules:

1. Execute only unchecked tasks.
2. Never edit checkboxes or Agent Log in the Markdown directly. Use:
   - `rein check <item-id>` after a task is implemented and verified
   - `rein log "<text>"` to append a concise entry after each completed task
   - `rein fail <item-id> --reason "<text>"` when blocked
3. Preserve `<!-- task:... -->` ID comments when editing other sections.
4. Run relevant tests before checking validation items.
5. If a PR is attached, run `rein push` when finished.
"#;

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
        let skill_path = repo
            .workdir
            .join(".claude/skills/run-rein-task/SKILL.md");
        util::atomic_write(&skill_path, SKILL_MD)?;
        println!("skill: {}", skill_path.display());
    }
    Ok(())
}

pub fn new(ctx: &Ctx, title: &str, shared: bool) -> Result<()> {
    let slug = ctx.store.unique_slug(&util::slugify(title));
    let id = format!("task-{}-{}", util::today_compact(), slug);
    let doc = TaskDoc::template(&id, title, "inbox", &util::now_iso(), shared);
    let path = ctx.store.status_dir(Status::Inbox).join(format!("{}.md", slug));
    if path.exists() {
        bail!("file already exists: {}", path.display());
    }
    ctx.store.write_doc(&path, &doc)?;
    let st = TaskState {
        path: format!("inbox/{}.md", slug),
        ..Default::default()
    };
    state::save(&ctx.store, &id, &st)?;
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
        crate::commands::assign_ids(ctx, &t)?;
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
        crate::commands::assign_ids(ctx, &fresh)?;
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
