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
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
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
    pub has_issue: bool,
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
            has_issue: t.doc.front.github_issue.is_some(),
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
    New(String),          // create an inbox task with this title
    Start(String),
    Move(String, Status), // free-form transition to any state
    Done(String),
    Publish(String), // issue if none attached, else push
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
    pub input: String,
    pub message: String,
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
            input: String::new(),
            message: String::new(),
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

    /// Indices into `tasks` visible under the current tab + fuzzy filter.
    pub fn visible(&self) -> Vec<usize> {
        let tab_filtered: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
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
            KeyCode::Char('s') => {
                if let Some(t) = self.selected_task() {
                    if t.status == Status::Inbox {
                        return UiAction::Start(t.id.clone());
                    }
                    self.message = "only inbox tasks can be started (use m to move)".into();
                }
            }
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
        self.render_list(f, panes[0]);
        self.render_preview(f, panes[1]);
        self.render_statusline(f, outer[1]);
    }

    fn render_list(&self, f: &mut Frame, area: Rect) {
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
                if !t.project.is_empty() {
                    spans.push(Span::styled(
                        format!("{} ", t.project),
                        Style::default().fg(Color::Magenta),
                    ));
                }
                spans.push(Span::styled(format!("{} — {}", t.slug, t.title), style));
                ListItem::new(Line::from(spans))
            })
            .collect();
        let title = format!(
            " tasks ({}) {} ",
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
        } else if self.filtering {
            format!("/{}", self.filter)
        } else if !self.message.is_empty() {
            self.message.clone()
        } else {
            "j/k move · Tab status · Enter edit · n new · s start · m move · d done · p issue/push · / filter · q quit"
                .to_string()
        };
        f.render_widget(Paragraph::new(text).style(Style::default().fg(Color::DarkGray)), area);
    }
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
        } else if line.trim_start().starts_with("- [x]") || line.trim_start().starts_with("- [X]") {
            out.push(Line::styled(line, Style::default().fg(Color::Green)));
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
    for p in projects {
        for t in p.store.list_tasks() {
            out.push(TaskRow::from_ref(&t, &p.project, &p.store.root));
        }
    }
    out
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
    app.home_store_root = home_info.as_ref().map(|h| h.store.root.clone());
    app.home_label = home_info.as_ref().map(|h| h.project.clone());

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
    loop {
        terminal.draw(|f| app.render(f))?;
        if !event::poll(std::time::Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        match app.on_key(key) {
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
                            Err(e) => app.message = format!("error: {}", e),
                        }
                    }
                    None => {
                        app.message =
                            "no project to create in — run `rein init` in a repo first".into()
                    }
                }
            }
            UiAction::Start(id) => {
                let r = match find_task(projects, &id) {
                    Some((info, _)) => ctx_for(info)
                        .and_then(|ctx| crate::commands::exec::start(&ctx, &id, false, None, false)),
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
                        if task.doc.front.github_issue.is_some() {
                            crate::commands::sync_cmd::push_task(&ctx, &task, false)
                        } else {
                            crate::commands::sync_cmd::issue(&ctx, &task.slug)
                        }
                    }),
                    None => Err(anyhow!("task '{}' vanished", id)),
                };
                finish(app, projects, r);
            }
        }
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
        Err(e) => app.message = format!("error: {}", e),
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
