use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
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
    cat - >/dev/null
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
            .env("GH_PR_EDIT_BODY", &self.pr_edit_body);
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
    rein(&env, &env.repo).args(["new", "alpha"]).assert().success();
    rein(&env, &env.repo).args(["new", "beta"]).assert().success();
    rein(&env, &env.repo).args(["start", "alpha"]).assert().success();
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
    rein(&env, &env.repo).args(["new", "demo task"]).assert().success();
    rein(&env, &env.repo).args(["start", "demo-task"]).assert().success();

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
    rein(&env, &env.repo).args(["new", "feat one"]).assert().success();
    rein(&env, &env.repo)
        .args(["start", "feat-one", "--worktree"])
        .assert()
        .success()
        .stdout(predicate::str::contains("worktree:"));

    let id1 = task_id(&env, "active", "feat-one");
    let wt = env._tmp.path().join("proj-wt/feat-one");
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
    rein(&env, &env.repo).args(["new", "feat two"]).assert().success();
    rein(&env, &env.repo).args(["start", "feat-two"]).assert().success();
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
    assert!(doc.contains("branch: rein/feat-one"));
}

#[test]
fn mutations_check_uncheck_log_fail() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo");
    rein(&env, &env.repo).args(["start", "demo"]).assert().success();
    let root = store_root(&env);
    let path = root.join("active/demo.md");

    // mutation assigns stable integer IDs on the spot — no GitHub needed
    rein(&env, &env.repo).args(["check", "1"]).assert().success();
    assert!(read(&path).contains("- [x] <!-- task:1 --> Do thing one"));

    rein(&env, &env.repo).args(["uncheck", "1"]).assert().success();
    assert!(read(&path).contains("- [ ] <!-- task:1 --> Do thing one"));

    rein(&env, &env.repo)
        .args(["log", "implemented the thing"])
        .assert()
        .success();
    let doc = read(&path);
    assert!(doc.contains("implemented the thing"));
    let log_pos = doc.find("## Agent Log").unwrap();
    assert!(doc.find("implemented the thing").unwrap() > log_pos);

    rein(&env, &env.repo)
        .args(["fail", "1", "--reason", "blocked by upstream"])
        .assert()
        .success();
    let doc = read(&path);
    assert!(doc.contains("FAIL 1: blocked by upstream"));
    // fail resolves the item: checked box + failed sentinel + ~~strike~~ ❌
    assert!(doc.contains("- [x] <!-- task:1 --> <!-- failed --> ~~Do thing one~~ ❌"));

    // retry reopens it: back to an unchecked, undecorated item + a RETRY log line
    rein(&env, &env.repo).args(["retry", "1"]).assert().success();
    let doc = read(&path);
    assert!(doc.contains("- [ ] <!-- task:1 --> Do thing one"));
    assert!(doc.contains("RETRY 1"));

    // unknown item errors and lists what's available
    rein(&env, &env.repo)
        .args(["check", "99"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("available items").and(predicate::str::contains("1, 2, 3")));
}

#[test]
fn local_check_assigns_integer_ids_without_github() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo"); // 3 items, no IDs, fully offline
    rein(&env, &env.repo).args(["start", "demo"]).assert().success();

    // single integer sequence across Tasks(1,2) and Validation(3)
    rein(&env, &env.repo).args(["check", "2"]).assert().success();
    let doc = read(&store_root(&env).join("active/demo.md"));
    assert!(doc.contains("- [ ] <!-- task:1 --> Do thing one"));
    assert!(doc.contains("- [x] <!-- task:2 --> Add tests later"));
    assert!(doc.contains("- [ ] <!-- task:3 --> Tests pass"));
}

#[test]
fn ids_are_stable_under_reorder() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo");
    rein(&env, &env.repo).args(["start", "demo"]).assert().success();
    rein(&env, &env.repo).args(["check", "1"]).assert().success(); // assigns 1,2,3
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
    rein(&env, &env.repo).args(["check", "2"]).assert().success();
    let doc = read(&path);
    assert!(doc.contains("- [x] <!-- task:2 --> Add tests later"));
    // the inserted item gets the next integer (4), never a reused one
    assert!(doc.contains("<!-- task:4 --> new top item"));
}

#[test]
fn check_with_task_arg_gives_helpful_error() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo");
    rein(&env, &env.repo).args(["start", "demo"]).assert().success();
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
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo"); // items lack IDs
    // EDITOR=true is a no-op edit; open must still heal IDs on return
    rein(&env, &env.repo).args(["open", "demo"]).assert().success();
    let doc = read(&store_root(&env).join("inbox/demo.md"));
    assert!(doc.contains("<!-- task:1 -->"));
    assert!(doc.contains("<!-- task:2 -->"));
    assert!(doc.contains("<!-- task:3 -->"));
}

#[test]
fn status_lists_items_with_numbers() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo");
    rein(&env, &env.repo).args(["start", "demo"]).assert().success();
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
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo"); // Tasks: 1,2 · Validation: 3
    rein(&env, &env.repo).args(["start", "demo"]).assert().success();

    // default: only unchecked items, grouped under their section headings
    rein(&env, &env.repo)
        .arg("todo")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("## Tasks")
                .and(predicate::str::contains("## Validation"))
                .and(predicate::str::contains("1\tDo thing one"))
                .and(predicate::str::contains("2\tAdd tests later"))
                .and(predicate::str::contains("3\tTests pass")),
        );

    // checked items drop out of the list
    rein(&env, &env.repo).args(["check", "2"]).assert().success();
    rein(&env, &env.repo)
        .arg("todo")
        .assert()
        .success()
        .stdout(
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
    rein(&env, &env.repo).args(["new", "other"]).assert().success();
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
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo"); // Tasks: 1,2 · Validation: 3
    rein(&env, &env.repo).args(["start", "demo"]).assert().success();

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
    rein(&env, &env.repo).args(["retry", "1"]).assert().success();
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
        rein(&env, &env.repo).args(["new", title]).assert().success();
    }
    // two active tasks, current points at the last
    rein(&env, &env.repo).args(["start", "one"]).assert().success();
    rein(&env, &env.repo).args(["start", "two"]).assert().success();

    rein(&env, &env.repo)
        .args(["log", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ambiguous").and(predicate::str::contains("--task")));

    // explicit --task passes the gate
    rein(&env, &env.repo)
        .args(["log", "hello", "--task", "one"])
        .assert()
        .success();
    assert!(read(&store_root(&env).join("active/one.md")).contains("hello"));

    // query commands are not gated
    rein(&env, &env.repo).arg("current").assert().success();
}

#[test]
fn resolution_order_flag_env_current() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo).args(["new", "one"]).assert().success();
    rein(&env, &env.repo).args(["new", "two"]).assert().success();
    rein(&env, &env.repo).args(["start", "one"]).assert().success();
    let id_one = task_id(&env, "active", "one");
    rein(&env, &env.repo).args(["use", "two"]).assert().success();
    let id_two = task_id(&env, "inbox", "two");
    assert_eq!(read(&store_root(&env).join("current")).trim(), id_two);

    // REIN_TASK env (#3) beats current file (#4)
    rein(&env, &env.repo)
        .env("REIN_TASK", &id_one)
        .arg("current")
        .assert()
        .success()
        .stdout(predicate::str::contains(&id_one));

    // --task flag (#1) beats env (#3): log lands in 'two'
    rein(&env, &env.repo)
        .env("REIN_TASK", &id_one)
        .args(["log", "flag wins", "--task", "two"])
        .assert()
        .success();
    assert!(read(&store_root(&env).join("inbox/two.md")).contains("flag wins"));
}

#[test]
fn use_rebinds_worktree_pointer() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo).args(["new", "one"]).assert().success();
    rein(&env, &env.repo).args(["new", "two"]).assert().success();
    rein(&env, &env.repo)
        .args(["start", "one", "--worktree"])
        .assert()
        .success();
    let wt = env._tmp.path().join("proj-wt/one");
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
    rein(&env, &env.repo).args(["new", "dirty job"]).assert().success();
    rein(&env, &env.repo)
        .args(["start", "dirty-job", "--worktree"])
        .assert()
        .success();
    let wt = env._tmp.path().join("proj-wt/dirty-job");

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
    rein(&env, &env.repo).args(["new", "keepwt"]).assert().success();
    rein(&env, &env.repo)
        .args(["start", "keepwt", "--worktree"])
        .assert()
        .success();
    let wt = env._tmp.path().join("proj-wt/keepwt");
    fs::write(wt.join("junk.txt"), "wip").unwrap();
    rein(&env, &env.repo)
        .args(["done", "keepwt", "--keep-worktree"])
        .assert()
        .success();
    assert!(wt.exists(), "worktree should be kept");
    let month = chrono::Local::now().format("%Y-%m").to_string();
    assert!(store_root(&env).join("done").join(&month).join("keepwt.md").exists());
}

#[test]
fn cancel_force_discards_dirty_worktree() {
    let env = setup();
    init(&env);
    rein(&env, &env.repo).args(["new", "byebye"]).assert().success();
    rein(&env, &env.repo)
        .args(["start", "byebye", "--worktree"])
        .assert()
        .success();
    let wt = env._tmp.path().join("proj-wt/byebye");
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
fn move_transitions_any_direction_without_side_effects() {
    let env = setup();
    init(&env);
    let root = store_root(&env);
    rein(&env, &env.repo).args(["new", "wander"]).assert().success();

    // inbox → active is a plain relocation: no current pointer, no worktree
    rein(&env, &env.repo)
        .args(["move", "wander", "active"])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved wander inbox → active"));
    assert!(root.join("active/wander.md").exists());
    assert!(!root.join("inbox/wander.md").exists());
    assert!(read(&root.join("active/wander.md")).contains("status: active"));
    assert!(!root.join("current").exists(), "move must not claim current");

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
    rein(&env, &env.repo).args(["new", "alpha"]).assert().success();
    rein(&env, &env.repo).args(["new", "beta"]).assert().success();
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
    assert!(root.join("state").join(format!("{}.json", id_alpha)).is_file());
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
    rein(&env, &env.repo).args(["new", "one"]).assert().success();
    rein(&env, &env.repo).args(["start", "one"]).assert().success();
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
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
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
fn push_local_change_preserves_human_text() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"]).assert().success();
    rein(&env, &env.repo).args(["use", "demo"]).assert().success();

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
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"]).assert().success();
    rein(&env, &env.repo).args(["use", "demo"]).assert().success();
    rein(&env, &env.repo)
        .args(["log", "local progress note"])
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
    assert!(doc.contains("local progress note"), "Agent Log must survive pull");

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
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"]).assert().success();
    rein(&env, &env.repo).args(["use", "demo"]).assert().success();

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
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    rein(&env, &env.repo).args(["use", "demo"]).assert().success();
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
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo");
    rein(&env, &env.repo).args(["use", "demo"]).assert().success();
    rein(&env, &env.repo)
        .args(["log", "agent did a thing"])
        .assert()
        .success();
    rein(&env, &env.repo).args(["attach-pr", "7"]).assert().success();

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
fn start_draft_pr_records_number() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo).args(["new", "feat"]).assert().success();
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["start", "feat", "--worktree", "--draft-pr"])
        .assert()
        .success()
        .stdout(predicate::str::contains("draft PR: #7"));
    assert!(read(&store_root(&env).join("active/feat.md")).contains("github_pr: 7"));
    assert!(gh.log_text().contains("pr create --draft"));
}

#[test]
fn done_closes_issue_and_updates_pr() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo).args(["new", "demo"]).assert().success();
    seed_items(&env, "demo");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["issue", "demo"]).assert().success();
    rein(&env, &env.repo).args(["use", "demo"]).assert().success();
    rein(&env, &env.repo).args(["attach-pr", "7"]).assert().success();
    rein(&env, &env.repo).args(["start", "demo"]).assert().success();

    gh.set_pr_view_body("");
    let mut c = rein(&env, &env.repo);
    gh.apply(&mut c);
    c.args(["done", "demo"]).assert().success();

    let log = gh.log_text();
    assert!(log.contains("issue close 41"), "log: {}", log);
    assert!(log.contains("pr edit 7"), "log: {}", log);
    assert!(read(&gh.pr_edit_body).contains("rein:begin"));

    let month = chrono::Local::now().format("%Y-%m").to_string();
    assert!(store_root(&env).join("done").join(&month).join("demo.md").exists());
}

#[test]
fn cancel_closes_issue_not_planned() {
    let env = setup();
    init(&env);
    let gh = fake_gh(&env);
    rein(&env, &env.repo).args(["new", "nope"]).assert().success();
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
        rein(&env, &env.repo).args(["new", title]).assert().success();
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
    let wt_a = env._tmp.path().join("proj-wt/job-a");
    let wt_b = env._tmp.path().join("proj-wt/job-b");

    // each worker mutates "its" task by cwd alone — same item id, no cross-talk
    rein(&env, &wt_a).args(["check", "work"]).assert().success();
    rein(&env, &wt_b)
        .args(["log", "b progress"])
        .assert()
        .success();

    let doc_a = read(&store_root(&env).join("active/job-a.md"));
    let doc_b = read(&store_root(&env).join("active/job-b.md"));
    assert!(doc_a.contains("- [x] <!-- task:work -->"));
    assert!(doc_b.contains("- [ ] <!-- task:work -->"), "b must stay unchecked");
    assert!(doc_b.contains("b progress"));
    assert!(!doc_a.contains("b progress"));

    // mutation without binding in the main repo is gated (2 active, no current…
    // actually current was never set in worktree mode → resolution fails cleanly)
    rein(&env, &env.repo)
        .args(["log", "lost"])
        .assert()
        .failure();

    // finish both from the parent, explicitly
    fs::write(wt_a.join("result.txt"), "made by worker a").unwrap();
    rein(&env, &env.repo).args(["done", "job-a"]).assert().failure(); // dirty
    git(&env.home, &wt_a, &["add", "-A"]);
    git(&env.home, &wt_a, &["commit", "-m", "a"]);
    rein(&env, &env.repo).args(["done", "job-a"]).assert().success();
    rein(&env, &env.repo)
        .args(["cancel", "job-b", "--force"])
        .assert()
        .success();
    assert!(!wt_a.exists());
    assert!(!wt_b.exists());
}
