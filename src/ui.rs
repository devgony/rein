use crate::gitx::{Repo, Worktree};
use crate::store::{Status, Store, StoreInfo, TaskRef};
use crate::Ctx;
use anyhow::{anyhow, Context, Result};
use crossterm::event::{
    self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io;
use std::path::{Path, PathBuf};

/// One row in the dashboard. Detached from the store so tests can build it.
/// Carries `project`/`store_root` so the cross-project view can label each task
/// and route actions back to the store it came from.
#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub status: Status,
    pub path: PathBuf,
    pub body: String,
    pub branch: Option<String>,
    pub github_issue: Option<u64>,
    pub github_pr: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
    pub tags: Vec<String>,
    pub shared: bool,
    /// State of the last `rein run`'s background session (`working`/`done`/…).
    /// Claude runs are resolved from `claude agents`; Codex runs use rein's
    /// wrapper status file.
    pub run_state: Option<String>,
    /// Isolated worktree path from the task's state, if it was started in
    /// worktree mode — distinguishes a worktree-backed task from a plain branch.
    pub worktree: Option<String>,
    /// The project's main repo working directory (from `StoreInfo`), used as the
    /// working directory for branch/single-mode tasks that have no worktree.
    pub repo_dir: Option<PathBuf>,
    pub project: String,
    pub store_root: PathBuf,
}

impl TaskRow {
    pub fn from_ref(t: &TaskRef, project: &str, store_root: &Path) -> TaskRow {
        TaskRow {
            id: t.id.clone(),
            slug: t.slug.clone(),
            title: t.doc.front.title.clone(),
            status: t.status,
            path: t.path.clone(),
            body: t.doc.body.clone(),
            branch: t.doc.front.branch.clone(),
            github_issue: t.doc.front.github_issue,
            github_pr: t.doc.front.github_pr,
            created_at: t.doc.front.created_at.clone(),
            updated_at: t.doc.front.updated_at.clone(),
            tags: t.doc.front.tags.clone(),
            shared: t.doc.front.shared,
            run_state: None,
            worktree: None,
            repo_dir: None,
            project: project.to_string(),
            store_root: store_root.to_path_buf(),
        }
    }

    /// Whether this task is backed by an isolated worktree (vs a plain branch).
    pub fn is_worktree(&self) -> bool {
        self.worktree.is_some()
    }

    /// The task's working directory: its worktree if it has one, else the
    /// project's main repo. `None` when neither is known.
    pub fn work_dir(&self) -> Option<PathBuf> {
        self.worktree
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| self.repo_dir.clone())
    }
}

/// What the event loop should do after a key.
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    None,
    Quit,
    Edit(PathBuf),
    New(String),             // create an inbox task with this title
    Start(String, StartMode), // claim inbox → active in the chosen mode
    Move(String, Status),    // free-form transition to any state
    Done(String),
    Delete(String),          // permanently remove the task (files + worktree)
    Issue(String),                       // begin creating an issue (offers a project picker)
    IssueWithProject(String, Option<String>), // create the issue, optionally onto a project board
    PushIssue(String),                   // push the managed section to an existing issue
    PushPr(String),                      // push the managed section to an existing PR
    ForcePush(String, ForceSurface),     // overwrite the remote after a sync conflict (push --resolved)
    CreatePr(String, bool), // open a draft PR; bool = worktree (vs main-repo branch)
    CopyDir(PathBuf),       // copy the task's working directory path to the clipboard
    Run(String),            // launch an agent on the task in the background
    AttachRun(String),      // open the last background run in the native agent UI
    Summary(String),        // LLM-summarize the task's items into title + Goal (rein owns the write)
    ToggleItem(String, String), // task id + item id: flip its checkbox (reopens a failed item)
    AddItem(String, String),    // task id + text: append a new checklist item to ## Tasks
    EditItem(String, String, String), // task id + item id + new text: reword a checklist item
    DeleteItem(String, String), // task id + item id: remove a checklist item
    Worktrees(String),          // anchor task id: open the worktree view for its project's repo
    AddWorktree(String, String), // anchor task id + branch: git worktree add (-b if branch is new)
    DeleteWorktree(String, String), // anchor task id + worktree path: git worktree remove
    LockWorktree(String, String, bool), // anchor task id + path + lock(true)/unlock(false)
}

/// How `s` claims a task: plain single mode, an isolated worktree, or a
/// main-repo branch — the dashboard counterpart of `rein start [--worktree|--branch]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StartMode {
    Single,
    Worktree,
    Branch,
}

const TABS: [Option<Status>; 5] = [
    None,
    Some(Status::Inbox),
    Some(Status::Active),
    Some(Status::Done),
    Some(Status::Canceled),
];

/// Which surface a force-push targets — the dashboard knows this from the key
/// that hit the conflict (`i` → issue, `p` → PR), so the offer can retry the
/// right one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ForceSurface {
    Issue,
    Pr,
}

/// A pending force-push offer, armed when a push (`i`/`p`) hits a sync conflict.
/// While present the dashboard shows a prompt: `f` overwrites the remote with
/// the local copy (the `rein push --resolved` path), any other key cancels.
#[derive(Debug, Clone, PartialEq)]
pub struct ForcePush {
    pub task_id: String,
    pub surface: ForceSurface,
    pub slug: String,
}

/// An in-flight `rein summary` launched from the dashboard (`S`). The LLM call
/// runs on a worker thread so the UI keeps animating a spinner instead of
/// freezing; the event loop polls `rx` each tick and swaps in a result popup
/// when it lands.
pub struct Summarizing {
    /// Task slug, shown in the spinner overlay.
    pub slug: String,
    /// Receives the `rein summary` outcome — the summary line or an error.
    pub rx: std::sync::mpsc::Receiver<Result<String>>,
    /// When the run started, for the elapsed-seconds readout.
    pub started: std::time::Instant,
}

pub struct App {
    pub tasks: Vec<TaskRow>,
    pub tab: usize,
    pub selected: usize,
    pub filter: String,
    pub filtering: bool,
    pub creating: bool,
    pub moving: bool,
    /// True while awaiting the delete confirmation key (`y` confirms; else cancels).
    pub deleting: bool,
    /// True while awaiting the start-mode key (`s` single / `w` worktree / `b` branch).
    pub starting: bool,
    /// True while awaiting the PR-mode key (`w` worktree / `b` branch).
    pub pring: bool,
    /// True while the optional issue→project picker is shown (after `i` on a
    /// task with no issue yet).
    pub issuing: bool,
    /// Candidate GitHub Project titles for the issue picker (fetched on `i`).
    pub issue_projects: Vec<String>,
    /// Cursor in the issue→project picker (0 = "no project", then projects).
    pub issue_sel: usize,
    /// The task awaiting issue creation while the project picker is open.
    pub issue_target: Option<String>,
    pub input: String,
    pub message: String,
    /// A modal overlay; `Some` blocks input until dismissed with any key.
    pub popup: Option<String>,
    /// Whether the current popup is an error (red) vs an informational result.
    pub popup_error: bool,
    /// Active project filter: `None` shows every project, `Some(name)` scopes
    /// the task list to one. Pre-set to the launch repo's project so `rein ui`
    /// lands in the current project's tasks.
    pub project_scope: Option<String>,
    /// Every known project (incl. ones with no tasks), for the picker.
    pub all_projects: Vec<String>,
    /// True while the left pane is the hierarchical project picker.
    pub picking_project: bool,
    /// Cursor in the project picker (0 = "all projects", then `project_list()`).
    pub project_sel: usize,
    /// Store + label of the repo `rein ui` was launched from, if any — the
    /// fallback target for `new` when no task is selected.
    pub home_store_root: Option<PathBuf>,
    pub home_label: Option<String>,
    /// True while drilled into the selected task's checklist items: the left
    /// pane lists the items and the preview shows the selected item's matching
    /// Agent-Log entries; `space` checks/unchecks the item under the cursor.
    pub viewing_items: bool,
    /// Cursor among the focused task's checklist items (only meaningful while
    /// `viewing_items`).
    pub item_sel: usize,
    /// True while typing a new checklist item in the item view (a sub-mode of
    /// `viewing_items`); the text accumulates in `input`.
    pub creating_item: bool,
    /// True while editing the selected checklist item's text (a sub-mode of
    /// `viewing_items`, prefilled into `input`).
    pub editing_item: bool,
    /// True while awaiting the item-delete confirmation key (`y` confirms; any
    /// other key cancels) in the item view.
    pub deleting_item: bool,
    /// True while drilled into the focused project's git worktrees: the left
    /// pane lists each worktree and the preview shows the selected one's
    /// details; `n` adds, `space` locks/unlocks, `d` removes, `y` copies its path.
    pub viewing_worktrees: bool,
    /// The worktrees of the anchor project's repo (refreshed after each mutation).
    pub worktrees: Vec<Worktree>,
    /// Cursor among `worktrees` (only meaningful while `viewing_worktrees`).
    pub worktree_sel: usize,
    /// Task id whose project's repo backs the worktree view — the handle the
    /// event loop resolves to a repo for list/add/remove/lock.
    pub worktree_anchor: Option<String>,
    /// Display label (project name) for the worktree view's title and hint.
    pub worktree_project: String,
    /// True while typing a branch name for a new worktree (a sub-mode of
    /// `viewing_worktrees`); the text accumulates in `input`.
    pub creating_worktree: bool,
    /// True while awaiting the worktree-remove confirmation key (`y` confirms;
    /// any other key cancels) in the worktree view.
    pub deleting_worktree: bool,
    /// An in-flight `rein summary` (the `S` action), or `None`. While `Some`,
    /// the dashboard shows an animated overlay and swallows keys until it
    /// resolves into a result popup.
    pub summarizing: Option<Summarizing>,
    /// Animation frame for the summarizing spinner, advanced each idle tick.
    pub spinner_frame: usize,
    /// A pending force-push offer after a push hit a sync conflict, or `None`.
    /// While `Some`, the dashboard shows a prompt (`f` overwrites the remote).
    pub force_push: Option<ForcePush>,
}

impl App {
    pub fn new(tasks: Vec<TaskRow>) -> App {
        App {
            tasks,
            tab: 0,
            selected: 0,
            filter: String::new(),
            filtering: false,
            creating: false,
            moving: false,
            deleting: false,
            starting: false,
            pring: false,
            issuing: false,
            issue_projects: Vec::new(),
            issue_sel: 0,
            issue_target: None,
            input: String::new(),
            message: String::new(),
            popup: None,
            popup_error: false,
            project_scope: None,
            all_projects: Vec::new(),
            picking_project: false,
            project_sel: 0,
            home_store_root: None,
            home_label: None,
            viewing_items: false,
            item_sel: 0,
            creating_item: false,
            editing_item: false,
            deleting_item: false,
            viewing_worktrees: false,
            worktrees: Vec::new(),
            worktree_sel: 0,
            worktree_anchor: None,
            worktree_project: String::new(),
            creating_worktree: false,
            deleting_worktree: false,
            summarizing: None,
            spinner_frame: 0,
            force_push: None,
        }
    }

    pub fn tab_name(&self) -> &'static str {
        match TABS[self.tab] {
            None => "all",
            Some(s) => s.as_str(),
        }
    }

    /// Canonical project list for the picker: the authoritative `all_projects`
    /// when populated (includes empty projects), else derived from the rows.
    pub fn project_list(&self) -> Vec<String> {
        if !self.all_projects.is_empty() {
            return self.all_projects.clone();
        }
        let mut names: Vec<String> = self
            .tasks
            .iter()
            .map(|t| t.project.clone())
            .filter(|p| !p.is_empty())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Label of the active project scope for display (`all` when unscoped).
    pub fn scope_name(&self) -> String {
        self.project_scope.clone().unwrap_or_else(|| "all".to_string())
    }

    /// Indices into `tasks` visible under the project scope + tab + fuzzy filter.
    pub fn visible(&self) -> Vec<usize> {
        let tab_filtered: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| match &self.project_scope {
                None => true,
                Some(p) => &t.project == p,
            })
            .filter(|(_, t)| match TABS[self.tab] {
                None => true,
                Some(s) => t.status == s,
            })
            .map(|(i, _)| i)
            .collect();
        if self.filter.is_empty() {
            return tab_filtered;
        }
        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(&self.filter, CaseMatching::Ignore, Normalization::Smart);
        // project name is searchable too, so the filter doubles as a project picker
        let candidates: Vec<(usize, String)> = tab_filtered
            .iter()
            .map(|&i| {
                (
                    i,
                    format!(
                        "{} {} {}",
                        self.tasks[i].project, self.tasks[i].slug, self.tasks[i].title
                    ),
                )
            })
            .collect();
        let matched = pattern.match_list(candidates.iter().map(|(_, s)| s.as_str()), &mut matcher);
        let kept: Vec<&str> = matched.iter().map(|(s, _)| *s).collect();
        candidates
            .iter()
            .filter(|(_, s)| kept.contains(&s.as_str()))
            .map(|(i, _)| *i)
            .collect()
    }

    pub fn selected_task(&self) -> Option<&TaskRow> {
        let vis = self.visible();
        vis.get(self.selected.min(vis.len().saturating_sub(1)))
            .map(|&i| &self.tasks[i])
    }

    pub fn on_key(&mut self, key: KeyEvent) -> UiAction {
        if key.kind != KeyEventKind::Press {
            return UiAction::None;
        }
        // a summary is running on a worker thread: swallow keys (so a second `S`
        // or a conflicting action can't fire mid-run) but still honor Ctrl-c so
        // the user is never trapped waiting on a slow LLM
        if self.summarizing.is_some() {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return UiAction::Quit;
            }
            return UiAction::None;
        }
        // a sync conflict armed a force-push offer: `f` overwrites the remote
        // with the local copy, any other key cancels (Ctrl-c still quits)
        if self.force_push.is_some() {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return UiAction::Quit;
            }
            let fp = self.force_push.take().expect("force_push is some");
            if matches!(key.code, KeyCode::Char('f')) {
                return UiAction::ForcePush(fp.task_id, fp.surface);
            }
            self.message = "force-push canceled — resolve locally and retry".into();
            return UiAction::None;
        }
        // a modal error overlay swallows the next key (any key dismisses it)
        if self.popup.is_some() {
            self.popup = None;
            return UiAction::None;
        }
        // a status message (e.g. "ok" after an action) is transient: any fresh
        // key press clears it so the keybinding hint comes back — without this a
        // single action would leave "ok" pinned in the status bar forever.
        self.message.clear();
        // drilled into the selected task's checklist items: j/k pick an item,
        // space toggles its checkbox, the preview shows its Agent-Log entries.
        // h/Esc/q step back out to the task list (Ctrl-c still quits).
        if self.viewing_items {
            // text entry for a new checklist item (a sub-mode of the item view):
            // Enter adds it, Esc cancels; everything else just edits `input`.
            if self.creating_item {
                match key.code {
                    KeyCode::Esc => {
                        self.creating_item = false;
                        self.input.clear();
                    }
                    KeyCode::Enter => {
                        self.creating_item = false;
                        let text = self.input.trim().to_string();
                        self.input.clear();
                        if !text.is_empty() {
                            if let Some(t) = self.selected_task() {
                                return UiAction::AddItem(t.id.clone(), text);
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                    }
                    KeyCode::Char(c) => self.input.push(c),
                    _ => {}
                }
                return UiAction::None;
            }
            // text entry for editing the selected item (prefilled with its text):
            // Enter saves the reworded text, Esc cancels.
            if self.editing_item {
                match key.code {
                    KeyCode::Esc => {
                        self.editing_item = false;
                        self.input.clear();
                    }
                    KeyCode::Enter => {
                        self.editing_item = false;
                        let text = self.input.trim().to_string();
                        self.input.clear();
                        if !text.is_empty() {
                            let items = self
                                .selected_task()
                                .map(|t| task_items(&t.body))
                                .unwrap_or_default();
                            if let Some(it) = items.get(self.item_sel.min(items.len().saturating_sub(1))) {
                                if let (Some(t), Some(item_id)) = (self.selected_task(), it.id.as_ref()) {
                                    return UiAction::EditItem(t.id.clone(), item_id.clone(), text);
                                }
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                    }
                    KeyCode::Char(c) => self.input.push(c),
                    _ => {}
                }
                return UiAction::None;
            }
            // confirm a checklist-item delete: only `y` proceeds; else cancel
            if self.deleting_item {
                self.deleting_item = false;
                if matches!(key.code, KeyCode::Char('y')) {
                    let items = self
                        .selected_task()
                        .map(|t| task_items(&t.body))
                        .unwrap_or_default();
                    if let Some(it) = items.get(self.item_sel.min(items.len().saturating_sub(1))) {
                        if let (Some(t), Some(item_id)) = (self.selected_task(), it.id.as_ref()) {
                            return UiAction::DeleteItem(t.id.clone(), item_id.clone());
                        }
                    }
                }
                return UiAction::None;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return UiAction::Quit;
            }
            let items = self
                .selected_task()
                .map(|t| task_items(&t.body))
                .unwrap_or_default();
            match key.code {
                KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('q') => {
                    self.viewing_items = false;
                    self.creating_item = false;
                    self.editing_item = false;
                    self.deleting_item = false;
                    self.input.clear();
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if !items.is_empty() {
                        self.item_sel = (self.item_sel + 1).min(items.len() - 1);
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.item_sel = self.item_sel.saturating_sub(1);
                }
                KeyCode::Char(' ') => {
                    if let Some(it) = items.get(self.item_sel.min(items.len().saturating_sub(1))) {
                        if let (Some(t), Some(item_id)) = (self.selected_task(), it.id.as_ref()) {
                            return UiAction::ToggleItem(t.id.clone(), item_id.clone());
                        }
                    }
                }
                // add a new checklist item to the task's ## Tasks section
                KeyCode::Char('n') => {
                    self.creating_item = true;
                    self.input.clear();
                }
                // edit the selected item's text (prefilled into the entry)
                KeyCode::Char('e') => {
                    if let Some(it) = items.get(self.item_sel.min(items.len().saturating_sub(1))) {
                        self.editing_item = true;
                        self.input = it.text.clone();
                    }
                }
                // delete the selected item (confirm with y)
                KeyCode::Char('d') => {
                    if !items.is_empty() {
                        self.deleting_item = true;
                    }
                }
                _ => {}
            }
            return UiAction::None;
        }
        // drilled into the focused project's git worktrees: j/k pick one, n adds
        // a worktree, space locks/unlocks it, d removes it, y copies its path;
        // h/Esc/q step back out to the task list (Ctrl-c still quits).
        if self.viewing_worktrees {
            // branch-name entry for a new worktree (a sub-mode): Enter creates it,
            // Esc cancels; everything else just edits `input`.
            if self.creating_worktree {
                match key.code {
                    KeyCode::Esc => {
                        self.creating_worktree = false;
                        self.input.clear();
                    }
                    KeyCode::Enter => {
                        self.creating_worktree = false;
                        let branch = self.input.trim().to_string();
                        self.input.clear();
                        if !branch.is_empty() {
                            if let Some(anchor) = self.worktree_anchor.clone() {
                                return UiAction::AddWorktree(anchor, branch);
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                    }
                    KeyCode::Char(c) => self.input.push(c),
                    _ => {}
                }
                return UiAction::None;
            }
            // confirm a worktree remove: only `y` proceeds; else cancel. The main
            // worktree is never a delete target (guarded when `d` is pressed).
            if self.deleting_worktree {
                self.deleting_worktree = false;
                let target = self
                    .worktrees
                    .get(self.worktree_sel)
                    .filter(|w| !w.is_main)
                    .map(|w| w.path.clone());
                if matches!(key.code, KeyCode::Char('y')) {
                    if let (Some(anchor), Some(path)) = (self.worktree_anchor.clone(), target) {
                        return UiAction::DeleteWorktree(anchor, path.display().to_string());
                    }
                }
                return UiAction::None;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return UiAction::Quit;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('q') => {
                    self.viewing_worktrees = false;
                    self.creating_worktree = false;
                    self.deleting_worktree = false;
                    self.input.clear();
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if !self.worktrees.is_empty() {
                        self.worktree_sel = (self.worktree_sel + 1).min(self.worktrees.len() - 1);
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.worktree_sel = self.worktree_sel.saturating_sub(1);
                }
                // add a worktree (prompts for a branch name)
                KeyCode::Char('n') => {
                    self.creating_worktree = true;
                    self.input.clear();
                }
                // remove the selected worktree (confirm with y); never the main one
                KeyCode::Char('d') => {
                    match self.worktrees.get(self.worktree_sel).map(|w| w.is_main) {
                        Some(true) => self.message = "can't remove the main worktree".into(),
                        Some(false) => self.deleting_worktree = true,
                        None => {}
                    }
                }
                // toggle the lock state of the selected worktree (not the main one)
                KeyCode::Char(' ') => {
                    let wt = self
                        .worktrees
                        .get(self.worktree_sel)
                        .map(|w| (w.is_main, w.path.clone(), w.locked));
                    if let Some((is_main, path, locked)) = wt {
                        if is_main {
                            self.message = "the main worktree can't be locked".into();
                        } else if let Some(anchor) = self.worktree_anchor.clone() {
                            return UiAction::LockWorktree(
                                anchor,
                                path.display().to_string(),
                                !locked,
                            );
                        }
                    }
                }
                // copy the selected worktree's path to the clipboard
                KeyCode::Char('y') => {
                    if let Some(path) = self.worktrees.get(self.worktree_sel).map(|w| w.path.clone())
                    {
                        return UiAction::CopyDir(path);
                    }
                }
                _ => {}
            }
            return UiAction::None;
        }
        // text entry for a new task title
        if self.creating {
            match key.code {
                KeyCode::Esc => {
                    self.creating = false;
                    self.input.clear();
                }
                KeyCode::Enter => {
                    self.creating = false;
                    let title = self.input.trim().to_string();
                    self.input.clear();
                    if !title.is_empty() {
                        return UiAction::New(title);
                    }
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(c) => self.input.push(c),
                _ => {}
            }
            return UiAction::None;
        }
        // pick a target state for the selected task; any other key cancels
        if self.moving {
            self.moving = false;
            let target = match key.code {
                KeyCode::Char('i') => Some(Status::Inbox),
                KeyCode::Char('a') => Some(Status::Active),
                KeyCode::Char('d') => Some(Status::Done),
                KeyCode::Char('c') => Some(Status::Canceled),
                _ => None,
            };
            if let Some(to) = target {
                if let Some((id, status)) =
                    self.selected_task().map(|t| (t.id.clone(), t.status))
                {
                    if status == to {
                        self.message = format!("already {}", to.as_str());
                    } else {
                        return UiAction::Move(id, to);
                    }
                }
            }
            return UiAction::None;
        }
        // confirm a destructive delete: only `y` proceeds; any other key cancels
        if self.deleting {
            self.deleting = false;
            if matches!(key.code, KeyCode::Char('y')) {
                if let Some(t) = self.selected_task() {
                    return UiAction::Delete(t.id.clone());
                }
            }
            return UiAction::None;
        }
        // pick how to claim the task: single / worktree / branch; any other key cancels
        if self.starting {
            self.starting = false;
            let mode = match key.code {
                KeyCode::Char('s') => Some(StartMode::Single),
                KeyCode::Char('w') => Some(StartMode::Worktree),
                KeyCode::Char('b') => Some(StartMode::Branch),
                _ => None,
            };
            if let Some(mode) = mode {
                if let Some(t) = self.selected_task() {
                    return UiAction::Start(t.id.clone(), mode);
                }
            }
            return UiAction::None;
        }
        // pick how to back the PR: a worktree (under the store) or a main-repo
        // branch; any other key cancels
        if self.pring {
            self.pring = false;
            let worktree = match key.code {
                KeyCode::Char('w') => Some(true),
                KeyCode::Char('b') => Some(false),
                _ => None,
            };
            if let Some(worktree) = worktree {
                if let Some(t) = self.selected_task() {
                    return UiAction::CreatePr(t.id.clone(), worktree);
                }
            }
            return UiAction::None;
        }
        // optional issue→project picker: row 0 files no project, rows 1.. each
        // file onto that board; Esc cancels issue creation entirely.
        if self.issuing {
            let len = self.issue_projects.len() + 1; // +1 for "no project"
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.issuing = false;
                    self.issue_target = None;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.issue_sel = (self.issue_sel + 1).min(len - 1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.issue_sel = self.issue_sel.saturating_sub(1);
                }
                KeyCode::Enter => {
                    self.issuing = false;
                    if let Some(id) = self.issue_target.take() {
                        let project = if self.issue_sel == 0 {
                            None
                        } else {
                            self.issue_projects.get(self.issue_sel - 1).cloned()
                        };
                        return UiAction::IssueWithProject(id, project);
                    }
                }
                _ => {}
            }
            return UiAction::None;
        }
        // hierarchical project level: pick which project scopes the task list
        if self.picking_project {
            let len = self.project_list().len() + 1; // +1 for "all projects"
            match key.code {
                KeyCode::Esc => self.picking_project = false,
                KeyCode::Char('q') => self.picking_project = false,
                KeyCode::Char('j') | KeyCode::Down => {
                    self.project_sel = (self.project_sel + 1).min(len - 1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.project_sel = self.project_sel.saturating_sub(1);
                }
                KeyCode::Enter => {
                    self.project_scope = if self.project_sel == 0 {
                        None
                    } else {
                        self.project_list().get(self.project_sel - 1).cloned()
                    };
                    self.picking_project = false;
                    self.selected = 0; // task selection is stale under a new scope
                }
                _ => {}
            }
            return UiAction::None;
        }
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.filtering = false;
                    self.filter.clear();
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.selected = 0;
                }
                _ => {}
            }
            return UiAction::None;
        }
        let visible_len = self.visible().len();
        match key.code {
            KeyCode::Char('q') => return UiAction::Quit,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return UiAction::Quit
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if visible_len > 0 {
                    self.selected = (self.selected + 1).min(visible_len - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Tab => {
                self.tab = (self.tab + 1) % TABS.len();
                self.selected = 0;
            }
            KeyCode::Char('/') => {
                self.filtering = true;
                self.filter.clear();
            }
            KeyCode::Esc => {
                self.filter.clear();
            }
            KeyCode::Enter => {
                if let Some(t) = self.selected_task() {
                    return UiAction::Edit(t.path.clone());
                }
            }
            KeyCode::Char('l') => match self.selected_task() {
                // drill into the task's checklist items (item list + per-item log)
                Some(t) if !task_items(&t.body).is_empty() => {
                    self.viewing_items = true;
                    self.item_sel = 0;
                }
                Some(_) => self.message = "no checklist items in this task".into(),
                None => self.message = "no task selected".into(),
            },
            KeyCode::Char('P') => {
                // open the project level with the cursor on the current scope
                self.project_sel = match &self.project_scope {
                    None => 0,
                    Some(p) => self
                        .project_list()
                        .iter()
                        .position(|n| n == p)
                        .map(|i| i + 1)
                        .unwrap_or(0),
                };
                self.picking_project = true;
            }
            KeyCode::Char('n') => {
                self.creating = true;
                self.input.clear();
            }
            KeyCode::Char('m') => {
                if self.selected_task().is_some() {
                    self.moving = true;
                } else {
                    self.message = "no task selected".into();
                }
            }
            KeyCode::Char('s') => match self.selected_task() {
                Some(t) if t.status == Status::Inbox => self.starting = true,
                Some(_) => {
                    self.message = "only inbox tasks can be started (use m to move)".into()
                }
                None => {}
            },
            KeyCode::Char('d') => {
                if let Some(t) = self.selected_task() {
                    if matches!(t.status, Status::Inbox | Status::Active) {
                        return UiAction::Done(t.id.clone());
                    }
                    self.message = "task is already finished (use m to reopen)".into();
                }
            }
            KeyCode::Char('D') => {
                if self.selected_task().is_some() {
                    self.deleting = true;
                } else {
                    self.message = "no task selected".into();
                }
            }
            KeyCode::Char('i') => match self.selected_task() {
                // already has an issue → push the managed section to it; otherwise
                // begin creating one (the event loop offers an optional project picker)
                Some(t) if t.github_issue.is_some() => return UiAction::PushIssue(t.id.clone()),
                Some(t) => return UiAction::Issue(t.id.clone()),
                None => self.message = "no task selected".into(),
            },
            KeyCode::Char('y') => match self.selected_task() {
                // yank the task's working directory (worktree, else the main repo)
                Some(t) => match t.work_dir() {
                    Some(dir) => return UiAction::CopyDir(dir),
                    None => self.message = "no working directory known for this task".into(),
                },
                None => self.message = "no task selected".into(),
            },
            KeyCode::Char('x') => match self.selected_task() {
                Some(t) if t.status == Status::Active => return UiAction::Run(t.id.clone()),
                Some(_) => self.message = "only active tasks can run (start it first)".into(),
                None => {}
            },
            KeyCode::Char('a') => match self.selected_task() {
                Some(t) => return UiAction::AttachRun(t.id.clone()),
                None => self.message = "no task selected".into(),
            },
            // summarize the task's checklist items into a title + Goal via the
            // configured LLM (the same `rein summary` path); needs items to work
            KeyCode::Char('S') => match self.selected_task() {
                Some(t) if !task_items(&t.body).is_empty() => return UiAction::Summary(t.id.clone()),
                Some(_) => self.message = "no checklist items to summarize".into(),
                None => self.message = "no task selected".into(),
            },
            KeyCode::Char('p') => match self.selected_task() {
                // already has a PR → push the managed section to it
                Some(t) if t.github_pr.is_some() => return UiAction::PushPr(t.id.clone()),
                Some(t) if matches!(t.status, Status::Inbox | Status::Active) => {
                    // a task already backed by a worktree/branch reuses it, so the
                    // w/b choice would be ignored — skip the prompt and open the PR.
                    // only ask when there's nothing set up yet (fresh inbox/active).
                    if t.branch.is_some() {
                        return UiAction::CreatePr(t.id.clone(), false);
                    }
                    self.pring = true;
                }
                Some(_) => self.message = "only inbox/active tasks can open a PR".into(),
                None => self.message = "no task selected".into(),
            },
            KeyCode::Char('w') => match self.selected_task() {
                // drill into the selected task's project's git worktrees (the
                // event loop lists them and opens the view)
                Some(t) => return UiAction::Worktrees(t.id.clone()),
                None => self.message = "no task selected".into(),
            },
            _ => {}
        }
        UiAction::None
    }

    pub fn render(&self, f: &mut Frame) {
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(f.area());
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(outer[0]);
        // right column: a small meta pane above the markdown preview
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(3)])
            .split(panes[1]);
        self.render_list(f, panes[0]);
        self.render_meta(f, right[0]);
        self.render_preview(f, right[1]);
        self.render_statusline(f, outer[1]);
        // overlays are mutually exclusive: the event loop clears `summarizing`
        // the moment it sets a popup, and a conflict arms `force_push` instead
        // of a popup, so at most one of these is ever set
        if let Some(s) = &self.summarizing {
            self.render_summarizing(f, s);
        } else if let Some(fp) = &self.force_push {
            self.render_force_push(f, fp);
        } else if let Some(msg) = &self.popup {
            self.render_popup(f, msg);
        }
    }

    /// A centered prompt offering to force-push after a sync conflict — `f`
    /// overwrites the remote with the local copy, any other key cancels.
    fn render_force_push(&self, f: &mut Frame, fp: &ForcePush) {
        let surface = match fp.surface {
            ForceSurface::Issue => "issue",
            ForceSurface::Pr => "PR",
        };
        let area = centered_rect(60, 35, f.area());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" sync conflict ");
        let body = format!(
            "conflict on the {} for '{}': local and remote both changed since the last sync. Backups are in conflicts/.\n\n\
             press f to force-push — overwrite the remote with your local copy\n\
             any other key to cancel (resolve locally, then retry)",
            surface, fp.slug
        );
        let para = Paragraph::new(body)
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: true })
            .block(block);
        f.render_widget(Clear, area);
        f.render_widget(para, area);
    }

    /// A centered "summarizing…" overlay with an animated spinner and an
    /// elapsed-seconds readout, shown while the `rein summary` worker runs so a
    /// slow LLM reads as working rather than frozen.
    fn render_summarizing(&self, f: &mut Frame, s: &Summarizing) {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = FRAMES[self.spinner_frame % FRAMES.len()];
        let secs = s.started.elapsed().as_secs();
        let area = centered_rect(50, 20, f.area());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" summary ");
        let body = format!(
            "{} summarizing {} … ({}s)\n\nasking the LLM to write title + Goal from the items\nCtrl-c to quit",
            spin, s.slug, secs
        );
        let para = Paragraph::new(body)
            .style(Style::default().fg(Color::Cyan))
            .wrap(Wrap { trim: true })
            .block(block);
        f.render_widget(Clear, area);
        f.render_widget(para, area);
    }

    /// A compact pane above the preview showing the selected task's frontmatter
    /// (id, branch, issue/PR numbers, timestamps, tags) — data the body doesn't show.
    fn render_meta(&self, f: &mut Frame, area: Rect) {
        let block = Block::default().borders(Borders::ALL).title(" meta ");
        let lines = match self.selected_task() {
            Some(t) => meta_lines(t),
            None => vec![Line::from("—")],
        };
        f.render_widget(Paragraph::new(lines).block(block), area);
    }

    /// A centered modal over the dashboard for predictable failures (e.g. a
    /// branch that already exists), dismissed with any key.
    fn render_popup(&self, f: &mut Frame, msg: &str) {
        let area = centered_rect(60, 40, f.area());
        let (color, title) = if self.popup_error {
            (Color::Red, " error — press any key to dismiss ")
        } else {
            (Color::Cyan, " run — press any key to dismiss ")
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .title(title);
        let para = Paragraph::new(msg.to_string())
            .wrap(Wrap { trim: true })
            .block(block);
        f.render_widget(Clear, area);
        f.render_widget(para, area);
    }

    fn render_list(&self, f: &mut Frame, area: Rect) {
        if self.viewing_worktrees {
            self.render_worktree_list(f, area);
        } else if self.viewing_items {
            self.render_item_list(f, area);
        } else if self.issuing {
            self.render_issue_picker(f, area);
        } else if self.picking_project {
            self.render_project_picker(f, area);
        } else {
            self.render_task_list(f, area);
        }
    }

    /// The drilled-in task's checklist items with their checkbox state. The
    /// cursor (j/k) selects one; its Agent-Log entries fill the preview and
    /// `space` toggles it. Mirrors the project/issue pickers (replaces the list).
    fn render_item_list(&self, f: &mut Frame, area: Rect) {
        let items = self
            .selected_task()
            .map(|t| task_items(&t.body))
            .unwrap_or_default();
        let title = match self.selected_task() {
            Some(t) => format!(" items · {} ", t.slug),
            None => " items ".to_string(),
        };
        if items.is_empty() {
            let p = Paragraph::new("no checklist items")
                .block(Block::default().borders(Borders::ALL).title(title));
            f.render_widget(p, area);
            return;
        }
        let sel = self.item_sel.min(items.len() - 1);
        let list_items: Vec<ListItem> = items
            .iter()
            .enumerate()
            .map(|(row, it)| {
                let (mark, color) = if it.failed {
                    ("[✗]", Color::Red)
                } else if it.checked {
                    ("[x]", Color::Rgb(0, 128, 0))
                } else {
                    ("[ ]", Color::Yellow)
                };
                let cursor = if row == sel { "▶ " } else { "  " };
                let mut text_style = Style::default();
                if row == sel {
                    text_style = text_style.add_modifier(Modifier::BOLD);
                }
                if it.failed {
                    text_style = text_style.add_modifier(Modifier::CROSSED_OUT);
                }
                ListItem::new(Line::from(vec![
                    Span::raw(cursor),
                    Span::styled(format!("{} ", mark), Style::default().fg(color)),
                    Span::styled(it.text.clone(), text_style),
                ]))
            })
            .collect();
        let list = List::new(list_items)
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(list, area);
    }

    /// Preview pane while drilled into items: the Agent-Log entries that
    /// reference the selected item (by the `Task<id>` convention the run skill
    /// uses), or a hint when none mention it.
    fn render_item_log(&self, f: &mut Frame, area: Rect) {
        let (lines, title) = match self.selected_task() {
            Some(t) => {
                let items = task_items(&t.body);
                if items.is_empty() {
                    (vec![Line::from("no checklist items")], " log ".to_string())
                } else {
                    let it = &items[self.item_sel.min(items.len() - 1)];
                    let id = it.id.as_deref().unwrap_or("-");
                    let title = format!(" log · item {} ", id);
                    let logs = item_log_lines(&t.body, id);
                    if logs.is_empty() {
                        (
                            vec![Line::styled(
                                format!("no Agent Log entries reference Task{}", id),
                                Style::default().fg(Color::DarkGray),
                            )],
                            title,
                        )
                    } else {
                        (render_markdown(&logs.join("\n")), title)
                    }
                }
            }
            None => (vec![Line::from("no task selected")], " log ".to_string()),
        };
        let p = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
    }

    /// The focused project's git worktrees: branch (or detached/bare) + the
    /// directory name, with `main`/`locked` flags. The cursor (j/k) selects one;
    /// its details fill the preview. Replaces the task list (like the pickers).
    fn render_worktree_list(&self, f: &mut Frame, area: Rect) {
        let title = format!(" worktrees · {} ", self.worktree_project);
        if self.worktrees.is_empty() {
            let p = Paragraph::new("no worktrees")
                .block(Block::default().borders(Borders::ALL).title(title));
            f.render_widget(p, area);
            return;
        }
        let sel = self.worktree_sel.min(self.worktrees.len() - 1);
        let items: Vec<ListItem> = self
            .worktrees
            .iter()
            .enumerate()
            .map(|(row, w)| {
                let cursor = if row == sel { "▶ " } else { "  " };
                let (label, label_color) = if w.bare {
                    ("(bare)".to_string(), Color::DarkGray)
                } else if let Some(b) = &w.branch {
                    (b.clone(), Color::Cyan)
                } else {
                    ("(detached)".to_string(), Color::DarkGray)
                };
                let name = w
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_else(|| w.path.to_str().unwrap_or("?"));
                let mut text_style = Style::default();
                if row == sel {
                    text_style = text_style.add_modifier(Modifier::BOLD);
                }
                let mut spans = vec![
                    Span::raw(cursor),
                    Span::styled(format!("{} ", label), Style::default().fg(label_color)),
                    Span::styled(format!("— {}", name), text_style),
                ];
                if w.is_main {
                    spans.push(Span::styled(" [main]", Style::default().fg(Color::Magenta)));
                }
                if w.locked {
                    spans.push(Span::styled(" [locked]", Style::default().fg(Color::Yellow)));
                }
                if w.prunable {
                    spans.push(Span::styled(" [prunable]", Style::default().fg(Color::Red)));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(list, area);
    }

    /// Preview pane while drilled into worktrees: the selected worktree's full
    /// path, branch/HEAD and flags — the read view of the CRUD.
    fn render_worktree_detail(&self, f: &mut Frame, area: Rect) {
        let dim = Style::default().fg(Color::DarkGray);
        let lines: Vec<Line> = match self.worktrees.get(self.worktree_sel.min(self.worktrees.len().saturating_sub(1))) {
            Some(w) => {
                let branch = if w.bare {
                    "(bare)".to_string()
                } else {
                    w.branch.clone().unwrap_or_else(|| "(detached)".to_string())
                };
                let head = w
                    .head
                    .as_deref()
                    .map(|h| h[..h.len().min(12)].to_string())
                    .unwrap_or_else(|| "—".to_string());
                let mut flags: Vec<&str> = Vec::new();
                if w.is_main {
                    flags.push("main");
                }
                if w.locked {
                    flags.push("locked");
                }
                if w.prunable {
                    flags.push("prunable");
                }
                let flags = if flags.is_empty() { "—".to_string() } else { flags.join(", ") };
                vec![
                    Line::from(vec![Span::styled("path:   ", dim), Span::raw(w.path.display().to_string())]),
                    Line::from(vec![Span::styled("branch: ", dim), Span::raw(branch)]),
                    Line::from(vec![Span::styled("HEAD:   ", dim), Span::raw(head)]),
                    Line::from(vec![Span::styled("flags:  ", dim), Span::raw(flags)]),
                ]
            }
            None => vec![Line::from("no worktrees")],
        };
        let p = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" worktree "));
        f.render_widget(p, area);
    }

    /// The optional issue→project picker: "no project" plus each GitHub Project
    /// the new issue can be filed onto. Mirrors the project-scope picker.
    fn render_issue_picker(&self, f: &mut Frame, area: Rect) {
        let mut entries: Vec<String> = vec!["— no project —".to_string()];
        entries.extend(self.issue_projects.iter().cloned());
        let sel = self.issue_sel.min(entries.len().saturating_sub(1));
        let items: Vec<ListItem> = entries
            .iter()
            .enumerate()
            .map(|(row, name)| {
                let marker = if row == sel { "▶ " } else { "  " };
                let style = if row == sel {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::raw(marker),
                    Span::styled(name.clone(), style.fg(Color::Magenta)),
                ]))
            })
            .collect();
        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" file issue onto project — Enter to confirm · Esc cancel "),
        );
        f.render_widget(list, area);
    }

    /// The project level of the hierarchy: every project with its task count,
    /// plus an "all projects" aggregate. Selecting one scopes the task list.
    fn render_project_picker(&self, f: &mut Frame, area: Rect) {
        let projects = self.project_list();
        let count = |scope: Option<&str>| {
            self.tasks
                .iter()
                .filter(|t| scope.is_none_or(|p| t.project == p))
                .count()
        };
        let mut entries: Vec<(String, usize)> = vec![("all projects".to_string(), count(None))];
        entries.extend(projects.iter().map(|p| (p.clone(), count(Some(p)))));

        let sel = self.project_sel.min(entries.len().saturating_sub(1));
        let items: Vec<ListItem> = entries
            .iter()
            .enumerate()
            .map(|(row, (name, n))| {
                let marker = if row == sel { "▶ " } else { "  " };
                let style = if row == sel {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::raw(marker),
                    Span::styled(format!("{} ", name), Style::default().fg(Color::Magenta)),
                    Span::styled(format!("({})", n), style),
                ]))
            })
            .collect();
        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" projects — Enter to select · Esc cancel "),
        );
        f.render_widget(list, area);
    }

    fn render_task_list(&self, f: &mut Frame, area: Rect) {
        let vis = self.visible();
        let items: Vec<ListItem> = vis
            .iter()
            .enumerate()
            .map(|(row, &i)| {
                let t = &self.tasks[i];
                let marker = if row == self.selected.min(vis.len().saturating_sub(1)) {
                    "▶ "
                } else {
                    "  "
                };
                let style = if row == self.selected.min(vis.len().saturating_sub(1)) {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let mut spans = vec![
                    Span::raw(marker),
                    Span::styled(
                        format!("[{}] ", t.status.as_str()),
                        Style::default().fg(status_color(t.status)),
                    ),
                ];
                // a project tag is redundant once a single project is scoped
                if self.project_scope.is_none() && !t.project.is_empty() {
                    spans.push(Span::styled(
                        format!("{} ", t.project),
                        Style::default().fg(Color::Magenta),
                    ));
                }
                spans.push(Span::styled(format!("{} — {}", t.slug, t.title), style));
                // a live background run gets a green dot; other states stay quiet
                if t.run_state.as_deref() == Some("working") {
                    spans.push(Span::styled(" ●", Style::default().fg(Color::Green)));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let title = format!(
            " tasks [{} · {}] {} ",
            self.scope_name(),
            self.tab_name(),
            if self.filter.is_empty() {
                String::new()
            } else {
                format!("filter: {}", self.filter)
            }
        );
        let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(list, area);
    }

    fn render_preview(&self, f: &mut Frame, area: Rect) {
        if self.viewing_worktrees {
            self.render_worktree_detail(f, area);
            return;
        }
        if self.viewing_items {
            self.render_item_log(f, area);
            return;
        }
        let lines: Vec<Line> = match self.selected_task() {
            Some(t) => render_markdown(&t.body),
            None => vec![Line::from("no task selected")],
        };
        let preview = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" preview "));
        f.render_widget(preview, area);
    }

    fn render_statusline(&self, f: &mut Frame, area: Rect) {
        let text = if self.creating_item {
            let slug = self.selected_task().map(|t| t.slug.as_str()).unwrap_or("");
            format!("new item [{}]: {} · Enter add · Esc cancel", slug, self.input)
        } else if self.editing_item {
            let slug = self.selected_task().map(|t| t.slug.as_str()).unwrap_or("");
            format!("edit item [{}]: {} · Enter save · Esc cancel", slug, self.input)
        } else if self.deleting_item {
            let slug = self.selected_task().map(|t| t.slug.as_str()).unwrap_or("");
            format!("delete item from {}? [y]es · any other key cancels", slug)
        } else if self.creating {
            let proj = self
                .selected_task()
                .map(|t| t.project.clone())
                .filter(|p| !p.is_empty())
                .or_else(|| self.home_label.clone())
                .unwrap_or_else(|| "?".into());
            format!("new task [{}]: {}", proj, self.input)
        } else if self.moving {
            let slug = self.selected_task().map(|t| t.slug.as_str()).unwrap_or("");
            format!(
                "move {} to: [i]nbox [a]ctive [d]one [c]anceled · Esc cancel",
                slug
            )
        } else if self.deleting {
            let slug = self.selected_task().map(|t| t.slug.as_str()).unwrap_or("");
            format!("delete {} permanently? [y]es · any other key cancels", slug)
        } else if self.starting {
            let slug = self.selected_task().map(|t| t.slug.as_str()).unwrap_or("");
            format!(
                "start {}: [s]ingle [w]orktree [b]ranch · any other key cancels",
                slug
            )
        } else if self.pring {
            let slug = self.selected_task().map(|t| t.slug.as_str()).unwrap_or("");
            format!(
                "PR for {}: [w]orktree [b]ranch · any other key cancels",
                slug
            )
        } else if self.viewing_items {
            let slug = self.selected_task().map(|t| t.slug.as_str()).unwrap_or("");
            format!(
                "items {} · j/k move · space toggle · n new · e edit · d delete · h/Esc/q back",
                slug
            )
        } else if self.creating_worktree {
            format!("new worktree branch: {} · Enter create · Esc cancel", self.input)
        } else if self.deleting_worktree {
            let name = self
                .worktrees
                .get(self.worktree_sel)
                .and_then(|w| w.path.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("");
            format!("remove worktree {}? [y]es · any other key cancels", name)
        } else if self.viewing_worktrees {
            format!(
                "worktrees {} · j/k move · n new · space lock · d remove · y copy · h/Esc/q back",
                self.worktree_project
            )
        } else if self.issuing {
            "j/k pick project · Enter file issue · Esc cancel".to_string()
        } else if self.picking_project {
            "j/k move · Enter select project · Esc cancel".to_string()
        } else if self.filtering {
            format!("/{}", self.filter)
        } else if !self.message.is_empty() {
            self.message.clone()
        } else {
            "j/k · Tab · P project · Enter edit · l items · n new · s start · m move · d done · D delete · x run · a attach · S summary · i issue · p PR · y copy dir · w worktrees · / · q quit"
                .to_string()
        };
        // a readable light gray so the hints stand out (the old dark gray was
        // too dim against most terminal backgrounds)
        f.render_widget(Paragraph::new(text).style(Style::default().fg(Color::Gray)), area);
    }
}

/// Render the selected task's frontmatter as compact lines for the meta pane.
fn meta_lines(t: &TaskRow) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let dash = || "—".to_string();
    let date = |s: &str| s.get(..10).unwrap_or(s).to_string();
    let issue = t.github_issue.map(|n| format!("#{}", n)).unwrap_or_else(dash);
    let pr = t.github_pr.map(|n| format!("#{}", n)).unwrap_or_else(dash);
    let tags = if t.tags.is_empty() { dash() } else { t.tags.join(", ") };
    let (run_txt, run_color) = run_state_label(t.run_state.as_deref());
    // branch + how it's backed: an isolated worktree vs a plain main-repo branch
    let branch_txt = match (&t.branch, t.is_worktree()) {
        (Some(b), true) => format!("{} (worktree)", b),
        (Some(b), false) => format!("{} (branch)", b),
        (None, _) => dash(),
    };
    let dir_txt = t
        .work_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(dash);
    vec![
        Line::from(Span::styled(
            t.id.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("branch: ", dim),
            Span::raw(branch_txt),
            Span::styled("   run: ", dim),
            Span::styled(run_txt, Style::default().fg(run_color)),
        ]),
        Line::from(vec![Span::styled("dir: ", dim), Span::raw(dir_txt)]),
        Line::from(vec![
            Span::styled("issue: ", dim),
            Span::raw(issue),
            Span::styled("   PR: ", dim),
            Span::raw(pr),
            Span::raw(if t.shared { "   shared" } else { "" }.to_string()),
        ]),
        Line::from(vec![
            Span::styled("created ", dim),
            Span::raw(date(&t.created_at)),
            Span::styled("  updated ", dim),
            Span::raw(date(&t.updated_at)),
            Span::styled("  tags: ", dim),
            Span::raw(tags),
        ]),
    ]
}

/// Human label + color for a background-session state.
fn run_state_label(state: Option<&str>) -> (String, Color) {
    match state {
        Some("working") => ("running".into(), Color::Green),
        Some("done") => ("done".into(), Color::Blue),
        Some("failed") => ("failed".into(), Color::Red),
        Some("blocked") => ("blocked".into(), Color::Yellow),
        Some("stopped") => ("stopped".into(), Color::DarkGray),
        Some(other) => (other.to_string(), Color::Gray),
        None => ("—".into(), Color::DarkGray),
    }
}

/// A `Rect` centered in `area`, sized to the given percentage of width/height.
fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vert[1])[1]
}

fn status_color(s: Status) -> Color {
    match s {
        Status::Inbox => Color::Yellow,
        Status::Active => Color::Green,
        Status::Done => Color::Blue,
        Status::Canceled => Color::DarkGray,
    }
}

/// Minimal line-based markdown styling (no in-TUI editing by design).
pub fn render_markdown(body: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut in_code = false;
    for raw in body.lines() {
        let line = raw.to_string();
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            out.push(Line::styled(line, Style::default().fg(Color::DarkGray)));
            continue;
        }
        if in_code {
            out.push(Line::styled(line, Style::default().fg(Color::Gray)));
        } else if line.starts_with("## ") {
            out.push(Line::styled(
                line,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
        } else if line.contains(crate::task::FAILED_SENTINEL) {
            // resolved-failed item: red + struck through (it carries a checked
            // box, so this branch must precede the `- [x]` one below)
            out.push(Line::styled(
                line,
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::CROSSED_OUT),
            ));
        } else if line.trim_start().starts_with("- [x]") || line.trim_start().starts_with("- [X]") {
            // completed item: a deep, theme-independent green so it stands clearly
            // apart from the yellow open-item color (ANSI green reads olive on some themes)
            out.push(Line::styled(line, Style::default().fg(Color::Rgb(0, 128, 0))));
        } else if line.trim_start().starts_with("- [ ]") {
            out.push(Line::styled(line, Style::default().fg(Color::Yellow)));
        } else {
            out.push(Line::from(line));
        }
    }
    out
}

/// Checklist items of a task body, with stable integer IDs assigned in-memory
/// the same way `assign_ids` persists them — so the id shown in the item view
/// matches the id a toggle will write back to disk.
fn task_items(body: &str) -> Vec<crate::task::Item> {
    let (assigned, _) = crate::task::ensure_item_ids(body);
    crate::task::scan_items(&assigned)
}

/// Agent-Log lines that reference the given item id by the `Task<id>` convention
/// the run skill follows (`- <ts> Task7: …`). Case-insensitive; the id must be a
/// whole token so `Task1` never picks up a `Task10` entry.
fn item_log_lines(body: &str, item_id: &str) -> Vec<String> {
    crate::task::log_section(body)
        .unwrap_or_default()
        .lines()
        .filter(|l| log_mentions_item(l, item_id))
        .map(|l| l.to_string())
        .collect()
}

/// Whether `line` mentions `Task<id>` as a standalone token (digits on either
/// side of the id rule it out, so item `1` and item `10` never cross-match).
fn log_mentions_item(line: &str, id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    let lower = line.to_lowercase();
    let needle = format!("task{}", id.to_lowercase());
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(&needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after_ok = lower[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

// ---------------------------------------------------------------------------
// Interactive loops (thin view + dispatcher over the CLI verbs)
// ---------------------------------------------------------------------------

fn load_all_rows(projects: &[StoreInfo]) -> Vec<TaskRow> {
    let mut out = Vec::new();
    // (row index, Claude session id) for tasks that have been `rein run`
    let mut claude_sessions: Vec<(usize, String)> = Vec::new();
    for p in projects {
        for t in p.store.list_tasks() {
            let st = crate::state::load(&p.store, &t.id);
            let mut row = TaskRow::from_ref(&t, &p.project, &p.store.root);
            if let Some(id) = st.run_session.clone() {
                if st.run_agent.as_deref() == Some("codex") {
                    row.run_state = crate::commands::exec::codex_status_from_state(&st);
                } else {
                    claude_sessions.push((out.len(), id));
                }
            }
            row.worktree = st.worktree.clone();
            row.repo_dir = p.repo_dir.clone();
            out.push(row);
        }
    }
    // one `claude agents` query covers every project; skip it when nothing ran
    if !claude_sessions.is_empty() {
        let states = run_states();
        for (i, id) in claude_sessions {
            out[i].run_state = states.get(&id).cloned();
        }
    }
    out
}

/// Map of background session id → state (`working`/`done`/`failed`/…) from
/// `claude agents --json`. Best-effort: an empty map if claude is absent or errors.
fn run_states() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let bin = std::env::var("REIN_CLAUDE").unwrap_or_else(|_| "claude".to_string());
    let Ok(out) = std::process::Command::new(bin)
        .args(["agents", "--json", "--all"])
        .output()
    else {
        return map;
    };
    if !out.status.success() {
        return map;
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return map;
    };
    let arr = v
        .as_array()
        .cloned()
        .or_else(|| v.get("sessions").and_then(|s| s.as_array()).cloned())
        .unwrap_or_default();
    for s in arr {
        if let (Some(id), Some(state)) = (
            s.get("id").and_then(|x| x.as_str()),
            s.get("state").and_then(|x| x.as_str()),
        ) {
            map.insert(id.to_string(), state.to_string());
        }
    }
    map
}

/// Locate a task by id across every project, reading fresh from disk.
fn find_task<'a>(projects: &'a [StoreInfo], id: &str) -> Option<(&'a StoreInfo, TaskRef)> {
    projects
        .iter()
        .find_map(|p| p.store.find_by_id(id).map(|t| (p, t)))
}

/// Locate a task by its document path across every project.
fn find_task_by_path<'a>(projects: &'a [StoreInfo], path: &Path) -> Option<(&'a StoreInfo, TaskRef)> {
    projects.iter().find_map(|p| {
        p.store
            .list_tasks()
            .into_iter()
            .find(|t| t.path == path)
            .map(|t| (p, t))
    })
}

/// Build a full command context (store + repo) for a project's git/gh actions.
/// Errors clearly if the repo has moved or vanished.
fn ctx_for(info: &StoreInfo) -> Result<Ctx> {
    let dir = info
        .repo_dir
        .clone()
        .with_context(|| format!("project '{}' has no recorded repo path", info.project))?;
    let repo = Repo::discover(&dir)
        .with_context(|| format!("repo for '{}' not found at {}", info.project, dir.display()))?;
    Ok(Ctx {
        repo,
        store: info.store.clone(),
    })
}

/// The repo owner (user/org login) from the git remote, used to scope the
/// GitHub Project picker to the right account. `None` if no remote is set.
fn repo_owner(ctx: &Ctx) -> Option<String> {
    let remote = ctx.repo.remote_url()?;
    let tail = remote
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git");
    // ssh: git@host:owner/repo  |  https: https://host/owner/repo
    let path = tail.rsplit_once(':').map(|(_, p)| p).unwrap_or(tail);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    (segs.len() >= 2).then(|| segs[segs.len() - 2].to_string())
}

/// Copy `text` to the system clipboard. Tries the common CLIs in turn so it
/// works on macOS (pbcopy) and Linux (wl-copy / xclip / xsel); errors if none
/// is available or the write fails.
fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let candidates: [(&str, &[&str]); 4] = [
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    let mut last_err: Option<anyhow::Error> = None;
    for (bin, args) in candidates {
        let spawned = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(anyhow!("{}: {}", bin, e));
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        if child.wait()?.success() {
            return Ok(());
        }
        last_err = Some(anyhow!("{} exited with an error", bin));
    }
    Err(last_err.unwrap_or_else(|| anyhow!("no clipboard tool found (pbcopy/wl-copy/xclip/xsel)")))
}

pub fn run() -> Result<()> {
    // The dashboard spans projects: the launch repo (if any) plus every other
    // store under <data home>/rein/, so it works from anywhere — even outside a repo.
    let home = Ctx::load().ok();
    let home_info = home.as_ref().map(|c| c.store.info());
    let mut projects: Vec<StoreInfo> = Vec::new();
    if let Some(hi) = &home_info {
        projects.push(hi.clone());
    }
    for info in Store::discover_all() {
        if !projects.iter().any(|p| p.store.root == info.store.root) {
            projects.push(info);
        }
    }
    if projects.is_empty() {
        anyhow::bail!("no rein stores found — run `rein init` in a project first");
    }

    let mut app = App::new(load_all_rows(&projects));
    app.all_projects = {
        let mut names: Vec<String> = projects.iter().map(|p| p.project.clone()).collect();
        names.sort();
        names.dedup();
        names
    };
    app.home_store_root = home_info.as_ref().map(|h| h.store.root.clone());
    app.home_label = home_info.as_ref().map(|h| h.project.clone());
    // launched inside a project → land scoped to it; the picker (P) goes wider
    app.project_scope = home_info.as_ref().map(|h| h.project.clone());

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // EnableFocusChange asks the terminal to report focus in/out. nvim forwards
    // these to a terminal buffer, so when another float (e.g. a claude-code
    // toggle) is layered over our window and then closed, we get a FocusGained
    // and force a full repaint — fixing the stale/left-shifted grid that hides
    // the borders and the ▶ marker.
    execute!(stdout, EnterAlternateScreen, EnableFocusChange)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = event_loop(&mut terminal, &mut app, &projects);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableFocusChange, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

/// Suspend the TUI while $EDITOR owns the terminal, then restore it.
fn open_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    path: &Path,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    let res = crate::commands::local::edit_file(path);
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    res
}

/// Suspend the dashboard while the agent's native interactive UI owns the terminal.
fn open_agent_session(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    attach: &crate::commands::exec::AttachCommand,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    let res = std::process::Command::new(&attach.program)
        .args(&attach.args)
        .current_dir(&attach.dir)
        .status()
        .with_context(|| format!("failed to launch {}", attach.program))
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                anyhow::bail!("{} exited with {}", attach.program, status)
            }
        });
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    res
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    projects: &[StoreInfo],
) -> Result<()> {
    let mut idle_ticks = 0u32;
    loop {
        // pump an in-flight summary worker: surface its result the moment it
        // lands, replacing the spinner overlay with the result/error popup
        if let Some(s) = app.summarizing.as_ref() {
            match s.rx.try_recv() {
                Ok(result) => {
                    app.summarizing = None;
                    match result {
                        Ok(msg) => {
                            app.popup = Some(msg);
                            app.popup_error = false;
                        }
                        Err(e) => {
                            app.popup = Some(format!("{:#}", e));
                            app.popup_error = true;
                        }
                    }
                    // reload so the freshly written title/Goal show in list + preview
                    app.tasks = load_all_rows(projects);
                    terminal.clear()?;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // worker vanished without sending (shouldn't happen) — don't hang
                    app.summarizing = None;
                    app.popup = Some("summary ended unexpectedly".into());
                    app.popup_error = true;
                    terminal.clear()?;
                }
            }
        }
        terminal.draw(|f| app.render(f))?;
        // poll faster while summarizing so the spinner stays smooth and the
        // result surfaces promptly; otherwise the calm 250ms idle cadence
        let timeout = if app.summarizing.is_some() { 80 } else { 250 };
        if !event::poll(std::time::Duration::from_millis(timeout))? {
            // animate the summary spinner on its own timer (don't also run the
            // background-run refresh below — that reload would stutter the spin)
            if app.summarizing.is_some() {
                app.spinner_frame = app.spinner_frame.wrapping_add(1);
                continue;
            }
            // while a background run is live, re-poll backend state every ~4s so
            // running → done/failed updates without the user touching anything
            idle_ticks += 1;
            let running = app
                .tasks
                .iter()
                .any(|t| t.run_state.as_deref() == Some("working"));
            if running && idle_ticks >= 16 {
                idle_ticks = 0;
                app.tasks = load_all_rows(projects);
            }
            continue;
        }
        idle_ticks = 0;
        let action = match event::read()? {
            Event::Key(key) => app.on_key(key),
            // Regaining focus or a resize can leave a stale/left-shifted grid
            // when another float was layered over us and closed; ratatui only
            // diff-renders, so force a full repaint to restore the borders + ▶
            // marker. (autoresize alone won't help: if the size ends unchanged
            // it thinks the screen already matches.)
            Event::FocusGained | Event::Resize(_, _) => {
                terminal.clear()?;
                continue;
            }
            _ => continue,
        };
        // no-op keys (navigation, typing) just update state → redraw next loop
        if action == UiAction::None {
            continue;
        }
        match action {
            UiAction::None => {}
            UiAction::Quit => return Ok(()),
            UiAction::Edit(path) => {
                if let Err(e) = open_editor(terminal, &path) {
                    app.message = format!("edit failed: {}", e);
                }
                heal_ids(projects, &path);
                app.tasks = load_all_rows(projects);
            }
            UiAction::New(title) => {
                // create in the selected task's project, else the launch repo
                let target = app
                    .selected_task()
                    .map(|t| t.store_root.clone())
                    .or_else(|| app.home_store_root.clone());
                match target {
                    Some(root) => {
                        let store = Store { root };
                        match crate::commands::local::create_task(&store, &title, false) {
                            Ok((_, path)) => {
                                if let Err(e) = open_editor(terminal, &path) {
                                    app.message = format!("edit failed: {}", e);
                                }
                                heal_ids(projects, &path);
                                app.tab = 0; // show the new inbox task
                                app.filter.clear();
                                app.tasks = load_all_rows(projects);
                                select_path(app, &path);
                            }
                            Err(e) => {
                                app.popup = Some(format!("{:#}", e));
                                app.popup_error = true;
                            }
                        }
                    }
                    None => {
                        app.message =
                            "no project to create in — run `rein init` in a repo first".into()
                    }
                }
            }
            UiAction::Start(id, mode) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info).and_then(|ctx| match mode {
                        StartMode::Single => {
                            crate::commands::exec::start(&ctx, &task.slug, false, None, false)
                        }
                        StartMode::Worktree => {
                            crate::commands::exec::start(&ctx, &task.slug, true, None, false)
                        }
                        StartMode::Branch => {
                            crate::commands::exec::start(&ctx, &task.slug, false, Some(&task.slug), false)
                        }
                    }),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
            }
            UiAction::Move(id, to) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => {
                        crate::commands::exec::relocate(&info.store, &task, to).map(|_| ())
                    }
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
            }
            UiAction::Done(id) => {
                let r = match find_task(projects, &id) {
                    Some((info, _)) => {
                        ctx_for(info).and_then(|ctx| crate::commands::exec::done(&ctx, Some(&id), false))
                    }
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
            }
            UiAction::Delete(id) => {
                // force=false so a dirty worktree blocks deletion with a clear
                // popup (mirrors `cancel`); the `D`→`y` confirm is the safety gate
                // for the common case of discarding inbox/draft tasks.
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info)
                        .and_then(|ctx| crate::commands::exec::delete(&ctx, &task.slug, false)),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
            }
            UiAction::Issue(id) => {
                // begin issue creation: offer an optional project picker, but only
                // if the owner actually has Projects — otherwise create straight away
                match find_task(projects, &id).and_then(|(info, task)| {
                    ctx_for(info).ok().map(|ctx| (ctx, task))
                }) {
                    Some((ctx, task)) => {
                        let owner = repo_owner(&ctx);
                        let gh = crate::gh::Gh::in_dir(&ctx.repo.workdir);
                        let names = gh.project_titles(owner.as_deref());
                        if names.is_empty() {
                            let r = crate::commands::sync_cmd::issue(&ctx, &task.slug, None);
                            finish(app, projects, r);
                        } else {
                            app.issue_projects = names;
                            app.issue_target = Some(id);
                            app.issue_sel = 0;
                            app.issuing = true;
                        }
                    }
                    None => finish(app, projects, Err(anyhow!("task '{}' vanished", id))),
                }
            }
            UiAction::IssueWithProject(id, project) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info).and_then(|ctx| {
                        crate::commands::sync_cmd::issue(&ctx, &task.slug, project.as_deref())
                    }),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
            }
            UiAction::PushIssue(id) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info)
                        .and_then(|ctx| crate::commands::sync_cmd::push_issue(&ctx, &task, false)),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish_push(app, projects, r, &id, ForceSurface::Issue);
            }
            UiAction::PushPr(id) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info)
                        .and_then(|ctx| crate::commands::sync_cmd::push_pr(&ctx, &task, false)),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish_push(app, projects, r, &id, ForceSurface::Pr);
            }
            UiAction::ForcePush(id, surface) => {
                // the user confirmed `f` after a conflict: re-push with force, the
                // single-surface `rein push --resolved` (overwrites the remote)
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info).and_then(|ctx| match surface {
                        ForceSurface::Issue => crate::commands::sync_cmd::push_issue(&ctx, &task, true),
                        ForceSurface::Pr => crate::commands::sync_cmd::push_pr(&ctx, &task, true),
                    }),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
            }
            UiAction::CopyDir(dir) => {
                let path = dir.display().to_string();
                match copy_to_clipboard(&path) {
                    Ok(()) => app.message = format!("copied {}", path),
                    Err(e) => {
                        app.popup = Some(format!("clipboard copy failed: {:#}", e));
                        app.popup_error = true;
                    }
                }
            }
            UiAction::CreatePr(id, worktree) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info)
                        .and_then(|ctx| crate::commands::exec::create_pr(&ctx, Some(&task.slug), worktree)),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
            }
            UiAction::Run(id) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info)
                        .and_then(|ctx| crate::commands::exec::run(&ctx, Some(&task.slug))),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                // show the session id / local log hints in a popup, not
                // via stdout (raw mode would garble the dashboard)
                match r {
                    Ok(msg) => {
                        app.popup = Some(msg);
                        app.popup_error = false;
                    }
                    Err(e) => {
                        app.popup = Some(format!("{:#}", e));
                        app.popup_error = true;
                    }
                }
                app.tasks = load_all_rows(projects);
            }
            UiAction::AttachRun(id) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info).and_then(|ctx| {
                        let attach = crate::commands::exec::attach_command(&ctx, Some(&task.slug))?;
                        open_agent_session(terminal, &attach)
                    }),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
            }
            UiAction::Summary(id) => {
                // the LLM call can take many seconds; run it on a worker thread so
                // the dashboard keeps animating a spinner instead of freezing. The
                // event loop polls the channel each tick and swaps in the result
                // popup when it lands (stdio is piped, so it won't garble the TUI).
                match find_task(projects, &id) {
                    Some((info, task)) => match ctx_for(info) {
                        Ok(ctx) => {
                            let slug = task.slug.clone();
                            let (tx, rx) = std::sync::mpsc::channel();
                            std::thread::spawn(move || {
                                let _ = tx.send(crate::commands::exec::summary(&ctx, Some(&slug)));
                            });
                            app.spinner_frame = 0;
                            app.summarizing = Some(Summarizing {
                                slug: task.slug.clone(),
                                rx,
                                started: std::time::Instant::now(),
                            });
                        }
                        Err(e) => {
                            app.popup = Some(format!("{:#}", e));
                            app.popup_error = true;
                        }
                    },
                    None => {
                        app.popup = Some(format!("task '{}' vanished", id));
                        app.popup_error = true;
                    }
                }
            }
            UiAction::ToggleItem(id, item_id) => {
                // mutate the doc in place; the item list redraws with the new
                // checkbox state (its own feedback, so no transient status line)
                let r = match find_task(projects, &id) {
                    Some((info, task)) => toggle_item(&info.store, &task, &item_id),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                if let Err(e) = r {
                    app.popup = Some(format!("{:#}", e));
                    app.popup_error = true;
                }
                app.tasks = load_all_rows(projects);
            }
            UiAction::AddItem(id, text) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => add_item(&info.store, &task, &text),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                if let Err(e) = r {
                    app.popup = Some(format!("{:#}", e));
                    app.popup_error = true;
                }
                app.tasks = load_all_rows(projects);
                // land the cursor on the freshly added item (now the last one)
                let n = app
                    .selected_task()
                    .map(|t| task_items(&t.body).len())
                    .unwrap_or(0);
                if n > 0 {
                    app.item_sel = n - 1;
                }
            }
            UiAction::EditItem(id, item_id, text) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => edit_item(&info.store, &task, &item_id, &text),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                if let Err(e) = r {
                    app.popup = Some(format!("{:#}", e));
                    app.popup_error = true;
                }
                app.tasks = load_all_rows(projects);
            }
            UiAction::DeleteItem(id, item_id) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => delete_item(&info.store, &task, &item_id),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                if let Err(e) = r {
                    app.popup = Some(format!("{:#}", e));
                    app.popup_error = true;
                }
                app.tasks = load_all_rows(projects);
                // the deleted item is gone; clamp the cursor to the new list
                let n = app
                    .selected_task()
                    .map(|t| task_items(&t.body).len())
                    .unwrap_or(0);
                app.item_sel = app.item_sel.min(n.saturating_sub(1));
            }
            UiAction::Worktrees(id) => {
                // resolve the task's project repo, list its worktrees, open the view
                match find_task(projects, &id) {
                    Some((info, _)) => {
                        let project = info.project.clone();
                        match ctx_for(info).and_then(|ctx| ctx.repo.worktree_list()) {
                            Ok(list) => {
                                app.worktrees = list;
                                app.worktree_sel = 0;
                                app.worktree_anchor = Some(id);
                                app.worktree_project = project;
                                app.viewing_worktrees = true;
                            }
                            Err(e) => {
                                app.popup = Some(format!("{:#}", e));
                                app.popup_error = true;
                            }
                        }
                    }
                    None => app.message = format!("task '{}' vanished", id),
                }
            }
            UiAction::AddWorktree(anchor, branch) => {
                // new worktrees live under the store's worktrees/ dir (like task
                // worktrees), keeping the repo's parent dir clean
                let r = match find_task(projects, &anchor) {
                    Some((info, _)) => {
                        let path = info.store.root.join("worktrees").join(&branch);
                        ctx_for(info).and_then(|ctx| ctx.repo.worktree_add_branch(&path, &branch))
                    }
                    None => Err(anyhow!("task '{}' vanished", anchor)),
                };
                if let Err(e) = r {
                    app.popup = Some(format!("{:#}", e));
                    app.popup_error = true;
                }
                reload_worktrees(app, projects);
                // land the cursor on the worktree we just added, if it's there
                if let Some(pos) = app
                    .worktrees
                    .iter()
                    .position(|w| w.branch.as_deref() == Some(branch.as_str()))
                {
                    app.worktree_sel = pos;
                }
            }
            UiAction::DeleteWorktree(anchor, path) => {
                let r = match find_task(projects, &anchor) {
                    Some((info, _)) => ctx_for(info)
                        .and_then(|ctx| ctx.repo.worktree_remove(Path::new(&path), false)),
                    None => Err(anyhow!("task '{}' vanished", anchor)),
                };
                if let Err(e) = r {
                    app.popup = Some(format!("{:#}", e));
                    app.popup_error = true;
                }
                reload_worktrees(app, projects);
            }
            UiAction::LockWorktree(anchor, path, lock) => {
                let r = match find_task(projects, &anchor) {
                    Some((info, _)) => ctx_for(info)
                        .and_then(|ctx| ctx.repo.worktree_lock(Path::new(&path), lock)),
                    None => Err(anyhow!("task '{}' vanished", anchor)),
                };
                if let Err(e) = r {
                    app.popup = Some(format!("{:#}", e));
                    app.popup_error = true;
                }
                reload_worktrees(app, projects);
            }
        }
        // exec verbs print progress to stdout; in raw mode that leaves stray text
        // on the alternate screen, so force a full repaint after each action
        terminal.clear()?;
    }
}

/// Flip the checked state of a checklist item in its document. IDs are healed
/// first so the in-UI id (computed the same way) resolves to a real marker on
/// disk; a resolved-failed item is reopened cleanly rather than left a
/// half-decorated `- [ ]` with a stale failed sentinel.
fn toggle_item(store: &Store, task: &TaskRef, item_id: &str) -> Result<()> {
    let task = crate::commands::assign_ids(store, task)?;
    let item = crate::task::scan_items(&task.doc.body)
        .into_iter()
        .find(|i| i.id.as_deref() == Some(item_id))
        .ok_or_else(|| anyhow!("item '{}' not found", item_id))?;
    let new_body = if item.failed {
        crate::task::clear_failed(&task.doc.body, item_id)?
    } else {
        crate::task::set_checked(&task.doc.body, item_id, !item.checked)?
    };
    let mut doc = task.doc.clone();
    doc.body = new_body;
    doc.touch();
    store.write_doc(&task.path, &doc)
}

/// Append a new checklist item to a task's `## Tasks` section, then heal item
/// IDs so the fresh line gets a stable integer id — the same persist-then-heal
/// path `rein open` follows after an `$EDITOR` edit (and the mirror of
/// `toggle_item`).
fn add_item(store: &Store, task: &TaskRef, text: &str) -> Result<()> {
    let body = crate::task::append_item(&task.doc.body, text)?;
    let mut doc = task.doc.clone();
    doc.body = body;
    doc.touch();
    store.write_doc(&task.path, &doc)?;
    if let Some(fresh) = store.find_by_id(&task.id) {
        crate::commands::assign_ids(store, &fresh)?;
    }
    Ok(())
}

/// Replace a checklist item's text in its document. IDs are healed first so the
/// in-UI id (computed the same way) resolves to a real marker on disk (mirrors
/// `toggle_item`).
fn edit_item(store: &Store, task: &TaskRef, item_id: &str, text: &str) -> Result<()> {
    let task = crate::commands::assign_ids(store, task)?;
    let body = crate::task::edit_item(&task.doc.body, item_id, text)?;
    let mut doc = task.doc.clone();
    doc.body = body;
    doc.touch();
    store.write_doc(&task.path, &doc)
}

/// Remove a checklist item from its document. IDs are healed first so the in-UI
/// id resolves to a real marker on disk (mirrors `toggle_item`).
fn delete_item(store: &Store, task: &TaskRef, item_id: &str) -> Result<()> {
    let task = crate::commands::assign_ids(store, task)?;
    let body = crate::task::delete_item(&task.doc.body, item_id)?;
    let mut doc = task.doc.clone();
    doc.body = body;
    doc.touch();
    store.write_doc(&task.path, &doc)
}

/// Heal item IDs for a doc just edited in $EDITOR (matches `rein open`).
fn heal_ids(projects: &[StoreInfo], path: &Path) {
    if let Some((info, t)) = find_task_by_path(projects, path) {
        let _ = crate::commands::assign_ids(&info.store, &t);
    }
}

/// Re-run `git worktree list` for the worktree view's anchor project and
/// refresh `app.worktrees`, clamping the cursor. Best-effort: a resolve/list
/// failure leaves the list as-is (the triggering mutation surfaces its own error).
fn reload_worktrees(app: &mut App, projects: &[StoreInfo]) {
    let Some(anchor) = app.worktree_anchor.clone() else {
        return;
    };
    if let Some((info, _)) = find_task(projects, &anchor) {
        if let Ok(list) = ctx_for(info).and_then(|ctx| ctx.repo.worktree_list()) {
            app.worktrees = list;
            app.worktree_sel = app.worktree_sel.min(app.worktrees.len().saturating_sub(1));
        }
    }
}

/// Move the selection onto the row whose doc lives at `path`, if visible.
fn select_path(app: &mut App, path: &Path) {
    let vis = app.visible();
    if let Some(pos) = vis.iter().position(|&i| app.tasks[i].path == path) {
        app.selected = pos;
    }
}

fn finish(app: &mut App, projects: &[StoreInfo], result: Result<()>) {
    match result {
        Ok(()) => app.message = "ok".to_string(),
        // surface failures as a modal popup, not a clipped status line — `{:#}`
        // flattens the whole anyhow chain so the cause (e.g. git's stderr) shows
        Err(e) => {
            app.popup = Some(format!("{:#}", e));
            app.popup_error = true;
        }
    }
    app.tasks = load_all_rows(projects);
}

/// Like `finish`, but a sync conflict arms the force-push offer (a prompt the
/// user answers with `f`) instead of a dead-end error popup — the dashboard
/// counterpart of the CLI's `rein push --resolved`. Success and every other
/// error behave exactly like `finish`.
fn finish_push(
    app: &mut App,
    projects: &[StoreInfo],
    result: Result<()>,
    id: &str,
    surface: ForceSurface,
) {
    if let Err(e) = &result {
        if let Some(c) = e.downcast_ref::<crate::sync::Conflict>() {
            app.force_push = Some(ForcePush {
                task_id: id.to_string(),
                surface,
                slug: c.slug.clone(),
            });
            return;
        }
    }
    finish(app, projects, result);
}

/// Fuzzy picker used by `rein open` without an argument.
pub fn pick_task(tasks: &[TaskRef]) -> Result<Option<PathBuf>> {
    use std::io::IsTerminal;
    if !io::stdout().is_terminal() {
        anyhow::bail!("no tty for the picker — pass a task argument");
    }
    let rows: Vec<TaskRow> = tasks
        .iter()
        .map(|t| TaskRow::from_ref(t, "", Path::new("")))
        .collect();
    let mut app = App::new(rows);
    app.filtering = true;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut picked = None;
    loop {
        terminal.draw(|f| app.render(f))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Enter => {
                picked = app.selected_task().map(|t| t.path.clone());
                break;
            }
            KeyCode::Esc if !app.filtering => break,
            KeyCode::Char('q') if !app.filtering => break,
            _ => {
                app.on_key(key);
            }
        }
    }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(picked)
}
