use anyhow::Result;
use clap::{Parser, Subcommand};
use rein::commands::{exec, local, sync_cmd};
use rein::Ctx;

#[derive(Parser)]
#[command(name = "rein", version, about = "LLM task journal + shared inbox manager")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the store and register the repo key (git config rein.store)
    Init {
        /// Scaffold .claude/skills/run-rein-task/SKILL.md in the repo
        #[arg(long)]
        skill: bool,
    },
    /// Create a task draft in the inbox
    New {
        title: String,
        #[arg(long)]
        shared: bool,
    },
    /// List tasks
    List {
        #[arg(long)]
        status: Option<String>,
    },
    /// List the resolved task's unchecked items (id + text) for the skill
    Todo {
        /// Include checked items too, with their state
        #[arg(long)]
        all: bool,
        #[arg(long)]
        task: Option<String>,
    },
    /// Open a task in $EDITOR (fuzzy picker without an argument)
    Open { task: Option<String> },
    /// Print the resolved task (resolution: --task > worktree > REIN_TASK > current)
    Current {
        /// Print the document path instead of the task id
        #[arg(long)]
        path: bool,
    },
    /// Switch the task binding (worktree pointer inside a bound worktree, current file otherwise)
    Use { task: String },
    /// Move a task to any state (plain relocation; no worktree/GitHub effects)
    Move {
        task: String,
        /// Target state: inbox | active | done | canceled
        status: String,
    },
    /// Claim a task: inbox → active (optionally in a new worktree)
    Start {
        task: String,
        #[arg(long)]
        worktree: bool,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long = "draft-pr")]
        draft_pr: bool,
    },
    /// Check an item (LLM-safe mutation)
    Check {
        item_id: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// Uncheck an item
    Uncheck {
        item_id: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// Append a line to the Agent Log
    Log {
        text: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// Record a blocker for an item (resolves it as failed)
    Fail {
        item_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// Reopen a failed item (inverse of `fail`)
    Retry {
        item_id: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// Publish a task as a GitHub issue (shared inbox)
    Issue { task: String },
    /// Import/refresh all rein-labeled issues
    PullInbox,
    /// Pull the resolved task's issue
    Pull,
    /// Push the resolved task to its issue/PR managed sections
    Push {
        /// Force-push local over a detected conflict
        #[arg(long)]
        resolved: bool,
    },
    /// Attach an existing issue to the resolved task
    AttachIssue { number: u64 },
    /// Attach an existing PR to the resolved task
    AttachPr { number: u64 },
    /// Finish a task: move to done/, close issue, update PR, remove worktree
    Done {
        task: Option<String>,
        #[arg(long = "keep-worktree")]
        keep_worktree: bool,
    },
    /// Cancel a task: move to canceled/, close issue as not planned
    Cancel {
        task: Option<String>,
        #[arg(long = "keep-worktree")]
        keep_worktree: bool,
        #[arg(long)]
        force: bool,
    },
    /// Rebuild state/ from task files, fix drift
    Doctor,
    /// Show store, resolved task and per-status counts
    Status,
    /// Print the store path
    Root,
    /// TUI dashboard
    Ui,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    // Commands that must run without a resolved store: init creates it; ui is a
    // cross-project dashboard that discovers every store and works anywhere.
    match &cli.cmd {
        Cmd::Init { skill } => return local::init(*skill),
        Cmd::Ui => return rein::ui::run(),
        _ => {}
    }
    let ctx = Ctx::load()?;
    match cli.cmd {
        Cmd::Init { .. } | Cmd::Ui => unreachable!(),
        Cmd::New { title, shared } => local::new(&ctx, &title, shared),
        Cmd::List { status } => local::list(&ctx, status.as_deref()),
        Cmd::Todo { all, task } => local::todo(&ctx, task.as_deref(), all),
        Cmd::Open { task } => local::open(&ctx, task.as_deref()),
        Cmd::Current { path } => local::current(&ctx, path),
        Cmd::Use { task } => local::use_task(&ctx, &task),
        Cmd::Move { task, status } => exec::move_to(&ctx, &task, &status),
        Cmd::Start {
            task,
            worktree,
            branch,
            draft_pr,
        } => exec::start(&ctx, &task, worktree, branch.as_deref(), draft_pr),
        Cmd::Check { item_id, task } => exec::check(&ctx, &item_id, task.as_deref(), true),
        Cmd::Uncheck { item_id, task } => exec::check(&ctx, &item_id, task.as_deref(), false),
        Cmd::Log { text, task } => exec::log(&ctx, &text, task.as_deref()),
        Cmd::Fail {
            item_id,
            reason,
            task,
        } => exec::fail(&ctx, &item_id, &reason, task.as_deref()),
        Cmd::Retry { item_id, task } => exec::retry(&ctx, &item_id, task.as_deref()),
        Cmd::Issue { task } => sync_cmd::issue(&ctx, &task),
        Cmd::PullInbox => sync_cmd::pull_inbox(&ctx),
        Cmd::Pull => sync_cmd::pull(&ctx),
        Cmd::Push { resolved } => sync_cmd::push(&ctx, resolved),
        Cmd::AttachIssue { number } => sync_cmd::attach_issue(&ctx, number),
        Cmd::AttachPr { number } => sync_cmd::attach_pr(&ctx, number),
        Cmd::Done {
            task,
            keep_worktree,
        } => exec::done(&ctx, task.as_deref(), keep_worktree),
        Cmd::Cancel {
            task,
            keep_worktree,
            force,
        } => exec::cancel(&ctx, task.as_deref(), keep_worktree, force),
        Cmd::Doctor => local::doctor(&ctx),
        Cmd::Status => local::status(&ctx),
        Cmd::Root => local::root(&ctx),
    }
}
