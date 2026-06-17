use crate::gitx::Repo;
use crate::store::{Status, Store, StoreInfo, TaskRef};
use crate::Ctx;
use anyhow::{anyhow, Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
    /// State of the last `rein run`'s background session (`working`/`done`/…),
    /// resolved from `claude agents`; `None` if no run or the session is unknown.
    pub run_state: Option<String>,
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
            project: project.to_string(),
            store_root: store_root.to_path_buf(),
        }
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
    Publish(String),        // issue if none attached, else push
    CreatePr(String, bool), // open a draft PR; bool = worktree (vs main-repo branch)
    Run(String),            // launch an agent on the task in the background
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

pub struct App {
    pub tasks: Vec<TaskRow>,
    pub tab: usize,
    pub selected: usize,
    pub filter: String,
    pub filtering: bool,
    pub creating: bool,
    pub moving: bool,
    /// True while awaiting the start-mode key (`s` single / `w` worktree / `b` branch).
    pub starting: bool,
    /// True while awaiting the PR-mode key (`w` worktree / `b` branch).
    pub pring: bool,
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
            starting: false,
            pring: false,
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
        // a modal error overlay swallows the next key (any key dismisses it)
        if self.popup.is_some() {
            self.popup = None;
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
            KeyCode::Char('p') => {
                if let Some(t) = self.selected_task() {
                    return UiAction::Publish(t.id.clone());
                }
            }
            KeyCode::Char('x') => match self.selected_task() {
                Some(t) if t.status == Status::Active => return UiAction::Run(t.id.clone()),
                Some(_) => self.message = "only active tasks can run (start it first)".into(),
                None => {}
            },
            KeyCode::Char('r') => match self.selected_task() {
                Some(t) if t.github_pr.is_some() => self.message = "already has a PR".into(),
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
            .constraints([Constraint::Length(6), Constraint::Min(3)])
            .split(panes[1]);
        self.render_list(f, panes[0]);
        self.render_meta(f, right[0]);
        self.render_preview(f, right[1]);
        self.render_statusline(f, outer[1]);
        if let Some(msg) = &self.popup {
            self.render_popup(f, msg);
        }
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
        if self.picking_project {
            self.render_project_picker(f, area);
        } else {
            self.render_task_list(f, area);
        }
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
        let text = if self.creating {
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
        } else if self.picking_project {
            "j/k move · Enter select project · Esc cancel".to_string()
        } else if self.filtering {
            format!("/{}", self.filter)
        } else if !self.message.is_empty() {
            self.message.clone()
        } else {
            "j/k move · Tab status · P project · Enter edit · n new · s start · m move · d done · x run · r PR · p issue/push · / filter · q quit"
                .to_string()
        };
        f.render_widget(Paragraph::new(text).style(Style::default().fg(Color::DarkGray)), area);
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
    vec![
        Line::from(Span::styled(
            t.id.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("branch: ", dim),
            Span::raw(t.branch.clone().unwrap_or_else(dash)),
            Span::styled("   run: ", dim),
            Span::styled(run_txt, Style::default().fg(run_color)),
        ]),
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

/// Human label + color for a background-session state from `claude agents`.
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

// ---------------------------------------------------------------------------
// Interactive loops (thin view + dispatcher over the CLI verbs)
// ---------------------------------------------------------------------------

fn load_all_rows(projects: &[StoreInfo]) -> Vec<TaskRow> {
    let mut out = Vec::new();
    // (row index, run session id) for tasks that have been `rein run`
    let mut sessions: Vec<(usize, String)> = Vec::new();
    for p in projects {
        for t in p.store.list_tasks() {
            if let Some(id) = crate::state::load(&p.store, &t.id).run_session {
                sessions.push((out.len(), id));
            }
            out.push(TaskRow::from_ref(&t, &p.project, &p.store.root));
        }
    }
    // one `claude agents` query covers every project; skip it when nothing ran
    if !sessions.is_empty() {
        let states = run_states();
        for (i, id) in sessions {
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
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = event_loop(&mut terminal, &mut app, &projects);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    projects: &[StoreInfo],
) -> Result<()> {
    let mut idle_ticks = 0u32;
    loop {
        terminal.draw(|f| app.render(f))?;
        if !event::poll(std::time::Duration::from_millis(250))? {
            // while a background run is live, re-poll `claude agents` every ~4s so
            // its state (running → done) updates without the user touching anything
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
        let Event::Key(key) = event::read()? else {
            continue;
        };
        let action = app.on_key(key);
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
            UiAction::Publish(id) => {
                let r = match find_task(projects, &id) {
                    Some((info, task)) => ctx_for(info).and_then(|ctx| {
                        // push to whatever's already attached (issue and/or PR);
                        // only create an issue when the task is completely bare,
                        // so a PR-only task publishes to its PR, not a new issue.
                        if task.doc.front.github_issue.is_some()
                            || task.doc.front.github_pr.is_some()
                        {
                            crate::commands::sync_cmd::push_task(&ctx, &task, false)
                        } else {
                            crate::commands::sync_cmd::issue(&ctx, &task.slug)
                        }
                    }),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
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
                // show the session id + claude attach/logs hints in a popup, not
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
        }
        // exec verbs print progress to stdout; in raw mode that leaves stray text
        // on the alternate screen, so force a full repaint after each action
        terminal.clear()?;
    }
}

/// Heal item IDs for a doc just edited in $EDITOR (matches `rein open`).
fn heal_ids(projects: &[StoreInfo], path: &Path) {
    if let Some((info, t)) = find_task_by_path(projects, path) {
        let _ = crate::commands::assign_ids(&info.store, &t);
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
