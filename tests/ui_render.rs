use crossterm::event::{KeyCode, KeyEvent};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use rein::store::Status;
use rein::ui::{App, TaskRow, UiAction};
use std::path::PathBuf;

fn rows() -> Vec<TaskRow> {
    let mk = |slug: &str, title: &str, status: Status, body: &str| TaskRow {
        id: format!("task-20260613-{}", slug),
        slug: slug.to_string(),
        title: title.to_string(),
        status,
        path: PathBuf::from(format!("/store/{}/{}.md", status.as_str(), slug)),
        body: body.to_string(),
        has_issue: false,
    };
    vec![
        mk(
            "settings-cleanup",
            "Settings cleanup",
            Status::Inbox,
            "## Goal\n\nClean settings\n\n## Tasks\n\n- [ ] <!-- task:layout --> Layout\n- [x] <!-- task:toast --> Toast",
        ),
        mk("auth-refactor", "Auth refactor", Status::Active, "## Goal\n\nRefactor auth"),
        mk("old-thing", "Old thing", Status::Done, "## Goal\n\nDone already"),
    ]
}

fn draw(app: &App) -> String {
    let backend = TestBackend::new(160, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| app.render(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let w = buf.area.width as usize;
    buf.content
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut s = c.symbol().to_string();
            if (i + 1) % w == 0 {
                s.push('\n');
            }
            s
        })
        .collect()
}

fn key(app: &mut App, code: KeyCode) -> UiAction {
    app.on_key(KeyEvent::from(code))
}

#[test]
fn renders_task_list_and_markdown_preview() {
    let app = App::new(rows());
    let screen = draw(&app);
    // left pane: all tasks with status chips
    assert!(screen.contains("settings-cleanup — Settings cleanup"));
    assert!(screen.contains("auth-refactor — Auth refactor"));
    assert!(screen.contains("[inbox]"));
    assert!(screen.contains("[active]"));
    // right pane previews the selected (first) task's markdown
    assert!(screen.contains("## Goal"));
    assert!(screen.contains("- [ ] <!-- task:layout --> Layout"));
    // status line shows keybindings
    assert!(screen.contains("s start"));
    assert!(screen.contains("p issue/push"));
}

#[test]
fn j_k_move_selection_and_preview_follows() {
    let mut app = App::new(rows());
    assert_eq!(app.selected_task().unwrap().slug, "settings-cleanup");
    key(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected_task().unwrap().slug, "auth-refactor");
    let screen = draw(&app);
    assert!(screen.contains("Refactor auth"));
    key(&mut app, KeyCode::Char('k'));
    assert_eq!(app.selected_task().unwrap().slug, "settings-cleanup");
    // clamped at both ends
    key(&mut app, KeyCode::Char('k'));
    assert_eq!(app.selected_task().unwrap().slug, "settings-cleanup");
    for _ in 0..10 {
        key(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.selected_task().unwrap().slug, "old-thing");
}

#[test]
fn tab_cycles_status_groups() {
    let mut app = App::new(rows());
    assert_eq!(app.tab_name(), "all");
    assert_eq!(app.visible().len(), 3);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.tab_name(), "inbox");
    assert_eq!(app.visible().len(), 1);
    assert_eq!(app.selected_task().unwrap().slug, "settings-cleanup");
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.tab_name(), "active");
    assert_eq!(app.selected_task().unwrap().slug, "auth-refactor");
}

#[test]
fn fuzzy_filter_narrows_list() {
    let mut app = App::new(rows());
    key(&mut app, KeyCode::Char('/'));
    for c in "auth".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    assert_eq!(app.visible().len(), 1);
    assert_eq!(app.selected_task().unwrap().slug, "auth-refactor");
    let screen = draw(&app);
    assert!(screen.contains("filter: auth"));
    // Esc clears the filter
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.visible().len(), 3);
}

#[test]
fn keys_dispatch_to_cli_verbs() {
    let mut app = App::new(rows());
    // Enter opens $EDITOR on the selected doc
    let action = key(&mut app, KeyCode::Enter);
    assert_eq!(
        action,
        UiAction::Edit(PathBuf::from("/store/inbox/settings-cleanup.md"))
    );
    // s on an inbox task starts it
    let action = key(&mut app, KeyCode::Char('s'));
    assert_eq!(action, UiAction::Start("task-20260613-settings-cleanup".into()));
    // s on an active task is refused with a message
    key(&mut app, KeyCode::Char('j'));
    let action = key(&mut app, KeyCode::Char('s'));
    assert_eq!(action, UiAction::None);
    assert!(app.message.contains("inbox"));
    // d finishes, p publishes/pushes
    let action = key(&mut app, KeyCode::Char('d'));
    assert_eq!(action, UiAction::Done("task-20260613-auth-refactor".into()));
    let action = key(&mut app, KeyCode::Char('p'));
    assert_eq!(action, UiAction::Publish("task-20260613-auth-refactor".into()));
    // q quits
    assert_eq!(key(&mut app, KeyCode::Char('q')), UiAction::Quit);
}
