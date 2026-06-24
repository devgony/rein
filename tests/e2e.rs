use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Env {
    _tmp: TempDir,
    home: PathBuf,
    repo: PathBuf,
    bin_dir: PathBuf,
}

fn setup() -> Env {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".local/share")).unwrap();
    fs::write(
        home.join(".gitconfig"),
        "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n",
    )
    .unwrap();
    let repo = tmp.path().join("proj");
    fs::create_dir_all(&repo).unwrap();
    git(&home, &repo, &["init"]);
    fs::write(repo.join("README.md"), "hi").unwrap();
    git(&home, &repo, &["add", "."]);
    git(&home, &repo, &["commit", "-m", "init"]);
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    Env {
        _tmp: tmp,
        home,
        repo,
        bin_dir,
    }
}

fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn rein(env: &Env, cwd: &Path) -> Command {
    let mut c = Command::cargo_bin("rein").unwrap();
    c.current_dir(cwd)
        .env("HOME", &env.home)
        .env("XDG_DATA_HOME", env.home.join(".local/share"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("EDITOR", "true")
        .env_remove("REIN_TASK")
        .env_remove("REIN_ROOT")
        .env_remove("REIN_GH");
    c
}

fn init(env: &Env) {
    rein(env, &env.repo).arg("init").assert().success();
}

fn store_root(env: &Env) -> PathBuf {
    let key = git(&env.home, &env.repo, &["config", "--get", "rein.store"]);
    env.home.join(".local/share/rein").join(key)
}

/// Add a bare `origin` remote and push `main`, so PR creation can push branches.
fn add_origin(env: &Env) {
    git(
        &env.home,
        env._tmp.path(),
        &["init", "--bare", "origin.git"],
    );
    let url = env._tmp.path().join("origin.git");
    git(
        &env.home,
        &env.repo,
        &["remote", "add", "origin", url.to_str().unwrap()],
    );
    git(&env.home, &env.repo, &["push", "origin", "main"]);
}

/// Write a file and commit it in `dir` (a repo or worktree).
fn commit_in(env: &Env, dir: &Path, file: &str, msg: &str) {
    fs::write(dir.join(file), "x").unwrap();
    git(&env.home, dir, &["add", "."]);
    git(&env.home, dir, &["commit", "-m", msg]);
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {}", p.display(), e))
}

fn task_id(env: &Env, status: &str, slug: &str) -> String {
    let content = read(&store_root(env).join(status).join(format!("{}.md", slug)));
    content
        .lines()
        .find_map(|l| l.strip_prefix("id: "))
        .expect("no id in frontmatter")
        .to_string()
}

/// Seed checklist items into a fresh task doc (simulating human edits in $EDITOR).
fn seed_items(env: &Env, slug: &str) {
    let path = store_root(env).join("inbox").join(format!("{}.md", slug));
    let content = read(&path)
        .replace(
            "## Tasks\n",
            "## Tasks\n\n- [ ] Do thing one\n- [ ] Add tests later\n",
        )
        .replace("## Validation\n", "## Validation\n\n- [ ] Tests pass\n");
    fs::write(&path, content).unwrap();
}

// ---------------------------------------------------------------------------
// Fake gh transport
// ---------------------------------------------------------------------------

const FAKE_GH: &str = r#"#!/bin/sh
[ -n "$GH_LOG" ] && printf '%s\n' "$*" >> "$GH_LOG"
case "$1_$2" in
  issue_create)
    if [ -n "$GH_CREATE_BODY" ]; then cat - > "$GH_CREATE_BODY"; else cat - >/dev/null; fi
    echo "https://github.com/o/r/issues/${GH_ISSUE_NUMBER:-41}"
    ;;
  issue_view)
    cat "$GH_ISSUE_VIEW"
    ;;
  issue_edit)
    cat - > "${GH_EDIT_BODY:-/dev/null}"
    ;;
  issue_list)
    cat "$GH_ISSUE_LIST"
    ;;
  issue_close|issue_comment|label_create)
    ;;
  pr_create)
    if [ -n "$GH_PR_CREATE_BODY" ]; then cat - > "$GH_PR_CREATE_BODY"; else cat - >/dev/null; fi
    echo "https://github.com/o/r/pull/${GH_PR_NUMBER:-7}"
    ;;
  pr_view)
    cat "$GH_PR_VIEW"
    ;;
  pr_edit)
    cat - > "${GH_PR_EDIT_BODY:-/dev/null}"
    ;;
  *)
    echo "fake gh: unhandled $*" >&2
    exit 1
    ;;
esac
"#;

struct FakeGh {
    bin: PathBuf,
    log: PathBuf,
    create_body: PathBuf,
    issue_view: PathBuf,
    edit_body: PathBuf,
    issue_list: PathBuf,
    pr_view: PathBuf,
    pr_edit_body: PathBuf,
    pr_create_body: PathBuf,
}

fn fake_gh(env: &Env) -> FakeGh {
    use std::os::unix::fs::PermissionsExt;
    let bin = env.bin_dir.join("gh");
    fs::write(&bin, FAKE_GH).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    FakeGh {
        bin,
        log: env.bin_dir.join("gh.log"),
        create_body: env.bin_dir.join("create_body.txt"),
        issue_view: env.bin_dir.join("issue_view.json"),
        edit_body: env.bin_dir.join("edit_body.txt"),
        issue_list: env.bin_dir.join("issue_list.json"),
        pr_view: env.bin_dir.join("pr_view.json"),
        pr_edit_body: env.bin_dir.join("pr_edit_body.txt"),
        pr_create_body: env.bin_dir.join("pr_create_body.txt"),
    }
}

impl FakeGh {
    fn apply(&self, cmd: &mut Command) {
        cmd.env("REIN_GH", &self.bin)
            .env("GH_LOG", &self.log)
            .env("GH_CREATE_BODY", &self.create_body)
            .env("GH_ISSUE_VIEW", &self.issue_view)
            .env("GH_EDIT_BODY", &self.edit_body)
            .env("GH_ISSUE_LIST", &self.issue_list)
            .env("GH_PR_VIEW", &self.pr_view)
            .env("GH_PR_EDIT_BODY", &self.pr_edit_body)
            .env("GH_PR_CREATE_BODY", &self.pr_create_body);
    }
    fn set_issue_view_body(&self, body: &str) {
        fs::write(
            &self.issue_view,
            serde_json::json!({ "body": body }).to_string(),
        )
        .unwrap();
    }
    fn set_pr_view_body(&self, body: &str) {
        fs::write(
            &self.pr_view,
            serde_json::json!({ "body": body }).to_string(),
        )
        .unwrap();
    }
    fn log_text(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Phase 1: local task journal
// ---------------------------------------------------------------------------

#[test]
fn init_creates_store_and_config() {
    let env = setup();
    rein(&env, &env.repo)
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("store:"));
    let root = store_root(&env);
    for d in ["inbox", "active", "done", "canceled", "conflicts", "state"] {
        assert!(root.join(d).is_dir(), "missing {}", d);
    }
    let meta = read(&root.join("meta.json"));
    assert!(meta.contains("common_dir"));

    // idempotent: same key on re-init
    let key1 = git(&env.home, &env.repo, &["config", "--get", "rein.store"]);
    rein(&env, &env.repo).arg("init").assert().success();
    let key2 = git(&env.home, &env.repo, &["config", "--get", "rein.store"]);
    assert_eq!(key1, key2);
}

#[test]
fn uninitialized_repo_errors() {
    let env = setup();
    rein(&env, &env.repo)
        .args(["list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("rein init"));
}

#[test]
fn init_skill_scaffold() {
    let env = setup();
    rein(&env, &env.repo)
        .args(["init", "--skill"])
        .assert()
        .success();
    let skill = read(&env.repo.join(".claude/skills/run-rein-task/SKILL.md"));
    assert!(skill.contains("rein check <item-id>"));
    assert!(skill.contains("rein current --path"));
    let agent_skill = read(&env.repo.join(".agents/skills/run-rein-task/SKILL.md"));
    assert!(agent_skill.contains("rein log \"<text>\" --item <item-id>"));
}

#[test]
fn new_creates_inbox_doc_with_slug_collision() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "Settings Cleanup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("settings-cleanup.md"));
    rein(&env, &env.repo)
        .args(["new", "Settings Cleanup!"])
        .assert()
        .success()
        .stdout(predicate::str::contains("settings-cleanup-2.md"));

    let doc = read(&store_root(&env).join("inbox/settings-cleanup.md"));
    assert!(doc.starts_with("---\n"));
    assert!(doc.contains("title: Settings Cleanup"));
    assert!(doc.contains("status: inbox"));
    assert!(doc.contains("## Goal"));
    assert!(doc.contains("## Agent Log"));

    // state file exists for the new task
    let id = task_id(&env, "inbox", "settings-cleanup");
    assert!(store_root(&env)
        .join("state")
        .join(format!("{}.json", id))
        .is_file());
}

#[test]
fn list_filters_by_status() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "alpha"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["new", "beta"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "alpha"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["list", "--status", "inbox"])
        .assert()
        .success()
        .stdout(predicate::str::contains("beta").and(predicate::str::contains("alpha").not()));
    rein(&env, &env.repo)
        .args(["list", "--status", "active"])
        .assert()
        .success()
        .stdout(predicate::str::contains("alpha"));
}

#[test]
fn start_claims_task_single_mode() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo task"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "demo-task"])
        .assert()
        .success();

    let root = store_root(&env);
    assert!(!root.join("inbox/demo-task.md").exists());
    let doc = read(&root.join("active/demo-task.md"));
    assert!(doc.contains("status: active"));

    let id = task_id(&env, "active", "demo-task");
    assert_eq!(read(&root.join("current")).trim(), id);

    // claiming again fails (already active)
    rein(&env, &env.repo)
        .args(["start", "demo-task"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already active"));

    // current resolves
    rein(&env, &env.repo)
        .arg("current")
        .assert()
        .success()
        .stdout(predicate::str::contains(&id));
    rein(&env, &env.repo)
        .args(["current", "--path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("active/demo-task.md"));
}

#[test]
fn start_worktree_binds_task_by_cwd() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "feat one"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "feat-one", "--worktree"])
        .assert()
        .success()
        .stdout(predicate::str::contains("worktree:"));

    let id1 = task_id(&env, "active", "feat-one");
    let wt = store_root(&env).join("worktrees/feat-one");
    assert!(wt.is_dir(), "worktree not created");

    // pointer file in the worktree's git-dir holds the task id
    let pointer = env.repo.join(".git/worktrees/feat-one/rein-task");
    assert_eq!(read(&pointer).trim(), id1);

    // resolution from the worktree cwd → its task; main repo has no current
    rein(&env, &wt)
        .arg("current")
        .assert()
        .success()
        .stdout(predicate::str::contains(&id1));
    rein(&env, &env.repo).arg("current").assert().failure();

    // a second task started in single mode does not disturb the worktree binding
    rein(&env, &env.repo)
        .args(["new", "feat two"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "feat-two"])
        .assert()
        .success();
    let id2 = task_id(&env, "active", "feat-two");
    rein(&env, &env.repo)
        .arg("current")
        .assert()
        .success()
        .stdout(predicate::str::contains(&id2));
    rein(&env, &wt)
        .arg("current")
        .assert()
        .success()
        .stdout(predicate::str::contains(&id1));

    // branch recorded in frontmatter
    let doc = read(&store_root(&env).join("active/feat-one.md"));
    assert!(doc.contains("branch: feat-one"));
}

#[test]
fn mutations_check_uncheck_log_fail() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    rein(&env, &env.repo)
        .args(["start", "demo"])
        .assert()
        .success();
    let root = store_root(&env);
    let path = root.join("active/demo.md");

    // mutation assigns stable integer IDs on the spot — no GitHub needed
    rein(&env, &env.repo)
        .args(["check", "1"])
        .assert()
        .success();
    assert!(read(&path).contains("- [x] <!-- task:1 --> Do thing one"));

    rein(&env, &env.repo)
        .args(["uncheck", "1"])
        .assert()
        .success();
    assert!(read(&path).contains("- [ ] <!-- task:1 --> Do thing one"));

    // log is item-scoped: --item ties the entry to a checklist item and the
    // entry is written with the `Task<id>` reference the UI's per-item log matches
    rein(&env, &env.repo)
        .args(["log", "implemented the thing", "--item", "1"])
        .assert()
        .success();
    let doc = read(&path);
    assert!(doc.contains("Task1: implemented the thing"));
    let log_pos = doc.find("## Agent Log").unwrap();
    assert!(doc.find("implemented the thing").unwrap() > log_pos);

    // log without --item is refused (use `rein note` for an un-itemized entry)
    rein(&env, &env.repo)
        .args(["log", "no item given"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--item"));

    // a bad item id fails loudly with the available ids
    rein(&env, &env.repo)
        .args(["log", "bad", "--item", "99"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("available items"));

    rein(&env, &env.repo)
        .args(["fail", "1", "--reason", "blocked by upstream"])
        .assert()
        .success();
    let doc = read(&path);
    // the blocker entry is tagged Task<id> so it shows under the item in the UI
    assert!(doc.contains("Task1: FAIL blocked by upstream"));
    // fail resolves the item: checked box + failed sentinel + ~~strike~~ ❌
    assert!(doc.contains("- [x] <!-- task:1 --> <!-- failed --> ~~Do thing one~~ ❌"));

    // retry reopens it: back to an unchecked, undecorated item + a RETRY log line
    rein(&env, &env.repo)
        .args(["retry", "1"])
        .assert()
        .success();
    let doc = read(&path);
    assert!(doc.contains("- [ ] <!-- task:1 --> Do thing one"));
    assert!(doc.contains("Task1: RETRY"));

    // unknown item errors and lists what's available
    rein(&env, &env.repo)
        .args(["check", "99"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("available items").and(predicate::str::contains("1, 2, 3")),
        );
}

#[test]
fn local_check_assigns_integer_ids_without_github() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo"); // 3 items, no IDs, fully offline
    rein(&env, &env.repo)
        .args(["start", "demo"])
        .assert()
        .success();

    // single integer sequence across Tasks(1,2) and Validation(3)
    rein(&env, &env.repo)
        .args(["check", "2"])
        .assert()
        .success();
    let doc = read(&store_root(&env).join("active/demo.md"));
    assert!(doc.contains("- [ ] <!-- task:1 --> Do thing one"));
    assert!(doc.contains("- [x] <!-- task:2 --> Add tests later"));
    assert!(doc.contains("- [ ] <!-- task:3 --> Tests pass"));
}

#[test]
fn ids_are_stable_under_reorder() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    rein(&env, &env.repo)
        .args(["start", "demo"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["check", "1"])
        .assert()
        .success(); // assigns 1,2,3
    let path = store_root(&env).join("active/demo.md");

    // a human reorders items (task:2 above task:1) and inserts a new one,
    // simulating an edit in $EDITOR / Obsidian
    let doc = read(&path);
    let reordered = doc.replace(
        "- [x] <!-- task:1 --> Do thing one\n- [ ] <!-- task:2 --> Add tests later",
        "- [ ] new top item\n- [ ] <!-- task:2 --> Add tests later\n- [x] <!-- task:1 --> Do thing one",
    );
    assert_ne!(reordered, doc, "reorder replacement must apply");
    fs::write(&path, reordered).unwrap();

    // checking 2 still hits "Add tests later" despite the move — id is identity, not position
    rein(&env, &env.repo)
        .args(["check", "2"])
        .assert()
        .success();
    let doc = read(&path);
    assert!(doc.contains("- [x] <!-- task:2 --> Add tests later"));
    // the inserted item gets the next integer (4), never a reused one
    assert!(doc.contains("<!-- task:4 --> new top item"));
}

#[test]
fn check_with_task_arg_gives_helpful_error() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    rein(&env, &env.repo)
        .args(["start", "demo"])
        .assert()
        .success();
    let id = task_id(&env, "active", "demo");

    // the exact mistake from the bug report: passing a task id to `check`
    rein(&env, &env.repo)
        .args(["check", &id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is a task, not an item"));
}

#[test]
fn open_assigns_ids_after_editor() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo"); // items lack IDs
                              // EDITOR=true is a no-op edit; open must still heal IDs on return
    rein(&env, &env.repo)
        .args(["open", "demo"])
        .assert()
        .success();
    let doc = read(&store_root(&env).join("inbox/demo.md"));
    assert!(doc.contains("<!-- task:1 -->"));
    assert!(doc.contains("<!-- task:2 -->"));
    assert!(doc.contains("<!-- task:3 -->"));
}

#[test]
fn status_lists_items_with_numbers() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    rein(&env, &env.repo)
        .args(["start", "demo"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .arg("status")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("items (demo):")
                .and(predicate::str::contains("Do thing one"))
                .and(predicate::str::contains("Tests pass")),
        );
}

#[test]
fn todo_lists_unchecked_items_grouped_by_section() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo"); // Tasks: 1,2 · Validation: 3
    rein(&env, &env.repo)
        .args(["start", "demo"])
        .assert()
        .success();

    // default: only unchecked items, grouped under their section headings
    rein(&env, &env.repo).arg("todo").assert().success().stdout(
        predicate::str::contains("## Tasks")
            .and(predicate::str::contains("## Validation"))
            .and(predicate::str::contains("1\tDo thing one"))
            .and(predicate::str::contains("2\tAdd tests later"))
            .and(predicate::str::contains("3\tTests pass")),
    );

    // checked items drop out of the list
    rein(&env, &env.repo)
        .args(["check", "2"])
        .assert()
        .success();
    rein(&env, &env.repo).arg("todo").assert().success().stdout(
        predicate::str::contains("Do thing one")
            .and(predicate::str::contains("Tests pass"))
            .and(predicate::str::contains("Add tests later").not()),
    );

    // --all shows every item with its state
    rein(&env, &env.repo)
        .args(["todo", "--all"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("2\t[x] Add tests later")
                .and(predicate::str::contains("1\t[ ] Do thing one")),
        );

    // --task targets a specific task regardless of resolution
    rein(&env, &env.repo)
        .args(["new", "other"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["todo", "--task", "demo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Do thing one"));
}

#[test]
fn fail_drops_item_from_todo_until_retried() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo"); // Tasks: 1,2 · Validation: 3
    rein(&env, &env.repo)
        .args(["start", "demo"])
        .assert()
        .success();

    // fail item 1 → it drops out of the default todo list (won't be re-attempted)
    rein(&env, &env.repo)
        .args(["fail", "1", "--reason", "blocked"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .arg("todo")
        .assert()
        .success()
        .stdout(predicate::str::contains("Do thing one").not());

    // --all surfaces it, marked failed with [!]
    rein(&env, &env.repo)
        .args(["todo", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1\t[!] Do thing one"));

    // retry reopens it → back on the default list
    rein(&env, &env.repo)
        .args(["retry", "1"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .arg("todo")
        .assert()
        .success()
        .stdout(predicate::str::contains("1\tDo thing one"));

    // retrying a non-failed item is refused (never silently unchecks done work)
    rein(&env, &env.repo)
        .args(["retry", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not failed"));
}

#[test]
fn mutation_gate_refuses_ambiguous_current() {
    let env = setup();
    init(&env);
    for title in ["one", "two"] {
        rein(&env, &env.repo)
            .args(["new", title])
            .assert()
            .success();
        let path = store_root(&env).join("inbox").join(format!("{}.md", title));
        let content =
            read(&path).replace("## Tasks\n", "## Tasks\n\n- [ ] <!-- task:1 --> Do thing\n");
        fs::write(path, content).unwrap();
    }
    // two active tasks, current points at the last
    rein(&env, &env.repo)
        .args(["start", "one"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "two"])
        .assert()
        .success();

    rein(&env, &env.repo)
        .args(["note", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ambiguous").and(predicate::str::contains("--task")));

    // explicit --task passes the gate
    rein(&env, &env.repo)
        .args(["note", "hello", "--task", "one"])
        .assert()
        .success();
    assert!(read(&store_root(&env).join("active/one.md")).contains("hello"));

    // log uses --item for the checklist id and --task for document selection
    rein(&env, &env.repo)
        .args(["log", "progress", "--item", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ambiguous").and(predicate::str::contains("--task")));
    rein(&env, &env.repo)
        .args(["log", "progress", "--item", "1", "--task", "two"])
        .assert()
        .success();
    assert!(read(&store_root(&env).join("active/two.md")).contains("Task1: progress"));
    assert!(!read(&store_root(&env).join("active/one.md")).contains("Task1: progress"));

    // query commands are not gated
    rein(&env, &env.repo).arg("current").assert().success();
}

#[test]
fn resolution_order_flag_env_current() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "one"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["new", "two"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "one"])
        .assert()
        .success();
    let id_one = task_id(&env, "active", "one");
    rein(&env, &env.repo)
        .args(["use", "two"])
        .assert()
        .success();
    let id_two = task_id(&env, "inbox", "two");
    assert_eq!(read(&store_root(&env).join("current")).trim(), id_two);

    // REIN_TASK env (#3) beats current file (#4)
    rein(&env, &env.repo)
        .env("REIN_TASK", &id_one)
        .arg("current")
        .assert()
        .success()
        .stdout(predicate::str::contains(&id_one));

    // --task flag (#1) beats env (#3): note lands in 'two'
    rein(&env, &env.repo)
        .env("REIN_TASK", &id_one)
        .args(["note", "flag wins", "--task", "two"])
        .assert()
        .success();
    assert!(read(&store_root(&env).join("inbox/two.md")).contains("flag wins"));
}

#[test]
fn use_rebinds_worktree_pointer() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "one"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["new", "two"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "one", "--worktree"])
        .assert()
        .success();
    let wt = store_root(&env).join("worktrees/one");
    let id_two = task_id(&env, "inbox", "two");

    // inside a bound worktree, `use` rewrites the pointer, not the current file
    rein(&env, &wt).args(["use", "two"]).assert().success();
    assert_eq!(
        read(&env.repo.join(".git/worktrees/one/rein-task")).trim(),
        id_two
    );
    assert!(!store_root(&env).join("current").exists());
}

#[test]
fn done_preflight_and_worktree_cleanup() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "dirty job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "dirty-job", "--worktree"])
        .assert()
        .success();
    let wt = store_root(&env).join("worktrees/dirty-job");

    // dirty worktree → pre-flight refuses, nothing moved
    fs::write(wt.join("junk.txt"), "wip").unwrap();
    rein(&env, &env.repo)
        .args(["done", "dirty-job"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted"));
    assert!(store_root(&env).join("active/dirty-job.md").exists());

    // commit → clean → done succeeds, doc lands in done/YYYY-MM, worktree removed
    git(&env.home, &wt, &["add", "."]);
    git(&env.home, &wt, &["commit", "-m", "wip"]);
    rein(&env, &env.repo)
        .args(["done", "dirty-job"])
        .assert()
        .success();
    assert!(!wt.exists(), "worktree should be removed");
    let month = chrono::Local::now().format("%Y-%m").to_string();
    assert!(store_root(&env)
        .join("done")
        .join(&month)
        .join("dirty-job.md")
        .exists());
    let doc = read(
        &store_root(&env)
            .join("done")
            .join(&month)
            .join("dirty-job.md"),
    );
    assert!(doc.contains("status: done"));
}

#[test]
fn done_keep_worktree() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "keepwt"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "keepwt", "--worktree"])
        .assert()
        .success();
    let wt = store_root(&env).join("worktrees/keepwt");
    fs::write(wt.join("junk.txt"), "wip").unwrap();
    rein(&env, &env.repo)
        .args(["done", "keepwt", "--keep-worktree"])
        .assert()
        .success();
    assert!(wt.exists(), "worktree should be kept");
    let month = chrono::Local::now().format("%Y-%m").to_string();
    assert!(store_root(&env)
        .join("done")
        .join(&month)
        .join("keepwt.md")
        .exists());
}

#[test]
fn cancel_force_discards_dirty_worktree() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "byebye"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "byebye", "--worktree"])
        .assert()
        .success();
    let wt = store_root(&env).join("worktrees/byebye");
    fs::write(wt.join("junk.txt"), "wip").unwrap();

    rein(&env, &env.repo)
        .args(["cancel", "byebye"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));

    rein(&env, &env.repo)
        .args(["cancel", "byebye", "--force"])
        .assert()
        .success();
    assert!(!wt.exists());
    assert!(store_root(&env).join("canceled/byebye.md").exists());
}

#[test]
fn delete_removes_inbox_task_files() {
    let env = setup();
    init(&env);
    let root = store_root(&env);
    rein(&env, &env.repo)
        .args(["new", "scratch"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "scratch"])
        .assert()
        .success(); // single mode → current pointer
    let id = task_id(&env, "active", "scratch");
    assert_eq!(read(&root.join("current")).trim(), id);

    // delete a task with no worktree → doc, state, and current pointer all gone
    rein(&env, &env.repo)
        .args(["delete", "scratch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted").and(predicate::str::contains(&id)));
    assert!(!root.join("active/scratch.md").exists());
    assert!(!root.join("state").join(format!("{}.json", id)).exists());
    assert!(
        !root.join("current").exists(),
        "current pointer must be cleared"
    );

    // deleting a vanished task errors
    rein(&env, &env.repo)
        .args(["delete", "scratch"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no task matches"));
}

#[test]
fn delete_refuses_dirty_worktree_unless_forced() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "wt task"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "wt-task", "--worktree"])
        .assert()
        .success();
    let wt = store_root(&env).join("worktrees/wt-task");
    fs::write(wt.join("junk.txt"), "wip").unwrap();

    // a dirty worktree blocks deletion without --force; nothing is removed
    rein(&env, &env.repo)
        .args(["delete", "wt-task"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
    assert!(wt.exists(), "worktree must survive a refused delete");
    assert!(store_root(&env).join("active/wt-task.md").exists());

    // --force discards the worktree and removes every record
    let id = task_id(&env, "active", "wt-task");
    rein(&env, &env.repo)
        .args(["delete", "wt-task", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed worktree"));
    assert!(!wt.exists());
    assert!(!store_root(&env).join("active/wt-task.md").exists());
    assert!(!store_root(&env)
        .join("state")
        .join(format!("{}.json", id))
        .exists());
}

#[test]
fn move_transitions_any_direction_without_side_effects() {
    let env = setup();
    init(&env);
    let root = store_root(&env);
    rein(&env, &env.repo)
        .args(["new", "wander"])
        .assert()
        .success();

    // inbox → active is a plain relocation: no current pointer, no worktree
    rein(&env, &env.repo)
        .args(["move", "wander", "active"])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved wander inbox → active"));
    assert!(root.join("active/wander.md").exists());
    assert!(!root.join("inbox/wander.md").exists());
    assert!(read(&root.join("active/wander.md")).contains("status: active"));
    assert!(
        !root.join("current").exists(),
        "move must not claim current"
    );

    // active → done, then the previously-impossible backward hop done → inbox
    rein(&env, &env.repo)
        .args(["move", "wander", "done"])
        .assert()
        .success();
    let month = chrono::Local::now().format("%Y-%m").to_string();
    assert!(root.join("done").join(&month).join("wander.md").exists());

    rein(&env, &env.repo)
        .args(["move", "wander", "inbox"])
        .assert()
        .success()
        .stdout(predicate::str::contains("done → inbox"));
    assert!(root.join("inbox/wander.md").exists());
    assert!(read(&root.join("inbox/wander.md")).contains("status: inbox"));

    // state path cache follows the file
    let id = task_id(&env, "inbox", "wander");
    let st = read(&root.join("state").join(format!("{}.json", id)));
    assert!(st.contains("inbox/wander.md"));

    // guards: same-state and unknown-state are rejected
    rein(&env, &env.repo)
        .args(["move", "wander", "inbox"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already inbox"));
    rein(&env, &env.repo)
        .args(["move", "wander", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown status"));
}

#[test]
fn doctor_rebuilds_state_and_fixes_drift() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "alpha"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["new", "beta"])
        .assert()
        .success();
    let root = store_root(&env);
    let id_alpha = task_id(&env, "inbox", "alpha");

    // simulate damage: drop state/, stale current, manual move with stale frontmatter
    fs::remove_dir_all(root.join("state")).unwrap();
    fs::write(root.join("current"), "task-00000000-ghost\n").unwrap();
    fs::rename(root.join("inbox/alpha.md"), root.join("active/alpha.md")).unwrap();

    rein(&env, &env.repo)
        .arg("doctor")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("fixed status: alpha -> active")
                .and(predicate::str::contains("cleared stale current"))
                .and(predicate::str::contains("2 tasks ok")),
        );

    // state regenerated for both, frontmatter fixed, current cleared
    assert!(root
        .join("state")
        .join(format!("{}.json", id_alpha))
        .is_file());
    assert!(read(&root.join("active/alpha.md")).contains("status: active"));
    assert!(!root.join("current").exists());
}

#[test]
fn root_prints_store_path() {
    let env = setup();
    init(&env);
    let expected = store_root(&env);
    rein(&env, &env.repo)
        .arg("root")
        .assert()
        .success()
        .stdout(predicate::str::contains(expected.to_str().unwrap()));
}

#[test]
fn status_reports_counts() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "one"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "one"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .arg("status")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("via current")
                .and(predicate::str::contains("active    1"))
                .and(predicate::str::contains("inbox     0")),
        );
}

// ---------------------------------------------------------------------------
// Phase 2: shared inbox via GitHub issues (fake gh)
// ---------------------------------------------------------------------------

#[test]
fn issue_publishes_projection_and_assigns_ids() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");

    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("issue #41"));

    // stable integer IDs assigned at the issue touchpoint (single sequence)
    let doc = read(&store_root(&env).join("inbox/demo.md"));
    assert!(doc.contains("- [ ] <!-- task:1 --> Do thing one"));
    assert!(doc.contains("- [ ] <!-- task:2 --> Add tests later"));
    assert!(doc.contains("- [ ] <!-- task:3 --> Tests pass"));
    assert!(doc.contains("github_issue: 41"));

    // published body: markers, content, no frontmatter, no Agent Log
    let body = read(&gh.create_body);
    let id = task_id(&env, "inbox", "demo");
    assert!(body.contains(&format!("<!-- rein:begin {} -->", id)));
    assert!(body.trim_end().ends_with("<!-- rein:end -->"));
    assert!(body.contains("Do thing one"));
    assert!(!body.contains("Agent Log"));
    assert!(!body.contains("id: task-"));

    // label + create were called
    let log = gh.log_text();
    assert!(log.contains("label create rein"));
    assert!(log.contains("issue create --title demo"));

    // synced hash recorded
    let st = read(&store_root(&env).join("state").join(format!("{}.json", id)));
    assert!(st.contains("issue_synced_hash"));

    // publishing twice is refused
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already issue #41"));
}

#[test]
fn issue_with_project_flag_files_onto_board() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");

    // --project passes the board name through to `gh issue create --project`
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo", "--project", "Roadmap"])
        .assert()
        .success()
        .stdout(predicate::str::contains("issue #41"));

    assert!(
        gh.log_text().contains("--project Roadmap"),
        "gh log: {}",
        gh.log_text()
    );
    assert!(read(&store_root(&env).join("inbox/demo.md")).contains("github_issue: 41"));
}

#[test]
fn push_local_change_preserves_human_text() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"]).assert().success();
    rein(&env, &env.repo)
        .args(["use", "demo"])
        .assert()
        .success();

    // remote = exactly what we published, plus human text outside markers
    let published = read(&gh.create_body);
    gh.set_issue_view_body(&format!("human intro\n\n{}\n\nhuman outro", published));

    // local change
    rein(&env, &env.repo)
        .args(["check", "1"])
        .assert()
        .success();

    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.arg("push")
        .assert()
        .success()
        .stdout(predicate::str::contains("issue #41: pushed"));

    let edited = read(&gh.edit_body);
    assert!(edited.contains("human intro"));
    assert!(edited.contains("human outro"));
    assert!(edited.contains("- [x] <!-- task:1 --> Do thing one"));

    // second push: up to date
    gh.set_issue_view_body(&edited);
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.arg("push")
        .assert()
        .success()
        .stdout(predicate::str::contains("up to date"));
}

#[test]
fn pull_applies_remote_change_and_keeps_log() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"]).assert().success();
    rein(&env, &env.repo)
        .args(["use", "demo"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["note", "local progress note"])
        .assert()
        .success();

    // remote edited the managed section (a human reworded an item on GitHub)
    let published = read(&gh.create_body);
    gh.set_issue_view_body(&published.replace("Do thing one", "Do thing one EDITED"));

    // wait: local also changed (the log) — but Agent Log is outside the projection,
    // so local projection hash == base and this is a clean Pull.
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.arg("pull")
        .assert()
        .success()
        .stdout(predicate::str::contains("updated from remote"));

    let doc = read(&store_root(&env).join("inbox/demo.md"));
    assert!(doc.contains("Do thing one EDITED"));
    assert!(
        doc.contains("local progress note"),
        "Agent Log must survive pull"
    );

    // pulling again: up to date
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.arg("pull")
        .assert()
        .success()
        .stdout(predicate::str::contains("up to date"));
}

#[test]
fn conflict_detected_then_resolved() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"]).assert().success();
    rein(&env, &env.repo)
        .args(["use", "demo"])
        .assert()
        .success();

    // both sides diverge
    let published = read(&gh.create_body);
    gh.set_issue_view_body(&published.replace("Do thing one", "Do thing one REMOTE"));
    rein(&env, &env.repo)
        .args(["check", "1"])
        .assert()
        .success();

    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.arg("push")
        .assert()
        .failure()
        .stderr(predicate::str::contains("conflict"));

    let root = store_root(&env);
    let local_backup = read(&root.join("conflicts/demo.local.md"));
    let remote_backup = read(&root.join("conflicts/demo.remote.md"));
    assert!(local_backup.contains("- [x]"));
    assert!(remote_backup.contains("REMOTE"));

    // user resolves locally, force-pushes
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["push", "--resolved"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pushed"));
    assert!(!root.join("conflicts/demo.local.md").exists());
    assert!(!root.join("conflicts/demo.remote.md").exists());
    assert!(read(&gh.edit_body).contains("- [x]"));
}

#[test]
fn pull_inbox_is_idempotent() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);

    // one tool-owned issue (marker carries the id), one human-created issue
    let marker_body =
        "<!-- rein:begin task-20260601-remote-made -->\n\n## Goal\n\nFrom remote\n\n<!-- rein:end -->";
    let list = serde_json::json!([
        { "number": 101, "title": "Remote made", "body": marker_body },
        { "number": 102, "title": "Human Idea", "body": "just an idea, no markers" },
    ]);
    fs::write(&gh.issue_list, list.to_string()).unwrap();

    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.arg("pull-inbox")
        .assert()
        .success()
        .stdout(predicate::str::contains("2 imported"));

    let root = store_root(&env);
    // marker id adopted verbatim — no new id minted
    let doc = read(&root.join("inbox/remote-made.md"));
    assert!(doc.contains("id: task-20260601-remote-made"));
    assert!(doc.contains("github_issue: 101"));
    assert!(doc.contains("From remote"));
    // human issue gets a fresh id
    let doc2 = read(&root.join("inbox/human-idea.md"));
    assert!(doc2.contains("github_issue: 102"));
    assert!(doc2.contains("just an idea"));

    // run again → converges, no duplicates
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.arg("pull-inbox")
        .assert()
        .success()
        .stdout(predicate::str::contains("0 imported"));
    let count = fs::read_dir(root.join("inbox")).unwrap().count();
    assert_eq!(count, 2, "pull-inbox must not duplicate tasks");
}

#[test]
fn attach_issue_links_and_hints() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["use", "demo"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["attach-issue", "55"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rein pull").and(predicate::str::contains("--resolved")));
    assert!(read(&store_root(&env).join("inbox/demo.md")).contains("github_issue: 55"));
}

// ---------------------------------------------------------------------------
// Phase 3: PR body integration (fake gh)
// ---------------------------------------------------------------------------

#[test]
fn attach_pr_push_renders_managed_section_with_folded_log() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    rein(&env, &env.repo)
        .args(["use", "demo"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["note", "agent did a thing"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["attach-pr", "7"])
        .assert()
        .success();

    gh.set_pr_view_body("Reviewer notes stay.\n");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.arg("push")
        .assert()
        .success()
        .stdout(predicate::str::contains("PR #7: pushed"));

    let body = read(&gh.pr_edit_body);
    assert!(body.contains("Reviewer notes stay."));
    assert!(body.contains("<details>"));
    assert!(body.contains("agent did a thing"));
    assert!(body.contains("Do thing one"));
    let id = task_id(&env, "inbox", "demo");
    assert!(body.contains(&format!("<!-- rein:begin {} -->", id)));
}

#[test]
fn start_draft_pr_warns_without_commits() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "feat"])
        .assert()
        .success();
    // a freshly claimed branch has no commits → PR creation warns, none is made
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["start", "feat", "--worktree", "--draft-pr"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no commits on 'feat'"));
    // the task is still claimed + the worktree set up; only the PR is skipped
    assert!(store_root(&env).join("worktrees/feat").is_dir());
    assert!(!read(&store_root(&env).join("active/feat.md")).contains("github_pr: 7"));
}

#[test]
fn pr_inbox_worktree_warns_without_commits() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "feat"])
        .assert()
        .success();
    // the worktree is created under the store, but with no commits the PR warns
    rein(&env, &env.repo)
        .args(["pr", "feat", "--worktree"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no commits on 'feat'")
                .and(predicate::str::contains("rein pr")),
        );
    assert!(
        store_root(&env).join("worktrees/feat").is_dir(),
        "worktree not under store"
    );
    assert!(
        !env._tmp.path().join("proj-wt/feat").exists(),
        "should not litter parent dir"
    );
    assert!(!read(&store_root(&env).join("active/feat.md")).contains("github_pr: 7"));
}

#[test]
fn pr_branch_mode_creates_branch_but_warns_without_commits() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "alpha"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["pr", "alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no commits on 'alpha'"));
    // the branch was created + checked out in the main repo even though PR warned
    assert!(!store_root(&env).join("worktrees/alpha").exists());
    let branch = git(&env.home, &env.repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(branch, "alpha");
}

#[test]
fn pr_pushes_branch_and_opens_draft_when_commits_exist() {
    let env = setup();
    init(&env);
    add_origin(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "beta"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "beta", "--worktree"])
        .assert()
        .success();
    // do work in the worktree so the branch has a commit ahead of main
    commit_in(
        &env,
        &store_root(&env).join("worktrees/beta"),
        "feature.txt",
        "work",
    );
    // active task with commits → push the branch + open the draft PR
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["pr", "beta"])
        .assert()
        .success()
        .stdout(predicate::str::contains("draft PR: #7"));
    assert!(read(&store_root(&env).join("active/beta.md")).contains("github_pr: 7"));
    assert!(gh.log_text().contains("pr create --draft"));
    // the branch was pushed to origin
    let remotes = git(&env.home, &env.repo, &["branch", "-r"]);
    assert!(
        remotes.contains("origin/beta"),
        "branch not pushed: {}",
        remotes
    );
    // a second PR is refused
    let mut c2 = rein(&env, &env.repo);
    gh.apply(&mut c2);
    c2.args(["pr", "beta"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already has PR #7"));
}

#[test]
fn pr_body_is_resolves_when_task_is_issue_linked() {
    let env = setup();
    init(&env);
    add_origin(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "gamma"])
        .assert()
        .success();
    // link an issue to the task, then start it in a worktree with a commit
    rein(&env, &env.repo)
        .args(["use", "gamma"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["attach-issue", "41"])
        .assert()
        .success();
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["start", "gamma", "--worktree"]).assert().success();
    commit_in(
        &env,
        &store_root(&env).join("worktrees/gamma"),
        "feature.txt",
        "work",
    );

    // the issue already holds the managed task description, so the PR body is just
    // GitHub's closing keyword — no duplicated task block
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["pr", "gamma"])
        .assert()
        .success()
        .stdout(predicate::str::contains("draft PR: #7"));
    let body = read(&gh.pr_create_body);
    assert_eq!(body.trim(), "resolves #41", "issue-linked PR body");
    assert!(
        !body.contains("rein:begin"),
        "PR body must not duplicate the managed block"
    );

    // Subsequent pushes keep the PR unmanaged; only the issue surface receives the block.
    gh.set_issue_view_body("issue notes stay.\n");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.arg("push").assert().success().stdout(
        predicate::str::contains("issue #41: pushed")
            .and(predicate::str::contains("PR #7: unmanaged")),
    );
    let log = gh.log_text();
    assert!(log.contains("issue edit 41"), "log: {}", log);
    assert!(!log.contains("pr edit 7"), "log: {}", log);
    assert!(
        !gh.pr_edit_body.exists(),
        "issue-linked PR body must not be edited"
    );
}

#[test]
fn pr_reports_actionable_error_when_branch_exists() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "dup"])
        .assert()
        .success();
    // a leftover branch from an earlier run collides with the task slug
    git(&env.home, &env.repo, &["branch", "dup"]);
    rein(&env, &env.repo)
        .args(["pr", "dup", "--worktree"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("branch 'dup' already exists")
                .and(predicate::str::contains("git branch -D dup")),
        );
}

/// Poll until `path` has non-empty content (the run command is backgrounded, and
/// a shell `>` redirect creates the file empty before the command writes it).
fn wait_for(path: &Path) -> String {
    let mut waited = 0;
    loop {
        if let Ok(s) = fs::read_to_string(path) {
            if !s.is_empty() {
                return s;
            }
        }
        if waited >= 5000 {
            return fs::read_to_string(path).unwrap_or_default();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 50;
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn run_launches_agent_in_worktree_with_task_env() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "job", "--worktree"])
        .assert()
        .success();
    let marker = env.bin_dir.join("run_marker.txt");
    let mut c = rein(&env, &env.repo);
    c.env(
        "REIN_RUN_CMD",
        format!(
            "printf '%s|%s' \"$REIN_TASK\" \"$REIN_DIR\" > {}",
            marker.display()
        ),
    );
    c.args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("running"));
    let content = wait_for(&marker);
    let id = task_id(&env, "active", "job");
    assert!(content.contains(&id), "REIN_TASK not set: {}", content);
    assert!(
        content.contains("worktrees/job"),
        "REIN_DIR not the worktree: {}",
        content
    );
}

#[test]
fn run_without_worktree_uses_repo_root_and_warns() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "solo"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "solo"])
        .assert()
        .success(); // single mode, no worktree
    let marker = env.bin_dir.join("run_marker2.txt");
    let mut c = rein(&env, &env.repo);
    c.env(
        "REIN_RUN_CMD",
        format!("printf '%s' \"$REIN_DIR\" > {}", marker.display()),
    );
    c.args(["run", "solo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("edits are not isolated"));
    let content = wait_for(&marker);
    assert!(
        !content.contains("worktrees/"),
        "should run in the repo root, got: {}",
        content
    );
}

#[test]
fn run_installs_skill_at_user_level_without_touching_repo() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "job", "--worktree"])
        .assert()
        .success();
    let user_skill = env.home.join(".claude/skills/run-rein-task/SKILL.md");
    let wt_skill = store_root(&env).join("worktrees/job/.claude/skills/run-rein-task/SKILL.md");
    assert!(
        !user_skill.exists(),
        "precondition: no user-level skill yet"
    );
    rein(&env, &env.repo)
        .env("REIN_RUN_CMD", "true")
        .args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("installed run-rein-task skill"));
    // installed globally for any worktree, but the repo/worktree stays clean
    assert!(
        user_skill.exists(),
        "skill should be installed at the user level"
    );
    assert!(
        !wt_skill.exists(),
        "run must not add skill files to the repo worktree"
    );
}

#[test]
fn run_captures_bg_session_id_for_logs() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "job", "--worktree"])
        .assert()
        .success();
    // fake agent prints what `claude --bg` prints; rein parses the session id out
    rein(&env, &env.repo)
        .env("REIN_RUN_CMD", "printf 'backgrounded abcd1234 rein:job\\n'")
        .args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backgrounded").and(predicate::str::contains("abcd1234")));
    let id = task_id(&env, "active", "job");
    let state = read(&store_root(&env).join("state").join(format!("{}.json", id)));
    assert!(
        state.contains("abcd1234"),
        "session id not recorded: {}",
        state
    );
    // `rein logs` surfaces the id + Claude Code's own viewers
    rein(&env, &env.repo)
        .args(["logs", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude attach abcd1234"));
}

#[test]
fn run_can_background_codex_command() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "job", "--worktree"])
        .assert()
        .success();
    let marker = env.bin_dir.join("codex_marker.txt");
    rein(&env, &env.repo)
        .env("REIN_RUN_AGENT", "codex")
        .env(
            "REIN_RUN_CMD",
            format!(
                "printf '%s|%s|%s' \"$REIN_TASK\" \"$REIN_DIR\" \"$REIN_PROMPT\" > {}; printf '\\n{{\"type\":\"thread.started\",\"thread_id\":\"codex-thread-1\"}}\\n'",
                marker.display()
            ),
        )
        .args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backgrounded codex pid"));

    let content = wait_for(&marker);
    let id = task_id(&env, "active", "job");
    assert!(content.contains(&id), "REIN_TASK not set: {}", content);
    assert!(
        content.contains("worktrees/job"),
        "REIN_DIR not the worktree: {}",
        content
    );
    assert!(
        content.contains("rein todo"),
        "REIN_PROMPT missing task instructions: {}",
        content
    );

    let state = read(&store_root(&env).join("state").join(format!("{}.json", id)));
    assert!(
        state.contains(r#""run_agent": "codex""#),
        "run agent not recorded: {}",
        state
    );
    assert!(
        state.contains(r#""run_log""#),
        "codex log not recorded: {}",
        state
    );

    rein(&env, &env.repo)
        .args(["logs", "job"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("codex pid")
                .and(predicate::str::contains("tail -f"))
                .and(predicate::str::contains(
                    "codex resume --include-non-interactive codex-thread-1",
                ))
                .and(predicate::str::contains(
                    "codex exec resume codex-thread-1 \"<prompt>\"",
                )),
        );
}

#[test]
fn logs_reports_stopped_codex_run_when_status_file_is_missing() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "job"])
        .assert()
        .success();
    let id = task_id(&env, "active", "job");
    let root = store_root(&env);
    let log = root.join("runs/missing_status.log");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(
        &log,
        r#"{"type":"thread.started","thread_id":"stopped-codex-thread"}"#,
    )
    .unwrap();
    let missing_status = root.join("runs/missing_status.status");
    fs::write(
        root.join("state").join(format!("{}.json", id)),
        serde_json::json!({
            "version": 1,
            "path": "active/job.md",
            "branch": "main",
            "run_session": "999999999",
            "run_agent": "codex",
            "run_log": log,
            "run_status": missing_status,
        })
        .to_string(),
    )
    .unwrap();

    rein(&env, &env.repo)
        .args(["logs", "job"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("state stopped")
                .and(predicate::str::contains("warning status file missing"))
                .and(predicate::str::contains(
                    "codex resume --include-non-interactive stopped-codex-thread",
                )),
        )
        .stderr(predicate::str::contains("kill:").not());
}

#[cfg(unix)]
#[test]
fn run_infers_codex_backend_from_run_cmd() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "job", "--worktree"])
        .assert()
        .success();
    let marker = env.bin_dir.join("fake_codex_marker.txt");
    let fake = env.bin_dir.join("codex");
    fs::write(&fake, "#!/bin/sh\nprintf '%s' \"$REIN_TASK\" > \"$1\"\n").unwrap();
    make_executable(&fake);
    let path = format!(
        "{}:{}",
        env.bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    rein(&env, &env.repo)
        .env("PATH", path)
        .env("REIN_RUN_CMD", format!("codex {}", marker.display()))
        .args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backgrounded codex pid"));

    let id = task_id(&env, "active", "job");
    assert_eq!(wait_for(&marker), id);
    let state = read(&store_root(&env).join("state").join(format!("{}.json", id)));
    assert!(
        state.contains(r#""run_agent": "codex""#),
        "run agent not inferred: {}",
        state
    );
}

#[cfg(unix)]
#[test]
fn run_default_codex_command_separates_prompt_from_options() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "job", "--worktree"])
        .assert()
        .success();
    let marker = env.bin_dir.join("default_codex_args.txt");
    let fake = env.bin_dir.join("codex");
    fs::write(
        &fake,
        "#!/bin/sh\ntmp=\"$CODEX_ARG_LOG.tmp\"\n{\n  printf 'REIN_ROOT=<%s>\\n' \"$REIN_ROOT\"\n  for arg in \"$@\"; do printf '<%s>\\n' \"$arg\"; done\n} > \"$tmp\"\nmv \"$tmp\" \"$CODEX_ARG_LOG\"\nprintf '{\"type\":\"thread.started\",\"thread_id\":\"default-codex-thread\"}\\n'\n",
    )
    .unwrap();
    make_executable(&fake);
    let path = format!(
        "{}:{}",
        env.bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    rein(&env, &env.repo)
        .env("PATH", path)
        .env("REIN_RUN_AGENT", "codex")
        .env("CODEX_ARG_LOG", &marker)
        .args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backgrounded codex pid"));

    let args = wait_for(&marker);
    let root = store_root(&env);
    assert!(
        args.contains(&format!("REIN_ROOT=<{}>\n", root.display())),
        "REIN_ROOT must be exported to Codex runs: {}",
        args
    );
    assert!(
        args.contains(&format!(
            "<exec>\n<--json>\n<--sandbox>\n<danger-full-access>\n<--add-dir>\n<{}>\n<-->\n",
            root.display()
        )),
        "default codex command must grant the rein store and pass `--` before the prompt: {}",
        args
    );
    assert!(
        args.contains("<Use the run-rein-task skill"),
        "default codex prompt must be the executable run prompt: {}",
        args
    );
}

#[test]
fn run_codex_status_file_survives_shell_exit() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .env("REIN_RUN_AGENT", "codex")
        .env("REIN_RUN_CMD", "exit 7")
        .args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backgrounded codex pid"));

    let id = task_id(&env, "active", "job");
    let state: serde_json::Value = serde_json::from_str(&read(
        &store_root(&env).join("state").join(format!("{}.json", id)),
    ))
    .unwrap();
    let status = state["run_status"].as_str().unwrap();
    assert_eq!(wait_for(Path::new(status)).trim(), "7");
}

#[test]
fn run_codex_marks_successful_exit_incomplete_when_todo_remains() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    seed_items(&env, "job");
    rein(&env, &env.repo)
        .args(["start", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .env("REIN_RUN_AGENT", "codex")
        .env("REIN_RUN_CMD", "true")
        .args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backgrounded codex pid"));

    let id = task_id(&env, "active", "job");
    let state: serde_json::Value = serde_json::from_str(&read(
        &store_root(&env).join("state").join(format!("{}.json", id)),
    ))
    .unwrap();
    let status = state["run_status"].as_str().unwrap();
    assert_eq!(wait_for(Path::new(status)).trim(), "incomplete");

    rein(&env, &env.repo)
        .args(["logs", "job"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("state incomplete")
                .and(predicate::str::contains("unchecked task items remain")),
        );
    let log = state["run_log"].as_str().unwrap();
    assert!(read(Path::new(log)).contains("rein post-run: unchecked items remain"));
}

#[test]
fn run_codex_marks_unfinished_json_turn_interrupted() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    seed_items(&env, "job");
    rein(&env, &env.repo)
        .args(["start", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .env("REIN_RUN_AGENT", "codex")
        .env(
            "REIN_RUN_CMD",
            "printf '%s\n' \
             '{\"type\":\"thread.started\",\"thread_id\":\"codex-thread-1\"}' \
             '{\"type\":\"turn.started\"}' \
             '{\"type\":\"item.started\",\"item\":{\"id\":\"item_1\",\"type\":\"command_execution\",\"status\":\"in_progress\"}}'",
        )
        .args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backgrounded codex pid"));

    let id = task_id(&env, "active", "job");
    let state: serde_json::Value = serde_json::from_str(&read(
        &store_root(&env).join("state").join(format!("{}.json", id)),
    ))
    .unwrap();
    let status = state["run_status"].as_str().unwrap();
    assert_eq!(wait_for(Path::new(status)).trim(), "interrupted");

    rein(&env, &env.repo)
        .args(["logs", "job"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("state interrupted")
                .and(predicate::str::contains("codex turn interrupted")),
        );
    let log = state["run_log"].as_str().unwrap();
    assert!(
        read(Path::new(log)).contains("rein post-run: codex turn interrupted before completion")
    );
}

#[test]
fn run_codex_marks_command_result_without_followup_interrupted() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    seed_items(&env, "job");
    rein(&env, &env.repo)
        .args(["start", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .env("REIN_RUN_AGENT", "codex")
        .env(
            "REIN_RUN_CMD",
            "printf '%s\n' \
             '{\"type\":\"thread.started\",\"thread_id\":\"codex-thread-1\"}' \
             '{\"type\":\"turn.started\"}' \
             '{\"type\":\"item.completed\",\"item\":{\"id\":\"item_0\",\"type\":\"agent_message\",\"text\":\"Starting.\"}}' \
             '{\"type\":\"item.completed\",\"item\":{\"id\":\"item_1\",\"type\":\"command_execution\",\"command\":\"rein todo\",\"aggregated_output\":\"## Tasks\\n1\\tDo work\\n\",\"exit_code\":0,\"status\":\"completed\"}}'",
        )
        .args(["run", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backgrounded codex pid"));

    let id = task_id(&env, "active", "job");
    let state: serde_json::Value = serde_json::from_str(&read(
        &store_root(&env).join("state").join(format!("{}.json", id)),
    ))
    .unwrap();
    let status = state["run_status"].as_str().unwrap();
    assert_eq!(wait_for(Path::new(status)).trim(), "interrupted");

    rein(&env, &env.repo)
        .args(["logs", "job"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state interrupted"));
}

#[test]
fn run_sets_session_title_with_branch_and_open_task_numbers() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    seed_items(&env, "job"); // Tasks: 1,2 · Validation: 3 — all open
    rein(&env, &env.repo)
        .args(["start", "job", "--worktree"])
        .assert()
        .success();
    let marker = env.bin_dir.join("title_marker.txt");
    // the run command sees REIN_TITLE = rein:<branch>:<open numbers>, range-folded
    rein(&env, &env.repo)
        .env(
            "REIN_RUN_CMD",
            format!("printf '%s' \"$REIN_TITLE\" > {}", marker.display()),
        )
        .args(["run", "job"])
        .assert()
        .success();
    assert_eq!(wait_for(&marker), "rein:job:1~3");

    // checking an item drops it from the title — only open items count, and a
    // run of two no longer folds into a range
    rein(&env, &env.repo)
        .args(["check", "1", "--task", "job"])
        .assert()
        .success();
    fs::remove_file(&marker).ok();
    rein(&env, &env.repo)
        .env(
            "REIN_RUN_CMD",
            format!("printf '%s' \"$REIN_TITLE\" > {}", marker.display()),
        )
        .args(["run", "job"])
        .assert()
        .success();
    assert_eq!(wait_for(&marker), "rein:job:2,3");
}

#[test]
fn logs_without_a_run_errors() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "job"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["logs", "job"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no run recorded"));
}

#[test]
fn done_closes_issue_and_leaves_issue_linked_pr_unmanaged() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"]).assert().success();
    rein(&env, &env.repo)
        .args(["use", "demo"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["attach-pr", "7"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["start", "demo"])
        .assert()
        .success();

    gh.set_pr_view_body("");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["done", "demo"]).assert().success();

    let log = gh.log_text();
    assert!(log.contains("issue close 41"), "log: {}", log);
    assert!(!log.contains("pr edit 7"), "log: {}", log);
    assert!(
        !gh.pr_edit_body.exists(),
        "issue-linked PR body must not be edited"
    );

    let month = chrono::Local::now().format("%Y-%m").to_string();
    assert!(store_root(&env)
        .join("done")
        .join(&month)
        .join("demo.md")
        .exists());
}

#[test]
fn cancel_closes_issue_not_planned() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo)
        .args(["new", "nope"])
        .assert()
        .success();
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "nope"]).assert().success();

    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["cancel", "nope"]).assert().success();
    assert!(gh
        .log_text()
        .contains("issue close 41 --comment Canceled via rein. --reason not planned"));
    assert!(store_root(&env).join("canceled/nope.md").exists());
}

// ---------------------------------------------------------------------------
// Phase 4: agent integration — full parallel worktree workflow
// ---------------------------------------------------------------------------

#[test]
fn parallel_worktrees_full_workflow() {
    let env = setup();
    init(&env);
    // two tasks, two worktrees, mutations from each cwd stay isolated
    for title in ["job a", "job b"] {
        rein(&env, &env.repo)
            .args(["new", title])
            .assert()
            .success();
    }
    for slug in ["job-a", "job-b"] {
        let path = store_root(&env).join("inbox").join(format!("{}.md", slug));
        let content = read(&path).replace(
            "## Tasks\n",
            &format!("## Tasks\n\n- [ ] <!-- task:work --> work for {}\n", slug),
        );
        fs::write(&path, content).unwrap();
        rein(&env, &env.repo)
            .args(["start", slug, "--worktree"])
            .assert()
            .success();
    }
    let wt_a = store_root(&env).join("worktrees/job-a");
    let wt_b = store_root(&env).join("worktrees/job-b");

    // each worker mutates "its" task by cwd alone — same item id, no cross-talk
    rein(&env, &wt_a).args(["check", "work"]).assert().success();
    // item-scoped log from a worktree: the bound task resolves by cwd, --item is
    // the item id, and the entry is tagged Task<id>
    rein(&env, &wt_b)
        .args(["log", "b progress", "--item", "work"])
        .assert()
        .success();

    let doc_a = read(&store_root(&env).join("active/job-a.md"));
    let doc_b = read(&store_root(&env).join("active/job-b.md"));
    assert!(doc_a.contains("- [x] <!-- task:work -->"));
    assert!(
        doc_b.contains("- [ ] <!-- task:work -->"),
        "b must stay unchecked"
    );
    assert!(doc_b.contains("b progress"));
    assert!(!doc_a.contains("b progress"));

    // mutation without binding in the main repo is gated (2 active, no current…
    // actually current was never set in worktree mode → resolution fails cleanly)
    rein(&env, &env.repo)
        .args(["note", "lost"])
        .assert()
        .failure();

    // finish both from the parent, explicitly
    fs::write(wt_a.join("result.txt"), "made by worker a").unwrap();
    rein(&env, &env.repo)
        .args(["done", "job-a"])
        .assert()
        .failure(); // dirty
    git(&env.home, &wt_a, &["add", "-A"]);
    git(&env.home, &wt_a, &["commit", "-m", "a"]);
    rein(&env, &env.repo)
        .args(["done", "job-a"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["cancel", "job-b", "--force"])
        .assert()
        .success();
    assert!(!wt_a.exists());
    assert!(!wt_b.exists());
}

// ---------------------------------------------------------------------------
// feat-v3: note, summary, single-mode branch
// ---------------------------------------------------------------------------

#[test]
fn note_appends_a_general_untagged_agent_log_entry() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "demo"])
        .assert()
        .success();
    seed_items(&env, "demo");
    rein(&env, &env.repo)
        .args(["start", "demo"])
        .assert()
        .success();

    // note appends a plain entry under the Agent Log, not tied to any item
    rein(&env, &env.repo)
        .args(["note", "a cross-cutting observation"])
        .assert()
        .success()
        .stdout(predicate::str::contains("noted in demo"));
    let doc = read(&store_root(&env).join("active/demo.md"));
    let log_pos = doc.find("## Agent Log").unwrap();
    let at = doc
        .find("a cross-cutting observation")
        .expect("note not appended");
    assert!(at > log_pos, "note must land in the Agent Log");
    // the entry carries no `Task<id>:` tag (that is `rein log`'s job)
    let line = doc
        .lines()
        .find(|l| l.contains("a cross-cutting observation"))
        .unwrap();
    assert!(
        !line.contains("Task"),
        "note entry must not be item-tagged: {}",
        line
    );
}

#[test]
fn title_and_goal_set_via_cli() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "feat x"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["use", "feat-x"])
        .assert()
        .success();
    // title sets the frontmatter; goal replaces the ## Goal section — rein owns
    // the write (the caller never edits the Markdown)
    rein(&env, &env.repo)
        .args(["title", "Polish the feature"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .args(["goal", "Make X usable end to end"])
        .assert()
        .success();
    let doc = read(&store_root(&env).join("inbox/feat-x.md"));
    assert!(doc.contains("title: Polish the feature"));
    assert!(doc.contains("## Goal\n\nMake X usable end to end"));
    // the rest of the scaffolding survives the Goal rewrite
    assert!(doc.contains("## Tasks"));
    assert!(doc.contains("## Agent Log"));
    // empty text is rejected
    rein(&env, &env.repo)
        .args(["goal", "   "])
        .assert()
        .failure()
        .stderr(predicate::str::contains("empty"));
}

#[test]
fn summary_generates_title_and_goal_from_items_via_llm() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "feat v3"])
        .assert()
        .success();
    seed_items(&env, "feat-v3"); // Tasks: 1,2 · Validation: 3
                                 // a fake LLM: drains the piped prompt, returns the TITLE/GOAL contract
    rein(&env, &env.repo)
        .env(
            "REIN_SUMMARY_CMD",
            "cat >/dev/null; printf 'TITLE: v3 CLI ergonomics\\nGOAL: Round out the v3 CLI.\\nKeep it LLM-safe.\\n'",
        )
        .args(["summary", "feat-v3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("summarized feat-v3"));
    let doc = read(&store_root(&env).join("inbox/feat-v3.md"));
    assert!(
        doc.contains("title: v3 CLI ergonomics"),
        "title not set: {}",
        doc
    );
    assert!(
        doc.contains("## Goal\n\nRound out the v3 CLI.\nKeep it LLM-safe."),
        "goal not set from LLM output: {}",
        doc
    );
    // the items it summarized are left untouched
    assert!(doc.contains("Do thing one"));
}

#[test]
fn summary_refuses_when_there_are_no_items() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "empty"])
        .assert()
        .success();
    rein(&env, &env.repo)
        .env("REIN_SUMMARY_CMD", "printf 'TITLE: x\\nGOAL: y\\n'")
        .args(["summary", "empty"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no checklist items"));
}

#[test]
fn start_single_records_the_current_branch_in_frontmatter() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo)
        .args(["new", "solo"])
        .assert()
        .success();
    // single mode claims no new branch — it records the checked-out branch (main)
    rein(&env, &env.repo)
        .args(["start", "solo"])
        .assert()
        .success();
    let doc = read(&store_root(&env).join("active/solo.md"));
    assert!(
        doc.contains("branch: main"),
        "single start should record the current branch: {}",
        doc
    );
    let id = task_id(&env, "active", "solo");
    let st = read(&store_root(&env).join("state").join(format!("{}.json", id)));
    assert!(
        st.contains("\"branch\": \"main\""),
        "state should record the branch: {}",
        st
    );
}
