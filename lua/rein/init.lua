-- rein.nvim — toggle the `rein` TUI in a floating terminal window.
--
-- Install with lazy.nvim:
--   { "devgony/rein", cmd = "Rein", keys = { "<M-r>" }, opts = { keymap = "<M-r>" } }
--
-- <M-r> (or :Rein) toggles the dashboard as a centered float — and the same key
-- hides it from inside the TUI. Toggling off keeps the `rein ui` session alive
-- (hidden), so the next toggle re-shows the same session; it ends only when you
-- quit the TUI with its own `q`. Set opts.dev = true (or a repo path) to launch
-- from source via `cargo run` while hacking on rein itself.

local M = {}
local unpack = unpack or table.unpack

---@class rein.Config
---@field cmd string|string[]
---@field dev boolean|string
---@field width number
---@field height number
---@field border string
---@field title string
---@field keymap string|false
local config = {
  cmd = "rein ui", -- string (split on spaces) or an argv list
  -- Dev mode: launch the TUI from source via `cargo run` instead of the
  -- installed binary, so edits show up on the next toggle (incremental debug
  -- build) without `cargo install`. `true` = auto-detect the repo from this
  -- plugin's own location (ideal for a local `dir=` install); a string = path
  -- to the rein repo; `false` = use `cmd` as-is.
  dev = false,
  width = 0.95, -- <= 1: fraction of the editor; > 1: absolute columns
  height = 0.95, -- <= 1: fraction of the editor; > 1: absolute rows
  border = "rounded", -- any nvim_open_win() border style
  title = " rein ",
  keymap = "<M-r>", -- toggle in normal mode + close from inside the TUI; false to skip
}

local state = { buf = nil, win = nil, job = nil }

-- Repo root inferred from this file: <root>/lua/rein/init.lua -> <root>.
local function plugin_root()
  local src = debug.getinfo(1, "S").source:sub(2) -- strip the leading '@'
  return vim.fn.fnamemodify(src, ":h:h:h")
end

local function dev_root()
  local dev = config.dev
  if dev == true then
    return plugin_root()
  elseif type(dev) == "string" and dev ~= "" then
    return (dev:gsub("/+$", ""))
  end
  return nil
end

-- The argv the toggle will spawn. In dev mode the binary is replaced by
-- `cargo run --manifest-path <root>/Cargo.toml -- <subcommand…>`, reusing the
-- subcommand args from `cmd` (e.g. `ui`). Exposed so you can `:lua =require("rein").command()`.
function M.command()
  local cmd = config.cmd
  local base
  if type(cmd) == "table" then
    base = cmd
  else
    base = vim.split(cmd, " ", { trimempty = true })
  end
  local root = dev_root()
  if root then
    local sub = { unpack(base, 2) } -- drop the binary name, keep {"ui", …}
    return vim.list_extend(
      { "cargo", "run", "--quiet", "--manifest-path", root .. "/Cargo.toml", "--" },
      sub
    )
  end
  return base
end

local function dim(v, total)
  if v <= 1 then
    return math.max(1, math.floor(total * v))
  end
  return math.min(v, total)
end

function M.is_open()
  return state.win ~= nil and vim.api.nvim_win_is_valid(state.win)
end

-- Whether a kept-alive (hidden) `rein ui` session exists and is still running,
-- so the next toggle can re-show it instead of launching fresh. Checks both that
-- the terminal buffer survives and that its job hasn't exited (jobwait with a 0
-- timeout returns -1 while the job is still alive).
local function session_alive()
  if not (state.buf and vim.api.nvim_buf_is_valid(state.buf)) then
    return false
  end
  if not state.job then
    return false
  end
  local ok, res = pcall(vim.fn.jobwait, { state.job }, 0)
  return ok and res ~= nil and res[1] == -1
end

-- Fully tear down the float + terminal. State is nilled first so the jobstart
-- on_exit callback (which fires while we force-delete the terminal buffer) just
-- re-enters as a no-op instead of double-closing.
local function cleanup()
  local win, buf = state.win, state.buf
  state.win, state.buf, state.job = nil, nil, nil
  -- drop the global WinClosed focus handler so it doesn't fire editor-wide once
  -- the float is gone (the buffer-local ones die with the buffer below).
  pcall(vim.api.nvim_clear_autocmds, { group = "rein_focus" })
  if win and vim.api.nvim_win_is_valid(win) then
    pcall(vim.api.nvim_win_close, win, true)
  end
  if buf and vim.api.nvim_buf_is_valid(buf) then
    pcall(vim.api.nvim_buf_delete, buf, { force = true })
  end
end

-- Full teardown — kills the TUI + buffer, ending the session. Used when the TUI
-- exits on its own (its `q`, via on_exit). The toggle path uses M.hide instead,
-- which keeps the session alive for a later re-show.
function M.close()
  cleanup()
end

-- Toggle-off: close the float window but KEEP the terminal buffer and its live
-- `rein ui` session (the buffer is `bufhidden = "hide"`), so the next toggle
-- re-shows the same session — preserving the selected task, item drill-down, and
-- filters. Earlier this killed the session and re-launched fresh each time; the
-- left-pane reflow that motivated that is now healed by repaint_float on re-show.
function M.hide()
  local win = state.win
  state.win = nil
  -- drop the global WinClosed handler before we close, so our own close doesn't
  -- trigger a refocus/repaint and it can't fire editor-wide while we're hidden.
  pcall(vim.api.nvim_clear_autocmds, { group = "rein_focus" })
  if win and vim.api.nvim_win_is_valid(win) then
    pcall(vim.api.nvim_win_close, win, true)
  end
end

-- Fires when the TUI exits on its own (its `q`); schedule so the buffer/window
-- teardown runs outside the restricted on_exit context.
local function on_exit()
  vim.schedule(cleanup)
end

local function start_terminal()
  local cmd = M.command()
  if vim.fn.has("nvim-0.10") == 1 then
    return vim.fn.jobstart(cmd, { term = true, on_exit = on_exit })
  end
  return vim.fn.termopen(cmd, { on_exit = on_exit }) -- nvim < 0.10
end

-- Re-enter terminal mode when our float is the current window. The TUI only
-- reads keys in terminal mode, so this is what keeps it controllable; the focus
-- autocmds below call it whenever our window regains focus. Guarded so it never
-- grabs input while you work elsewhere (must be the current window) and never
-- fights a deliberate normal-mode visit (skip when already in terminal mode).
local function refocus_terminal()
  if not (state.win and vim.api.nvim_win_is_valid(state.win)) then
    return
  end
  if vim.api.nvim_get_current_win() ~= state.win then
    return
  end
  if vim.api.nvim_get_mode().mode ~= "t" then
    vim.cmd("startinsert")
  end
end

-- Re-assert terminal mode now AND again shortly after. When a second floating
-- terminal (e.g. a claude-code toggle on another key) is closed, it hands focus
-- back to us via its OWN deferred callbacks — a lone vim.schedule can run before
-- that plugin's cleanup settles and get reverted to normal mode, which is the
-- "stuck in normal mode until you press i" bug. The delayed second pass runs
-- after those callbacks and wins the race; refocus_terminal is idempotent (it
-- no-ops once we're already in terminal mode), so the double call is harmless.
local function queue_refocus()
  vim.schedule(refocus_terminal)
  vim.defer_fn(refocus_terminal, 40)
end

-- Force the rein TUI to fully repaint. When an overlay float is closed, nvim can
-- leave our covered terminal's grid left-shifted (borders + the ▶ marker clipped
-- off the left), and a diff-rendering TUI won't fix it on its own — nor does it
-- always receive a focus/resize event to react to. So nudge the window width by
-- one column and restore it on the next tick: that resizes the PTY twice, the
-- child gets real resize events, and ratatui redraws every cell at the correct
-- width. Only runs when our float is the current window, so it stays a no-op
-- (and flicker-free) for unrelated window closes.
local function repaint_float()
  if not (state.win and vim.api.nvim_win_is_valid(state.win)) then
    return
  end
  if vim.api.nvim_get_current_win() ~= state.win then
    return
  end
  local ok, cfg = pcall(vim.api.nvim_win_get_config, state.win)
  if not ok or type(cfg.width) ~= "number" or cfg.width <= 2 then
    return
  end
  local full = cfg.width
  cfg.width = full - 1
  pcall(vim.api.nvim_win_set_config, state.win, cfg)
  vim.schedule(function()
    if state.win and vim.api.nvim_win_is_valid(state.win) then
      local ok2, c2 = pcall(vim.api.nvim_win_get_config, state.win)
      if ok2 then
        c2.width = full
        pcall(vim.api.nvim_win_set_config, state.win, c2)
      end
    end
  end)
end

-- Open the centered float over `buf` and wire the focus/repaint autocmds that
-- keep us controllable when an overlay closes over us. Shared by a fresh start
-- (open) and a re-show of a kept-alive session (M.show). Leaves entering
-- terminal mode to the caller (the fresh path must start the job first).
local function build_float(buf)
  local cols, rows = vim.o.columns, vim.o.lines
  local w, h = dim(config.width, cols), dim(config.height, rows)

  state.buf = buf
  state.win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    width = w,
    height = h,
    row = math.floor((rows - h) / 2),
    col = math.floor((cols - w) / 2),
    style = "minimal",
    border = config.border,
    title = config.title,
  })

  -- When another floating terminal (e.g. a second toggle-term plugin bound to
  -- its own key) is layered over this float and then closed, nvim hands focus
  -- back to our window in NORMAL mode (TUI visible but inert until you press
  -- `i`) AND can leave its grid left-shifted (borders + ▶ marker clipped). Two
  -- triggers heal both: a buffer-local (Win|Buf)Enter re-enters terminal mode on
  -- any focus return, and a global WinClosed re-enters terminal mode + forces a
  -- full repaint when that overlay closes. The group is cleared per open (and on
  -- hide/cleanup) so re-toggling never stacks duplicate handlers.
  local group = vim.api.nvim_create_augroup("rein_focus", { clear = true })
  vim.api.nvim_create_autocmd({ "WinEnter", "BufEnter" }, {
    group = group,
    buffer = buf,
    callback = queue_refocus,
  })
  vim.api.nvim_create_autocmd("WinClosed", {
    group = group,
    callback = function()
      queue_refocus()
      vim.schedule(repaint_float)
    end,
  })
end

-- Fresh start: a new terminal buffer running `rein ui`. The TUI grabs all input
-- while focused (terminal mode), so the normal-mode toggle can't fire from
-- inside it; a buffer-local terminal-mode mapping on the same key toggles the
-- float so the one key works both ways.
local function open()
  local buf = vim.api.nvim_create_buf(false, true)
  if config.keymap then
    vim.keymap.set("t", config.keymap, M.toggle, {
      buffer = buf,
      desc = "Toggle rein UI",
      silent = true,
    })
  end
  build_float(buf)
  state.job = start_terminal()
  -- keep the terminal buffer + its live session when the window is hidden on
  -- toggle-off (instead of wiping it), so re-toggling re-shows the same session.
  vim.bo[buf].bufhidden = "hide"
  vim.cmd("startinsert")
end

-- Re-show a kept-alive (hidden) session in a fresh float. nvim can leave the
-- re-shown terminal's grid stale/left-shifted, so re-assert terminal mode and
-- force a full repaint (the width round-trip) once it is back on screen.
function M.show()
  build_float(state.buf)
  queue_refocus()
  vim.schedule(repaint_float)
end

function M.toggle()
  if M.is_open() then
    M.hide()
    return
  end
  -- a previous toggle-off left the session hidden but alive → re-show it
  if session_alive() then
    M.show()
    return
  end
  local exe = M.command()[1]
  if vim.fn.executable(exe) == 0 then
    vim.notify("rein: `" .. exe .. "` not found on $PATH", vim.log.levels.ERROR)
    return
  end
  open()
end

function M.setup(opts)
  config = vim.tbl_deep_extend("force", config, opts or {})
  vim.api.nvim_create_user_command("Rein", function()
    M.toggle()
  end, { desc = "Toggle the rein TUI" })
  if config.keymap then
    vim.keymap.set("n", config.keymap, M.toggle, { desc = "Toggle rein UI", silent = true })
  end
end

return M
