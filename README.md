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

Then hand it to Claude Code; following the skill rules, the LLM proceeds:

```sh
rein todo                     # list remaining unchecked items (skill entry point)
rein check <item-id>          # check off a completed item
rein log "implementation note"
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

Only the managed section between the `rein:begin`/`rein:end` markers is updated on the remote body; human text outside the markers is preserved. Conflicts are detected by a 3-way hash, backed up under `conflicts/`, and force-pushed with `rein push --resolved` after you resolve them.

Open a draft PR with `rein pr [task] [--worktree]` (worktree-backed, else a main-repo branch), or attach an existing one with `rein attach-pr <n>`; then update it with `rein push` (the Agent Log folds into a `<details>`). In the TUI, `p` opens the same PR flow (pick `w` worktree / `b` branch). `rein pr` pushes the branch to `origin` for you; if the branch has no commits yet it just warns (GitHub rejects an empty PR) — commit your work first, then run `rein pr` again. (`rein start … --draft-pr` folds PR creation into the claim, but since a freshly claimed branch has no commits it will warn — the usual flow is start → work → `rein pr`.)

## TUI (`rein ui`)

A single dashboard across all your projects. Launched inside a repo, it pre-scopes to that project; press `P` to pick another. The right column shows a small **meta** pane — id, branch (tagged `(worktree)` or `(branch)`), the working `dir:`, issue/PR numbers, created/updated dates, tags, and the live `run:` state of the last `rein run` (running/done/failed, polled from `claude agents`) — above the Markdown preview of the selected task. A task with a live run also gets a green `●` in the list.

| key     | action                                        |
| ------- | --------------------------------------------- |
| `j`/`k` | move                                          |
| `Tab`   | cycle status (all/inbox/active/done/canceled) |
| `P`     | pick project (project > task hierarchy)       |
| `Enter` | edit in `$EDITOR`                             |
| `l`     | drill into the task's checklist items (item list + per-item Agent Log; `space` checks/unchecks, `n` adds a new item, `h`/`Esc`/`q` back) |
| `n`     | new task                                      |
| `s`     | start (inbox → active) → `s` single / `w` worktree / `b` branch |
| `m`     | move to any state (i/a/d/c)                   |
| `d`     | done                                          |
| `D`     | delete permanently (asks `y` to confirm; removes files + worktree) |
| `x`     | run an agent on the task in the background (`REIN_RUN_CMD`) |
| `i`     | create the issue (pick a GitHub Project, or none), or push to an existing one |
| `p`     | open a draft PR (then `w` worktree / `b` branch), or push to an existing one |
| `y`     | copy the task's working directory path to the clipboard |
| `/`     | filter (matches project name too)             |
| `q`     | quit                                          |

Editing is always delegated to `$EDITOR` — there is no built-in Markdown editor in the TUI.

Press `l` to **drill into a task's checklist items**: the left pane lists each item with its checkbox state (green done, yellow open, red struck-through failed), the preview shows the Agent-Log entries that reference the selected item (matched by the `Task<id>` convention the run skill follows), and `space` checks/unchecks the item under the cursor (a failed item is reopened). Press `n` to **add a new item** — type its text and `Enter` (the item is appended to the task's `## Tasks` section and gets a stable id), or `Esc` to cancel. `h`/`Esc`/`q` steps back to the task list.

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

Usage: `<M-r>` (or `:Rein`) opens the dashboard centered as a 95% × 95% float and **closes it again from inside the TUI** — one key, both ways. You can also quit the TUI with its own `q`. Failed items show in red (struck through). Set `keymap = false` to skip the built-in mapping and wire your own key to `:Rein` (give it `mode = { "n", "t" }` so it toggles out from terminal mode too).

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
rein use <task>                      switch the task binding (worktree pointer / current file)
rein move <task> <status>            move to any state (plain relocation, no side effects)
rein start <task> [--worktree] [--branch <b>] [--draft-pr]
rein pr [task] [--worktree]           open a draft PR (worktree under the store, else a main-repo branch)
rein run [task]                       launch an agent on the task in the background (in its worktree)
rein logs [task]                      show the background session id of the last run (+ claude attach/logs)
rein check / uncheck <item-id> [--task <id>]
rein log <text> [--task <id>]
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

## LLM integration (Claude skill)

```sh
rein init --skill   # scaffold .claude/skills/run-rein-task/SKILL.md
```

The skill gets remaining items via `rein todo` and changes state only through `rein check`/`log`/`fail` (never editing the Markdown directly). The full rules live in the scaffolded SKILL.md.

### Launching the agent (`rein run`)

You don't have to `cd` into a worktree to work a task — rein already knows where each task lives. `rein run [task]` (TUI: `x`) launches an agent **in the background**, with its cwd set to the task's worktree (or the main repo if the task only has a branch) and `REIN_TASK`/`REIN_SLUG`/`REIN_BRANCH`/`REIN_DIR` exported, so the agent resolves the task no matter where it was invoked. It's detached (`nohup … &`) and keeps running after rein returns; the agent writes its own transcript to its standard location (Claude Code: `~/.claude/projects/…`, visible in its background-agents view).

The command is a template, resolved in order: `REIN_RUN_CMD` env → git config `rein.run` → the built-in default:

```sh
claude --bg --dangerously-skip-permissions /run-rein-task
```

`claude --bg` dispatches a **tracked background session** (it runs under Claude Code's daemon, not a detached `-p` process) and returns immediately. A custom `REIN_RUN_CMD` should likewise return promptly (self-background) — `rein run` waits for the command and surfaces its output.

No `--name` is passed, so Claude Code auto-names the session from the prompt — easier to read in `claude agents` than a forced `rein:<slug>` label, and rein tracks the session by its **id** regardless. Add `--name` in a custom command if you want to pin your own label.

**Watching it.** `claude --bg` prints a session id, which `rein run` echoes and records. The TUI shows the session's live state in the `run:` line of the meta pane (and a green `●` in the list while it's running), refreshed automatically every few seconds. For the full conversation use Claude Code's own tools: `claude agents` (list all sessions), `claude attach <id>` (watch live / resume), `claude logs <id>` (recent output); `rein logs [task]` reprints the recorded id with those commands. Task progress also shows as the checklist and Agent Log fill in (the agent reports through `rein check`/`rein log`).

Override it for a different agent or flags, e.g. `git config rein.run 'claude --name rein:$REIN_SLUG -p /run-rein-task'` (this example pins a `rein:<slug>` name back). Notes:

- The default runs fully autonomously (`--dangerously-skip-permissions`). Claude Code may show a one-time prompt to accept bypass mode, which a detached run can't answer — set `"skipDangerousModePermissionPrompt": true` in `~/.claude/settings.json` to suppress it (if you already use skip-permissions normally, this is likely already set).
- Prefer a worktree (`rein start … --worktree`) so the autonomous run is isolated; running a branch-only task happens in the main repo and is **not** isolated (rein warns).
- The default prompt is the `/run-rein-task` skill. A new worktree only has a project-level skill if it was **committed** (worktrees check out committed files only). So `rein run` installs rein's bundled copy at the **user level** (`~/.claude/skills/run-rein-task/`) when it's missing — available in every worktree with **no files added to your repo** and nothing to commit or share. (`rein init --skill` is the separate opt-in if you *do* want a project-level skill to commit and customize; a project skill takes precedence over the user-level one.)
