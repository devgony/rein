use crate::store::{Status, TaskRef};
use crate::Ctx;
use anyhow::Result;
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
use std::path::PathBuf;

/// One row in the dashboard. Detached from the store so tests can build it.
#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub status: Status,
    pub path: PathBuf,
    pub body: String,
    pub has_issue: bool,
}

impl TaskRow {
    pub fn from_ref(t: &TaskRef) -> TaskRow {
        TaskRow {
            id: t.id.clone(),
            slug: t.slug.clone(),
            title: t.doc.front.title.clone(),
            status: t.status,
            path: t.path.clone(),
            body: t.doc.body.clone(),
            has_issue: t.doc.front.github_issue.is_some(),
        }
    }
}

/// What the event loop should do after a key.
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    None,
    Quit,
    Edit(PathBuf),
    Start(String),
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
    pub message: String,
}

impl App {
    pub fn new(tasks: Vec<TaskRow>) -> App {
        App {
            tasks,
            tab: 0,
            selected: 0,
            filter: String::new(),
            filtering: false,
            message: String::new(),
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
        let candidates: Vec<(usize, String)> = tab_filtered
            .iter()
            .map(|&i| (i, format!("{} {}", self.tasks[i].slug, self.tasks[i].title)))
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
            KeyCode::Char('s') => {
                if let Some(t) = self.selected_task() {
                    if t.status == Status::Inbox {
                        return UiAction::Start(t.id.clone());
                    }
                    self.message = "only inbox tasks can be started".into();
                }
            }
            KeyCode::Char('d') => {
                if let Some(t) = self.selected_task() {
                    if matches!(t.status, Status::Inbox | Status::Active) {
                        return UiAction::Done(t.id.clone());
                    }
                    self.message = "task is already finished".into();
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
                ListItem::new(Line::from(vec![
                    Span::raw(marker),
                    Span::styled(format!("[{}] ", t.status.as_str()), Style::default().fg(status_color(t.status))),
                    Span::styled(format!("{} — {}", t.slug, t.title), style),
                ]))
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
        let text = if self.filtering {
            format!("/{}", self.filter)
        } else if !self.message.is_empty() {
            self.message.clone()
        } else {
            "j/k move · Tab status · Enter edit · s start · d done · p issue/push · / filter · q quit"
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

fn load_rows(ctx: &Ctx) -> Vec<TaskRow> {
    ctx.store.list_tasks().iter().map(TaskRow::from_ref).collect()
}

pub fn run(ctx: &Ctx) -> Result<()> {
    let mut app = App::new(load_rows(ctx));
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = event_loop(&mut terminal, &mut app, ctx);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    ctx: &Ctx,
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
                // suspend the TUI while $EDITOR owns the terminal
                disable_raw_mode()?;
                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                let res = crate::commands::local::edit_file(&path);
                enable_raw_mode()?;
                execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                terminal.clear()?;
                if let Err(e) = res {
                    app.message = format!("edit failed: {}", e);
                }
                app.tasks = load_rows(ctx);
            }
            UiAction::Start(id) => {
                dispatch(app, ctx, |ctx| {
                    crate::commands::exec::start(ctx, &id, false, None, false)
                });
            }
            UiAction::Done(id) => {
                dispatch(app, ctx, |ctx| {
                    crate::commands::exec::done(ctx, Some(&id), false)
                });
            }
            UiAction::Publish(id) => {
                let has_issue = app
                    .tasks
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| t.has_issue)
                    .unwrap_or(false);
                dispatch(app, ctx, |ctx| {
                    let task = ctx.store.find(&id)?;
                    if has_issue {
                        crate::commands::sync_cmd::push_task(ctx, &task, false)
                    } else {
                        crate::commands::sync_cmd::issue(ctx, &task.slug)
                    }
                });
            }
        }
    }
}

fn dispatch<F: FnOnce(&Ctx) -> Result<()>>(app: &mut App, ctx: &Ctx, f: F) {
    match f(ctx) {
        Ok(()) => app.message = "ok".to_string(),
        Err(e) => app.message = format!("error: {}", e),
    }
    app.tasks = load_rows(ctx);
}

/// Fuzzy picker used by `rein open` without an argument.
pub fn pick_task(tasks: &[TaskRef]) -> Result<Option<PathBuf>> {
    use std::io::IsTerminal;
    if !io::stdout().is_terminal() {
        anyhow::bail!("no tty for the picker — pass a task argument");
    }
    let rows: Vec<TaskRow> = tasks.iter().map(TaskRow::from_ref).collect();
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
