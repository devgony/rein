-- rein.nvim — toggle the `rein` TUI in a floating terminal window.
--
-- Install with lazy.nvim:
--   { "devgony/rein", cmd = "Rein", keys = { "<M-r>" }, opts = { keymap = "<M-r>" } }
--
-- <M-r> (or :Rein) toggles the dashboard as a centered float — and the same key
-- closes it from inside the TUI. Set opts.dev = true (or a repo path) to launch
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

-- Fully tear down the float + terminal. State is nilled first so the jobstart
-- on_exit callback (which fires while we force-delete the terminal buffer) just
-- re-enters as a no-op instead of double-closing.
local function cleanup()
  local win, buf = state.win, state.buf
  state.win, state.buf, state.job = nil, nil, nil
  if win and vim.api.nvim_win_is_valid(win) then
    pcall(vim.api.nvim_win_close, win, true)
  end
  if buf and vim.api.nvim_buf_is_valid(buf) then
    pcall(vim.api.nvim_buf_delete, buf, { force = true })
  end
end

-- Closing ends the session (kills the TUI + buffer). Each open then starts a
-- fresh `rein ui`. Reusing a hidden terminal buffer instead made nvim reflow it
-- on the next show, shaving the border's worth of leading columns off the left
-- pane on every toggle; the dashboard is stateless, so a fresh start is free.
function M.close()
  cleanup()
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
-- autocmd below calls it whenever our window regains focus. Guarded on the
-- current window so it never grabs input while you're working elsewhere.
local function refocus_terminal()
  if state.win and vim.api.nvim_win_is_valid(state.win) and vim.api.nvim_get_current_win() == state.win then
    vim.cmd("startinsert")
  end
end

local function open()
  local cols, rows = vim.o.columns, vim.o.lines
  local w, h = dim(config.width, cols), dim(config.height, rows)

  state.buf = vim.api.nvim_create_buf(false, true)
  -- the TUI grabs all input while focused (terminal mode), so the normal-mode
  -- toggle can't fire from inside it; a buffer-local terminal-mode mapping on
  -- the same key closes the float so the one key toggles both ways.
  if config.keymap then
    vim.keymap.set("t", config.keymap, M.toggle, {
      buffer = state.buf,
      desc = "Toggle rein UI",
      silent = true,
    })
  end

  state.win = vim.api.nvim_open_win(state.buf, true, {
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
  -- back to our window in NORMAL mode — leaving the TUI visible but inert. Re-
  -- enter terminal mode on every (Win|Buf)Enter into our window so the dashboard
  -- stays controllable. Buffer-local, so it dies with the buffer on cleanup();
  -- the group is cleared per open so re-toggling never stacks duplicates.
  local group = vim.api.nvim_create_augroup("rein_focus", { clear = true })
  vim.api.nvim_create_autocmd({ "WinEnter", "BufEnter" }, {
    group = group,
    buffer = state.buf,
    callback = function()
      vim.schedule(refocus_terminal)
    end,
  })

  state.job = start_terminal()
  vim.cmd("startinsert")
end

function M.toggle()
  if M.is_open() then
    M.close()
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
