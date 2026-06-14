use crossterm::event::{KeyCode, KeyEvent};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use rein::store::Status;
use rein::ui::{App, TaskRow, UiAction};
use std::path::PathBuf;

fn rows() -> Vec<TaskRow> {
    rows_in("")
}

fn rows_in(project: &str) -> Vec<TaskRow> {
    let project = project.to_string();
    let mk = move |slug: &str, title: &str, status: Status, body: &str| TaskRow {
        id: format!("task-20260613-{}", slug),
        slug: slug.to_string(),
        title: title.to_string(),
        status,
        path: PathBuf::from(format!("/store/{}/{}.md", status.as_str(), slug)),
        body: body.to_string(),
        has_issue: false,
        project: project.clone(),
        store_root: PathBuf::from("/store"),
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

/// Rows spanning two projects (acme/web has 2, tools has 1).
fn rows_multi() -> Vec<TaskRow> {
    let mk = |slug: &str, project: &str, status: Status| TaskRow {
        id: format!("task-20260613-{}", slug),
        slug: slug.to_string(),
        title: format!("{} title", slug),
        status,
        path: PathBuf::from(format!("/store/{}/{}/{}.md", project, status.as_str(), slug)),
        body: format!("## Goal\n\n{}", slug),
        has_issue: false,
        project: project.to_string(),
        store_root: PathBuf::from(format!("/store/{}", project)),
    };
    vec![
        mk("web-a", "acme/web", Status::Inbox),
        mk("web-b", "acme/web", Status::Active),
        mk("tools-a", "tools", Status::Inbox),
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

#[test]
fn n_creates_task_via_input_mode() {
    let mut app = App::new(rows());
    // n opens a title prompt; typed chars do not trigger other verbs
    assert_eq!(key(&mut app, KeyCode::Char('n')), UiAction::None);
    assert!(app.creating);
    for c in "my new task".chars() {
        assert_eq!(key(&mut app, KeyCode::Char(c)), UiAction::None);
    }
    let screen = draw(&app);
    assert!(screen.contains("new task"));
    assert!(screen.contains("my new task"));
    // Enter emits the create action and leaves input mode
    let action = key(&mut app, KeyCode::Enter);
    assert_eq!(action, UiAction::New("my new task".into()));
    assert!(!app.creating);

    // Esc cancels without creating
    key(&mut app, KeyCode::Char('n'));
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(key(&mut app, KeyCode::Esc), UiAction::None);
    assert!(!app.creating);
    assert!(app.input.is_empty());
}

#[test]
fn m_moves_selected_task_to_any_state() {
    let mut app = App::new(rows());
    // selected = inbox task; m then d moves it to done (a backward-or-forward
    // transition the one-shot verbs don't offer)
    assert_eq!(key(&mut app, KeyCode::Char('m')), UiAction::None);
    assert!(app.moving);
    let screen = draw(&app);
    assert!(screen.contains("move settings-cleanup to:"));
    let action = key(&mut app, KeyCode::Char('d'));
    assert_eq!(
        action,
        UiAction::Move("task-20260613-settings-cleanup".into(), Status::Done)
    );
    assert!(!app.moving);

    // m then a on the done task reopens it to active
    let mut app = App::new(rows());
    for _ in 0..2 {
        key(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.selected_task().unwrap().slug, "old-thing");
    key(&mut app, KeyCode::Char('m'));
    let action = key(&mut app, KeyCode::Char('a'));
    assert_eq!(
        action,
        UiAction::Move("task-20260613-old-thing".into(), Status::Active)
    );

    // moving to the current state is a no-op with a message
    let mut app = App::new(rows());
    key(&mut app, KeyCode::Char('m'));
    let action = key(&mut app, KeyCode::Char('i')); // already inbox
    assert_eq!(action, UiAction::None);
    assert!(app.message.contains("already inbox"));

    // any non-target key cancels move mode
    let mut app = App::new(rows());
    key(&mut app, KeyCode::Char('m'));
    assert_eq!(key(&mut app, KeyCode::Char('z')), UiAction::None);
    assert!(!app.moving);
}

#[test]
fn project_label_renders_and_filters() {
    let mut app = App::new(rows_in("acme/web"));
    let screen = draw(&app);
    // each row is tagged with its project for the cross-project view
    assert!(screen.contains("acme/web"));
    assert!(screen.contains("settings-cleanup — Settings cleanup"));

    // the fuzzy filter also matches on project name (doubles as a project picker)
    key(&mut app, KeyCode::Char('/'));
    for c in "acme".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    assert_eq!(app.visible().len(), 3, "all tasks share the project");
}

#[test]
fn keybinding_hint_advertises_new_and_move() {
    let app = App::new(rows());
    let screen = draw(&app);
    assert!(screen.contains("n new"));
    assert!(screen.contains("m move"));
    assert!(screen.contains("P project"));
}

#[test]
fn project_scope_filters_task_list() {
    let mut app = App::new(rows_multi());
    // unscoped: every project's tasks are visible, tagged by project
    assert_eq!(app.visible().len(), 3);
    let screen = draw(&app);
    assert!(screen.contains("tasks [all · all]"));
    assert!(screen.contains("acme/web"));
    assert!(screen.contains("tools"));

    // scoping to one project hides the others (and the now-redundant tag)
    app.project_scope = Some("acme/web".into());
    assert_eq!(app.visible().len(), 2);
    assert!(app.visible().iter().all(|&i| app.tasks[i].project == "acme/web"));
    let screen = draw(&app);
    assert!(screen.contains("tasks [acme/web · all]"));
    assert!(screen.contains("web-a — web-a title"));
    assert!(!screen.contains("tools-a"));
}

#[test]
fn project_picker_navigates_and_scopes() {
    let mut app = App::new(rows_multi());
    // P opens the hierarchical project level, listing projects with counts
    assert_eq!(key(&mut app, KeyCode::Char('P')), UiAction::None);
    assert!(app.picking_project);
    let screen = draw(&app);
    assert!(screen.contains("projects"));
    assert!(screen.contains("all projects (3)"));
    assert!(screen.contains("acme/web (2)"));
    assert!(screen.contains("tools (1)"));

    // j to "acme/web" (index 1), Enter scopes the task list to it
    key(&mut app, KeyCode::Char('j'));
    key(&mut app, KeyCode::Enter);
    assert!(!app.picking_project);
    assert_eq!(app.project_scope.as_deref(), Some("acme/web"));
    assert_eq!(app.visible().len(), 2);

    // reopening pre-positions the cursor on the active scope; "all" resets it
    key(&mut app, KeyCode::Char('P'));
    assert_eq!(app.project_sel, 1, "cursor sits on the current scope");
    key(&mut app, KeyCode::Char('k')); // up to "all projects"
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.project_scope, None);
    assert_eq!(app.visible().len(), 3);

    // Esc cancels without changing scope
    app.project_scope = Some("tools".into());
    key(&mut app, KeyCode::Char('P'));
    key(&mut app, KeyCode::Char('j'));
    key(&mut app, KeyCode::Esc);
    assert!(!app.picking_project);
    assert_eq!(app.project_scope.as_deref(), Some("tools"));
}

#[test]
fn failed_items_render_red_and_struck() {
    use ratatui::style::{Color, Modifier};
    use rein::ui::render_markdown;

    let body = "## Tasks\n\n- [ ] <!-- task:1 --> open\n- [x] <!-- task:2 --> done\n- [x] <!-- task:3 --> <!-- failed --> ~~nope~~ \u{274c}";
    let lines = render_markdown(body);
    let text = |l: &ratatui::text::Line| -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    };

    let failed = lines.iter().find(|l| text(l).contains("nope")).unwrap();
    assert_eq!(failed.style.fg, Some(Color::Red));
    assert!(failed.style.add_modifier.contains(Modifier::CROSSED_OUT));

    // a normal checked item stays green, an open item stays yellow
    let done = lines.iter().find(|l| text(l).contains("done")).unwrap();
    assert_eq!(done.style.fg, Some(Color::Green));
    let open = lines.iter().find(|l| text(l).contains("open")).unwrap();
    assert_eq!(open.style.fg, Some(Color::Yellow));
}
