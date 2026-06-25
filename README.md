<h1 align="center">Rein</h1>

<p align="center">
  <img src="images/rein-logo.png" alt="Rein logo" width="160">
</p>

<p align="center">
  <b>LLM task journal &amp; shared inbox manager</b>
</p>

<p align="center">
  <i>The reins of a harness</i> — steer an LLM with local Markdown task docs,<br>
  safe CLI mutations it can't corrupt, and GitHub only when you need to share.
</p>

<p align="center">
  <a href="https://crates.io/crates/reins"><img alt="crates.io" src="https://img.shields.io/crates/v/reins?logo=rust&logoColor=white&label=crates.io&color=E37B40"></a>
  <a href="LICENSE"><img alt="license: MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
  <a href="https://github.com/devgony/rein/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/devgony/rein?style=social"></a>
</p>

---

## Why rein?

- 📝 **Markdown is the source of truth.** Task docs live in a local store; GitHub is a publishing and review surface, not the truth.
- 🔒 **The LLM can't corrupt the doc.** State changes go through CLI mutation commands (`check` / `log` / `fail`), never direct Markdown edits.
- 🌳 **Parallel-safe.** Each git worktree binds to its own task, so multi-agent runs never cross-talk.
- 🔗 **GitHub when you want it.** Publish issues, attach PRs, and sync a marker-wrapped managed section — human text outside the markers is preserved.
- 🖥️ **At a glance.** A cross-project TUI (`rein ui`) and a Neovim plugin to toggle it in a float.

See [`PLAN.md`](PLAN.md) for the design rationale and decisions.

## Contents

- [Install](#install)
- [Concepts](#concepts)
- [Workflows](#workflows)
- [TUI (`rein ui`)](#tui-rein-ui)
- [Command summary](#command-summary)
- [LLM integration (Claude skill)](#llm-integration-claude-skill)

## Install

```sh
cargo install reins   # the installed command is `rein`
```

Or from source:

```sh
cargo install --path .
```

The crate is published as `reins` (the singular `rein` was taken); the command you run is `rein`. Requires git. GitHub integration (`issue`/`pull`/`push`, etc.) requires the `gh` CLI.

## Concepts

- **store**: a per-repo local store (`~/.local/share/rein/<key>/`). The key is a UUID that `rein init` writes to `git config rein.store`, so the store is immune to worktree and directory moves, and living outside the repo means task docs are never committed by accident. Override the location with `REIN_ROOT`.
- **state = directory**: `inbox/` → `active/` → `done/YYYY-MM/`, plus `canceled/`. The `status` in frontmatter is derived.
- **item IDs**: the `<!-- task:N -->` on a checklist item is a stable integer assigned once and kept (not a line number). The tool assigns it whenever it touches the document, and `rein check <N>` uses that number.
- **task resolution order**: `--task <id>` → worktree pointer → `REIN_TASK` → the store's `current` file.

## Workflows

### A. Solo, local (the basics)

```sh
rein new "settings cleanup"   # create a draft in inbox (prints id + path)
rein open settings-cleanup    # write Goal/Tasks/Validation in $EDITOR
rein start settings-cleanup   # inbox → active, sets current to this task
```

`rein title <text>` / `rein goal <text>` set the frontmatter title and the `## Goal` section through rein (LLM-safe — rein owns the write, no direct Markdown edits). Listed the items but the title/Goal are still a placeholder? `rein summary [task]` has the configured run agent summarize the items into a concise title + Goal and applies both through that same safe path — the LLM only returns text. The summary backend follows `REIN_RUN_AGENT` → git config `rein.runAgent` → `opencode` (the default); Claude uses `claude -p`, Codex uses `codex exec --sandbox read-only -- -`, and opencode uses `opencode run "$(cat)"`. The prompt (the item list) is piped on stdin, and the reply must be `TITLE: …` / `GOAL: …`.

Then hand it to Claude Code; following the skill rules, the LLM proceeds:

```sh
rein todo                     # list remaining unchecked items (skill entry point)
rein check <item-id>          # check off a completed item
rein log "implementation note" --item <item-id>   # item-scoped Agent Log entry (tagged Task<id>)
rein note "general observation"                   # Agent Log entry not tied to an item
rein fail <item-id> --reason "…"   # resolve an item as failed (drops out of todo)
rein retry <item-id>               # reopen a failed item
```

When done:

```sh
rein done                     # active → done/YYYY-MM/ (clears current)
```

You don't have to add `<!-- task:... -->` IDs by hand — the tool assigns them (`check` needs an ID).

### B. Parallel worktrees (Claude Code multi-agent)

```sh
rein start feat-a --worktree  # creates a worktree + branch feat-a (prints its path)
rein start feat-b --worktree
```

Worktrees live under the store (`<store>/worktrees/<slug>`), not beside the repo, so the project's parent dir stays clean and `done`/`cancel` remove them from a path the engine owns. `start` prints the worktree path (`worktree: …`). Each worktree is bound to its own task, so an agent just runs commands from its own cwd:

```sh
cd <printed worktree path>   # e.g. ~/.local/share/rein/<key>/worktrees/feat-a
rein current                 # → feat-a (resolved from cwd)
rein check x                 # → edits only feat-a, no cross-talk
```

Clean up explicitly from the parent session (`rein done feat-a` / `rein cancel feat-b --force`). Running a mutation without a task in the main repo is blocked by a guard when two or more tasks are active — pass `--task` or run it from the right worktree.

### C. GitHub shared inbox / PRs

```sh
rein issue settings-cleanup   # publish a GitHub issue (rein label, marker-wrapped)
rein issue settings-cleanup --project Roadmap  # …and file it onto a GitHub Project board
rein pull-inbox               # import rein-labeled issues (idempotent)
rein pull                     # apply remote issue-body changes
rein push                     # push local changes into the issue/PR managed section
```

Only the managed section between the `rein:begin`/`rein:end` markers is updated on the remote body; human text outside the markers is preserved. Conflicts are detected by a 3-way hash, backed up under `conflicts/`, and force-pushed with `rein push --resolved` after you resolve them (in the TUI a conflict on `i`/`p` shows a prompt — press `f` to force-push the same way).

Open a draft PR with `rein pr [task] [--worktree]` (worktree-backed, else a main-repo branch), or attach an existing one with `rein attach-pr <n>`; then update it with `rein push` (the Agent Log folds into a `<details>`). In the TUI, `p` opens the same PR flow (pick `w` worktree / `b` branch). `rein pr` pushes the branch to `origin` for you; if the branch has no commits yet it just warns (GitHub rejects an empty PR) — commit your work first, then run `rein pr` again. (`rein start … --draft-pr` folds PR creation into the claim, but since a freshly claimed branch has no commits it will warn — the usual flow is start → work → `rein pr`.)

## TUI (`rein ui`)

A single dashboard across all your projects. Launched inside a repo, it pre-scopes to that project; press `P` to pick another. The task list title shows the scoped project's configured run agent (`REIN_RUN_AGENT` or project `rein.runAgent`) beside the project name. The right column shows a small **meta** pane — id, branch (tagged `(worktree)` or `(branch)`), the working `dir:`, issue/PR numbers, created/updated dates, tags, and the live `run:` state of the last `rein run` (running/done/failed, polled from the configured backend) — above the Markdown preview of the selected task. A task with a live run colors its task title green in the list.

| key     | action                                        |
| ------- | --------------------------------------------- |
| `j`/`k` | move                                          |
| `Tab`   | cycle status (all/inbox/active/done/canceled) |
| `P`     | pick project (project > task hierarchy)       |
| `Enter` | edit in `$EDITOR`                             |
| `l`     | drill into the task's checklist items (item list + per-item Agent Log; `space` checks/unchecks, `n` adds, `e` edits, `d` deletes, `h`/`Esc`/`q` back) |
| `n`     | new task                                      |
| `s`     | start (inbox → active) → `s` single / `w` worktree / `b` branch |
| `m`     | move to any state (i/a/d/c)                   |
| `d`     | done                                          |
| `D`     | delete permanently (asks `y` to confirm; removes files + worktree) |
| `x`     | run an agent on the task in the background (`REIN_RUN_CMD`) |
| `a`     | attach/resume the task's last run (`claude attach`, `codex resume --include-non-interactive`, or `opencode --session`) |
| `L`     | show recent log output for the selected running task |
| `A`     | choose the project's run agent (`codex`, `claude`, or `opencode`) |
| `S`     | summarize the task's checklist items into title + Goal via the configured LLM (`rein summary`); runs on a worker thread with a spinner overlay so the slow LLM call doesn't freeze the dashboard (`Ctrl-c` still quits) |
| `i`     | create the issue (pick a GitHub Project, or none), or push to an existing one (on a sync conflict, press `f` to force-push) |
| `p`     | open a draft PR (then `w` worktree / `b` branch), or push to an existing one (on a sync conflict, press `f` to force-push) |
| `y`     | copy the task's working directory path to the clipboard |
| `w`     | view & manage the project's git worktrees (list + `n` add / `space` lock / `d` remove / `y` copy path / `h`/`Esc`/`q` back) |
| `/`     | filter (matches project name too)             |
| `q`     | quit                                          |

Editing is always delegated to `$EDITOR` — there is no built-in Markdown editor in the TUI.

Press `l` to **drill into a task's checklist items**: the left pane lists each item with its checkbox state (green done, yellow open, red struck-through failed), the preview shows the Agent-Log entries that reference the selected item (matched by the `Task<id>` convention the run skill follows), and `space` checks/unchecks the item under the cursor (a failed item is reopened). Press `n` to **add a new item** — type its text and `Enter` (the item is appended to the task's `## Tasks` section and gets a stable id), or `Esc` to cancel. Press `e` to **edit** the selected item's text (the entry is prefilled; `Enter` saves, `Esc` cancels) or `d` to **delete** it (asks `y` to confirm) — both keep the item's stable id and checkbox state. `h`/`Esc`/`q` steps back to the task list.

Press `w` to **manage the project's git worktrees** (the selected task's project): the left pane lists every worktree of the repo — branch (or `(detached)`/`(bare)`) and directory name, with `[main]`/`[locked]`/`[prunable]` flags — and the preview shows the selected worktree's full path, branch, HEAD and flags. Press `n` to **add** a worktree (type a branch name; an existing branch is checked out, a new one is created with `-b`, placed under the store's `worktrees/` dir), `space` to **lock/unlock** it, `d` to **remove** it (asks `y` to confirm; git refuses a dirty or locked worktree, surfaced as a popup), or `y` to **copy its path** to the clipboard. The main worktree can't be locked or removed. `h`/`Esc`/`q` steps back to the task list.

Failed items (resolved via `rein fail`) render in red and struck through in the preview, distinct from green done and yellow open.

### Neovim

The repo doubles as a Neovim plugin that toggles `rein ui` in a floating terminal — install it like any other plugin.

Prerequisites: the `rein` binary on `$PATH` (see [Install](#install)) and Neovim 0.10+.

With [lazy.nvim](https://github.com/folke/lazy.nvim), add the spec:

```lua
{
  "devgony/rein",
  cmd = "Rein",
  keys = { "<M-r>" },          -- lazy-load trigger; the plugin owns the mapping
  opts = { keymap = "<M-r>" }, -- Alt-r toggles the float both ways
}
```

On LazyVim, put that in its own file, `~/.config/nvim/lua/plugins/rein.lua`:

```lua
return {
  {
    "devgony/rein",
    cmd = "Rein",
    keys = { "<M-r>" },
    opts = { keymap = "<M-r>" },
  },
}
```

Hacking on rein itself? Point at the working tree instead of GitHub, and set `dev = true` so each toggle launches the TUI from source via `cargo run` (a fast incremental debug build) — your edits show up on the next toggle with no `cargo install`:

```lua
{ dir = "/path/to/rein", name = "rein", cmd = "Rein", keys = { "<M-r>" }, opts = { keymap = "<M-r>", dev = true } }
```

`dev = true` auto-detects the repo from the plugin's own location; pass a path (`dev = "/path/to/rein"`) to point elsewhere. `:lua =require("rein").command()` prints exactly what will run.

Usage: `<M-r>` (or `:Rein`) opens the dashboard centered as a 95% × 95% float and **hides it again from inside the TUI** — one key, both ways. Toggling off only hides the float: the `rein ui` session stays alive in the background, so the next `<M-r>` **re-shows the same session** (your selected task, item drill-down, and filters are preserved) instead of launching a fresh one. The session ends only when you quit the TUI with its own `q` — after that, the next toggle starts fresh. Failed items show in red (struck through). Set `keymap = false` to skip the built-in mapping and wire your own key to `:Rein` (give it `mode = { "n", "t" }` so it toggles out from terminal mode too).

Options (`opts = { ... }`, defaults shown):

| option             | default     | meaning                                                                                                    |
| ------------------ | ----------- | ---------------------------------------------------------------------------------------------------------- |
| `cmd`              | `"rein ui"` | command to launch (string or argv list)                                                                    |
| `dev`              | `false`     | `true` (auto-detect repo) or a repo path → run from source via `cargo run` instead of the installed binary |
| `width` / `height` | `0.95`      | `<= 1` fraction of the editor, `> 1` absolute cells                                                        |
| `border`           | `"rounded"` | any `nvim_open_win()` border style                                                                         |
| `keymap`           | `"<M-r>"`   | toggles in normal mode and closes from inside the TUI; `false` to skip                                     |

## Command summary

```text
rein init [--skill]                  create the store + register git config rein.store (--skill: scaffold SKILL.md)
rein new <title> [--shared]          create a task draft in inbox
rein list [--status <s>]             list tasks
rein todo [--all] [--task <id>]      resolved task's unchecked items (--all: all items + state)
rein open [task]                     open in $EDITOR (fuzzy picker with no argument)
rein current [--path]                print the resolved task (read-only)
rein title <text> [--task <id>]      set the frontmatter title (LLM-safe; rein owns the write)
rein goal <text> [--task <id>]       set the ## Goal section (LLM-safe; rein owns the write)
rein summary [task]                  LLM-summarize the items into title + Goal, applied via rein
rein use <task>                      switch the task binding (worktree pointer / current file)
rein move <task> <status>            move to any state (plain relocation, no side effects)
rein start <task> [--worktree] [--branch <b>] [--draft-pr]
rein pr [task] [--worktree]           open a draft PR (worktree under the store, else a main-repo branch)
rein run [task]                       launch an agent on the task in the background (in its worktree)
rein logs [task]                      show the background session/log handle of the last run
rein check / uncheck <item-id> [--task <id>]
rein log <text> --item <item-id> [--task <id>]   item-scoped Agent Log entry (tagged Task<id>; --item required)
rein note <text> [--task <id>]       Agent Log entry not tied to a checklist item
rein fail <item-id> --reason <text> [--task <id>]   resolve as failed (checked + struck through, drops from todo)
rein retry <item-id> [--task <id>]   reopen a failed item
rein issue <task> | pull-inbox | pull | push [--resolved]
rein attach-issue <n> | attach-pr <n>
rein done [task] [--keep-worktree]
rein cancel [task] [--keep-worktree] [--force]
rein delete <task> [--force]         permanently remove a task (files + worktree; no GitHub effects)
rein doctor                          rebuild state/, fix frontmatter drift
rein status | root | ui
```

## LLM integration (Claude, Codex, or opencode)

```sh
rein init --skill   # scaffold .claude/ and .agents/ run-rein-task skills
```

The skill gets remaining items via `rein todo` and changes state only through `rein check`/`log`/`note`/`fail` (never editing the Markdown directly). `rein log` is item-scoped (`--item <item-id>` is required, and the entry is tagged `Task<id>` so it shows under that item in the TUI's per-item log); `rein note` records an entry not tied to any item. The full rules live in the scaffolded SKILL.md.

### Launching the agent (`rein run`)

You don't have to `cd` into a worktree to work a task — rein already knows where each task lives. `rein run [task]` (TUI: `x`) launches an agent **in the background**, with its cwd set to the task's worktree (or the main repo if the task only has a branch) and `REIN_TASK`/`REIN_SLUG`/`REIN_BRANCH`/`REIN_DIR`/`REIN_TITLE`/`REIN_PROMPT` exported, so the agent resolves the task no matter where it was invoked. Claude Code backgrounds itself with `claude --bg`; Codex's local `codex exec` and `opencode run` are foreground-only, so rein backgrounds them and writes stdout/stderr under `<store>/runs/`.

The agent backend is resolved from `REIN_RUN_AGENT` → git config `rein.runAgent` → inferred from a configured `REIN_RUN_CMD` whose first word is `codex`/`opencode`/`claude` → `opencode` (the default when nothing is configured). The command template is resolved from `REIN_RUN_CMD` env → git config `rein.run` → the backend default.

Claude default:

```sh
claude --bg --dangerously-skip-permissions --name "$REIN_TITLE" /run-rein-task
```

Codex default:

```sh
codex exec --json --sandbox danger-full-access --add-dir "$REIN_ROOT" -- "$REIN_PROMPT"
```

opencode default:

```sh
opencode run --format json --dangerously-skip-permissions "$REIN_PROMPT"
```

`claude --bg` dispatches a **tracked background session** (it runs under Claude Code's daemon, not a detached `-p` process) and returns immediately. Claude-compatible custom commands should likewise return promptly (self-background) because `rein run` waits for that command and surfaces its output. Codex and opencode backend commands are the exception: rein spawns them in the background and records their pid/log/status itself.

`--name "$REIN_TITLE"` pins the session's display name (shown in `claude agents`, the picker, and the terminal title) to `rein:<branch>:<open task numbers>` — the open (unchecked, unfailed) checklist item numbers, with consecutive runs of three or more folded into a range, e.g. `rein:feat-v3:1~12,14,16`. rein exports the computed name as `REIN_TITLE`; a custom command can reference `$REIN_TITLE` (or set its own `--name`). rein still tracks the session by its **id** regardless of the name.

**Watching it.** `claude --bg` prints a session id, which `rein run` echoes and records. Codex and opencode runs record the wrapper pid, log path, and exit-code file. The TUI shows the live state in the `run:` line of the meta pane (and a green `●` in the list while it's running), refreshed automatically every few seconds. For Claude, `rein logs [task]` reprints `claude agents`/`attach`/`logs` hints. For Codex, `rein logs [task]` prints the pid, run state, status file, local log, a `tail -f` command, and when the default JSON mode has emitted a `thread.started` event, the exact `codex resume --include-non-interactive <thread-id>` command for opening the interactive UI. A Codex process exit code of `0` is shown as `succeeded`; if its JSON log stops on a started turn/item or a command result with no follow-up assistant message, rein records `interrupted`, and if the turn ends but `rein todo --task <id>` still reports unchecked items, rein records `incomplete` instead. It also prints `codex exec resume <thread-id> "<prompt>"` for non-interactive continuation. For opencode, `rein logs [task]` prints the same pid/state/status/log/`tail -f`, and once the `--format json` stream carries a `sessionID` it prints `opencode --session <id>` (open the interactive UI) and `opencode run --session <id> "<prompt>"` (continue non-interactively); a `0` exit is `succeeded`, a non-zero exit `failed`, and a clean exit with unchecked items left `incomplete`. In the TUI, press `a` to open the native agent UI for the selected task's last run (`claude attach <id>`, `codex resume --include-non-interactive <thread-id>`, or `opencode --session <id>`). Task progress also shows as the checklist and Agent Log fill in (the agent reports through `rein check`/`rein log`).

Use Codex or opencode locally with:

```sh
git config rein.runAgent codex      # or: opencode
# or one-off:
REIN_RUN_AGENT=codex rein run my-task
REIN_RUN_AGENT=opencode rein run my-task
```

Codex uses local `codex exec`, not `codex cloud exec`. Cloud tasks run in configured Codex cloud environments and count against Codex cloud-task usage; use them explicitly with a custom `REIN_RUN_CMD` only when you want that remote execution model. opencode runs the local `opencode run` headless mode with `--dangerously-skip-permissions` so the detached agent can edit files and call `rein` without prompting; like Codex it has no daemon, so rein backgrounds it and owns the log.

Override the command for a different agent or flags, e.g. `git config rein.run 'claude --name rein:$REIN_SLUG -p /run-rein-task'` (this example names the session after the slug instead of the default branch + task numbers). A custom Codex command can use `$REIN_PROMPT`, or just start with `codex` so rein infers the Codex backend. Notes:

- The defaults run non-interactively. Claude uses its background daemon with `--dangerously-skip-permissions`; Codex uses `--json --sandbox danger-full-access --add-dir "$REIN_ROOT"`, so a Codex run selected by `REIN_RUN_AGENT=codex` or `git config rein.runAgent codex` can perform the same repo-level work, including Git writes, while still keeping a machine-readable event log. Claude Code may show a one-time prompt to accept bypass mode, which a detached run can't answer — set `"skipDangerousModePermissionPrompt": true` in `~/.claude/settings.json` to suppress it (if you already use skip-permissions normally, this is likely already set).
- Prefer a worktree (`rein start … --worktree`) so the autonomous run is isolated; running a branch-only task happens in the main repo and is **not** isolated (rein warns).
- The default prompt is the `/run-rein-task` skill. A new worktree only has a project-level skill if it was **committed** (worktrees check out committed files only). So `rein run` installs rein's bundled copy at the **user level** (`~/.claude/skills/run-rein-task/`) when it's missing — available in every worktree with **no files added to your repo** and nothing to commit or share. (`rein init --skill` is the separate opt-in if you *do* want a project-level skill to commit and customize; a project skill takes precedence over the user-level one.)
