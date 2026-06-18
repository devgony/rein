use crossterm::event::{KeyCode, KeyEvent};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use rein::store::Status;
use rein::ui::{App, StartMode, TaskRow, UiAction};
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
        branch: None,
        github_issue: None,
        github_pr: None,
        created_at: String::new(),
        updated_at: String::new(),
        tags: Vec::new(),
        shared: false,
        project: project.clone(),
        store_root: PathBuf::from("/store"),
        run_state: None,
        worktree: None,
        repo_dir: None,
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
        branch: None,
        github_issue: None,
        github_pr: None,
        created_at: String::new(),
        updated_at: String::new(),
        tags: Vec::new(),
        shared: false,
        project: project.to_string(),
        store_root: PathBuf::from(format!("/store/{}", project)),
        run_state: None,
        worktree: None,
        repo_dir: None,
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
    assert!(screen.contains("i issue"));
    assert!(screen.contains("r PR"));
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
    // s on an inbox task opens the start-mode picker; w starts in a worktree
    assert_eq!(key(&mut app, KeyCode::Char('s')), UiAction::None);
    assert!(app.starting);
    let action = key(&mut app, KeyCode::Char('w'));
    assert_eq!(
        action,
        UiAction::Start("task-20260613-settings-cleanup".into(), StartMode::Worktree)
    );
    assert!(!app.starting);
    // s on an active task is refused with a message (no picker)
    key(&mut app, KeyCode::Char('j'));
    let action = key(&mut app, KeyCode::Char('s'));
    assert_eq!(action, UiAction::None);
    assert!(!app.starting);
    assert!(app.message.contains("inbox"));
    // d finishes; i begins issue creation (auth-refactor has no issue yet)
    let action = key(&mut app, KeyCode::Char('d'));
    assert_eq!(action, UiAction::Done("task-20260613-auth-refactor".into()));
    let action = key(&mut app, KeyCode::Char('i'));
    assert_eq!(action, UiAction::Issue("task-20260613-auth-refactor".into()));
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
fn shift_d_confirms_then_deletes() {
    // D opens a confirmation; only y proceeds to the delete action
    let mut app = App::new(rows());
    assert_eq!(key(&mut app, KeyCode::Char('D')), UiAction::None);
    assert!(app.deleting);
    let screen = draw(&app);
    assert!(screen.contains("delete settings-cleanup permanently?"));
    let action = key(&mut app, KeyCode::Char('y'));
    assert_eq!(
        action,
        UiAction::Delete("task-20260613-settings-cleanup".into())
    );
    assert!(!app.deleting);

    // any other key cancels without deleting (no destructive default)
    let mut app = App::new(rows());
    key(&mut app, KeyCode::Char('D'));
    assert_eq!(key(&mut app, KeyCode::Char('n')), UiAction::None);
    assert!(!app.deleting);
    // lowercase d is still "done", not delete
    assert_eq!(
        key(&mut app, KeyCode::Char('d')),
        UiAction::Done("task-20260613-settings-cleanup".into())
    );
}

#[test]
fn x_runs_only_active_tasks() {
    let mut app = App::new(rows());
    // the inbox task (index 0) can't run — must be started first
    assert_eq!(key(&mut app, KeyCode::Char('x')), UiAction::None);
    assert!(app.message.contains("active"));
    // the active task runs
    key(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected_task().unwrap().status, Status::Active);
    assert_eq!(
        key(&mut app, KeyCode::Char('x')),
        UiAction::Run("task-20260613-auth-refactor".into())
    );
}

#[test]
fn s_opens_start_mode_picker() {
    // s on the inbox task opens the picker; each key maps to a start mode
    let cases = [
        (KeyCode::Char('s'), StartMode::Single),
        (KeyCode::Char('w'), StartMode::Worktree),
        (KeyCode::Char('b'), StartMode::Branch),
    ];
    for (k, mode) in cases {
        let mut app = App::new(rows());
        assert_eq!(key(&mut app, KeyCode::Char('s')), UiAction::None);
        assert!(app.starting);
        let screen = draw(&app);
        assert!(screen.contains("start settings-cleanup:"));
        assert_eq!(
            key(&mut app, k),
            UiAction::Start("task-20260613-settings-cleanup".into(), mode)
        );
        assert!(!app.starting);
    }

    // any other key cancels the picker
    let mut app = App::new(rows());
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(key(&mut app, KeyCode::Char('x')), UiAction::None);
    assert!(!app.starting);
}

#[test]
fn r_opens_pr_with_worktree_or_branch_mode() {
    // r on an inbox task opens the worktree/branch picker; w → worktree-backed PR
    let mut app = App::new(rows());
    assert_eq!(key(&mut app, KeyCode::Char('r')), UiAction::None);
    assert!(app.pring);
    let screen = draw(&app);
    assert!(screen.contains("PR for settings-cleanup:"));
    let action = key(&mut app, KeyCode::Char('w'));
    assert_eq!(
        action,
        UiAction::CreatePr("task-20260613-settings-cleanup".into(), true)
    );
    assert!(!app.pring);

    // r then b → main-repo branch PR
    let mut app = App::new(rows());
    key(&mut app, KeyCode::Char('r'));
    let action = key(&mut app, KeyCode::Char('b'));
    assert_eq!(
        action,
        UiAction::CreatePr("task-20260613-settings-cleanup".into(), false)
    );

    // any other key cancels the picker
    let mut app = App::new(rows());
    key(&mut app, KeyCode::Char('r'));
    assert_eq!(key(&mut app, KeyCode::Char('x')), UiAction::None);
    assert!(!app.pring);
}

#[test]
fn r_skips_picker_when_task_already_has_a_branch() {
    // a task already backed by a worktree/branch reuses it, so r opens the PR
    // straight away instead of re-asking worktree vs branch.
    let mut with_branch = rows();
    with_branch[1].branch = Some("auth-refactor".into());
    let mut app = App::new(with_branch);
    key(&mut app, KeyCode::Char('j')); // select the active auth-refactor task
    assert_eq!(app.selected_task().unwrap().slug, "auth-refactor");
    let action = key(&mut app, KeyCode::Char('r'));
    assert_eq!(
        action,
        UiAction::CreatePr("task-20260613-auth-refactor".into(), false)
    );
    assert!(!app.pring);
}

#[test]
fn r_pushes_when_pr_attached_and_refuses_finished() {
    // a task that already has a PR → r pushes the managed section to it
    let mut with_pr = rows();
    with_pr[0].github_pr = Some(7);
    let mut app = App::new(with_pr);
    assert_eq!(
        key(&mut app, KeyCode::Char('r')),
        UiAction::PushPr("task-20260613-settings-cleanup".into())
    );
    assert!(!app.pring);

    // a done task with no PR can't open one
    let mut app = App::new(rows());
    for _ in 0..2 {
        key(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.selected_task().unwrap().slug, "old-thing");
    assert_eq!(key(&mut app, KeyCode::Char('r')), UiAction::None);
    assert!(!app.pring);
    assert!(app.message.contains("inbox/active"));
}

#[test]
fn error_popup_shows_and_swallows_next_key() {
    let mut app = App::new(rows());
    app.popup = Some("branch 'rein/fix' already exists — git branch -D rein/fix".into());
    app.popup_error = true;
    let screen = draw(&app);
    assert!(screen.contains("error — press any key to dismiss"));
    assert!(screen.contains("already exists"));
    // any key dismisses the popup and is otherwise consumed (no action)
    assert_eq!(key(&mut app, KeyCode::Char('j')), UiAction::None);
    assert!(app.popup.is_none());

    // while open, the popup intercepts even quit
    app.popup = Some("boom".into());
    assert_eq!(key(&mut app, KeyCode::Char('q')), UiAction::None);
    assert!(app.popup.is_none());

    // a non-error (run) popup uses the neutral title, not "error"
    let mut app = App::new(rows());
    app.popup = Some("running task — backgrounded · abcd1234".into());
    app.popup_error = false;
    let screen = draw(&app);
    assert!(screen.contains("run — press any key to dismiss"));
    assert!(screen.contains("backgrounded"));
}

#[test]
fn run_state_shows_in_meta_and_list() {
    let mut with_run = rows();
    with_run[1].run_state = Some("working".into()); // auth-refactor (active)
    let mut app = App::new(with_run);
    key(&mut app, KeyCode::Char('j')); // select the running task
    assert_eq!(app.selected_task().unwrap().slug, "auth-refactor");
    let screen = draw(&app);
    assert!(screen.contains("run: "), "meta should show a run line");
    assert!(screen.contains("running"), "working state renders as 'running'");
    assert!(screen.contains("●"), "a live run shows a dot in the list");
}

#[test]
fn meta_pane_shows_frontmatter() {
    let mut row = rows();
    row[0].branch = Some("settings-cleanup".into());
    row[0].github_issue = Some(41);
    row[0].github_pr = Some(7);
    row[0].tags = vec!["ui".into(), "cleanup".into()];
    row[0].created_at = "2026-06-13T10:00:00+09:00".into();
    row[0].updated_at = "2026-06-14T12:00:00+09:00".into();
    let app = App::new(row); // selection defaults to the first task
    let screen = draw(&app);
    assert!(screen.contains("meta"));
    assert!(screen.contains("settings-cleanup"));
    assert!(screen.contains("#41"));
    assert!(screen.contains("#7"));
    assert!(screen.contains("ui, cleanup"));
    assert!(screen.contains("2026-06-13")); // created date, trimmed to the day
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
    assert!(screen.contains("D delete"));
    assert!(screen.contains("P project"));
    assert!(screen.contains("i issue"));
    assert!(screen.contains("r PR"));
    assert!(screen.contains("y copy dir"));
    assert!(screen.contains("x run"));
}

#[test]
fn i_pushes_when_issue_attached_else_creates() {
    // no issue yet → i begins issue creation (event loop offers a project picker)
    let mut app = App::new(rows());
    assert_eq!(
        key(&mut app, KeyCode::Char('i')),
        UiAction::Issue("task-20260613-settings-cleanup".into())
    );

    // an attached issue → i pushes the managed section to it
    let mut with_issue = rows();
    with_issue[0].github_issue = Some(41);
    let mut app = App::new(with_issue);
    assert_eq!(
        key(&mut app, KeyCode::Char('i')),
        UiAction::PushIssue("task-20260613-settings-cleanup".into())
    );
}

#[test]
fn y_copies_working_dir_worktree_then_repo() {
    // a worktree-backed task yanks its worktree path
    let mut row = rows();
    row[0].worktree = Some("/store/worktrees/settings-cleanup".into());
    row[0].repo_dir = Some(PathBuf::from("/repo"));
    let mut app = App::new(row);
    assert_eq!(
        key(&mut app, KeyCode::Char('y')),
        UiAction::CopyDir(PathBuf::from("/store/worktrees/settings-cleanup"))
    );

    // a plain branch/single task with no worktree falls back to the main repo
    let mut row = rows();
    row[0].repo_dir = Some(PathBuf::from("/repo"));
    let mut app = App::new(row);
    assert_eq!(
        key(&mut app, KeyCode::Char('y')),
        UiAction::CopyDir(PathBuf::from("/repo"))
    );

    // nothing known → no action, a message instead
    let mut app = App::new(rows());
    assert_eq!(key(&mut app, KeyCode::Char('y')), UiAction::None);
    assert!(app.message.contains("no working directory"));
}

#[test]
fn meta_shows_worktree_vs_branch_mode_and_dir() {
    // worktree-backed task: branch line tagged (worktree), dir = the worktree
    let mut row = rows();
    row[0].branch = Some("settings-cleanup".into());
    row[0].worktree = Some("/store/worktrees/settings-cleanup".into());
    let app = App::new(row);
    let screen = draw(&app);
    assert!(screen.contains("(worktree)"));
    assert!(screen.contains("/store/worktrees/settings-cleanup"));

    // plain branch task: tagged (branch), dir = the main repo
    let mut row = rows();
    row[0].branch = Some("settings-cleanup".into());
    row[0].repo_dir = Some(PathBuf::from("/repo"));
    let app = App::new(row);
    let screen = draw(&app);
    assert!(screen.contains("(branch)"));
}

#[test]
fn status_message_clears_on_next_key() {
    // a sticky message (the "ok" case) must not pin itself over the hint forever:
    // the next key press clears it so the keybindings come back
    let mut app = App::new(rows());
    app.message = "ok".into();
    // a navigation key clears the stale message without setting a new one
    key(&mut app, KeyCode::Char('j'));
    assert!(app.message.is_empty());
    let screen = draw(&app);
    assert!(screen.contains("i issue"), "hint returns after the message clears");
}

#[test]
fn issue_project_picker_selects_and_cancels() {
    // simulate the event loop having fetched two boards and opened the picker
    let mut app = App::new(rows());
    app.issuing = true;
    app.issue_target = Some("task-20260613-settings-cleanup".into());
    app.issue_projects = vec!["Roadmap".into(), "Bugs".into()];
    app.issue_sel = 0;
    let screen = draw(&app);
    assert!(screen.contains("— no project —"));
    assert!(screen.contains("Roadmap"));

    // row 0 (no project) → create without a board
    let action = key(&mut app, KeyCode::Enter);
    assert_eq!(
        action,
        UiAction::IssueWithProject("task-20260613-settings-cleanup".into(), None)
    );
    assert!(!app.issuing);

    // j to "Roadmap" (row 1) then Enter → file onto that board
    let mut app = App::new(rows());
    app.issuing = true;
    app.issue_target = Some("task-20260613-settings-cleanup".into());
    app.issue_projects = vec!["Roadmap".into(), "Bugs".into()];
    key(&mut app, KeyCode::Char('j'));
    let action = key(&mut app, KeyCode::Enter);
    assert_eq!(
        action,
        UiAction::IssueWithProject(
            "task-20260613-settings-cleanup".into(),
            Some("Roadmap".into())
        )
    );

    // Esc cancels issue creation entirely
    let mut app = App::new(rows());
    app.issuing = true;
    app.issue_target = Some("task-20260613-settings-cleanup".into());
    app.issue_projects = vec!["Roadmap".into()];
    assert_eq!(key(&mut app, KeyCode::Esc), UiAction::None);
    assert!(!app.issuing);
    assert!(app.issue_target.is_none());
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

    // a normal checked item is a deep green, an open item stays yellow
    let done = lines.iter().find(|l| text(l).contains("done")).unwrap();
    assert_eq!(done.style.fg, Some(Color::Rgb(0, 128, 0)));
    let open = lines.iter().find(|l| text(l).contains("open")).unwrap();
    assert_eq!(open.style.fg, Some(Color::Yellow));
}
