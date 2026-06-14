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
rein start feat-a --worktree  # creates ../proj-wt/feat-a + branch rein/feat-a
rein start feat-b --worktree
```

Each worktree is bound to its own task, so an agent just runs commands from its own cwd:

```sh
cd ../proj-wt/feat-a && rein current   # → feat-a (resolved from cwd)
cd ../proj-wt/feat-b && rein check x   # → edits only feat-b, no cross-talk
```

Clean up explicitly from the parent session (`rein done feat-a` / `rein cancel feat-b --force`). Running a mutation without a task in the main repo is blocked by a guard when two or more tasks are active — pass `--task` or run it from the right worktree.

### C. GitHub shared inbox / PRs

```sh
rein issue settings-cleanup   # publish a GitHub issue (rein label, marker-wrapped)
rein pull-inbox               # import rein-labeled issues (idempotent)
rein pull                     # apply remote issue-body changes
rein push                     # push local changes into the issue/PR managed section
```

Only the managed section between the `rein:begin`/`rein:end` markers is updated on the remote body; human text outside the markers is preserved. Conflicts are detected by a 3-way hash, backed up under `conflicts/`, and force-pushed with `rein push --resolved` after you resolve them. Attach a PR with `rein start … --draft-pr` or `rein attach-pr <n>`, then update it with `rein push` (the Agent Log folds into a `<details>`).

## TUI (`rein ui`)

A single dashboard across all your projects. Launched inside a repo, it pre-scopes to that project; press `P` to pick another.

| key     | action                                        |
| ------- | --------------------------------------------- |
| `j`/`k` | move                                          |
| `Tab`   | cycle status (all/inbox/active/done/canceled) |
| `P`     | pick project (project > task hierarchy)       |
| `Enter` | edit in `$EDITOR`                             |
| `n`     | new task                                      |
| `s`     | start (inbox → active)                        |
| `m`     | move to any state (i/a/d/c)                   |
| `d`     | done                                          |
| `p`     | publish issue or push                         |
| `/`     | filter (matches project name too)             |
| `q`     | quit                                          |

Editing is always delegated to `$EDITOR` — there is no built-in Markdown editor in the TUI.

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
rein check / uncheck <item-id> [--task <id>]
rein log <text> [--task <id>]
rein fail <item-id> --reason <text> [--task <id>]   resolve as failed (checked + struck through, drops from todo)
rein retry <item-id> [--task <id>]   reopen a failed item
rein issue <task> | pull-inbox | pull | push [--resolved]
rein attach-issue <n> | attach-pr <n>
rein done [task] [--keep-worktree]
rein cancel [task] [--keep-worktree] [--force]
rein doctor                          rebuild state/, fix frontmatter drift
rein status | root | ui
```

## LLM integration (Claude skill)

```sh
rein init --skill   # scaffold .claude/skills/run-rein-task/SKILL.md
```

The skill gets remaining items via `rein todo` and changes state only through `rein check`/`log`/`fail` (never editing the Markdown directly). The full rules live in the scaffolded SKILL.md.
