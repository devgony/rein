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

function M.close()
  if state.win and vim.api.nvim_win_is_valid(state.win) then
    vim.api.nvim_win_close(state.win, true)
  end
  state.win = nil
end

-- Called when the TUI process exits: tear the float and buffer down so the
-- next toggle starts a fresh session.
local function on_exit()
  M.close()
  if state.buf and vim.api.nvim_buf_is_valid(state.buf) then
    vim.api.nvim_buf_delete(state.buf, { force = true })
  end
  state.buf, state.job = nil, nil
end

local function start_terminal()
  local cmd = M.command()
  if vim.fn.has("nvim-0.10") == 1 then
    return vim.fn.jobstart(cmd, { term = true, on_exit = on_exit })
  end
  return vim.fn.termopen(cmd, { on_exit = on_exit }) -- nvim < 0.10
end

local function open()
  local cols, rows = vim.o.columns, vim.o.lines
  local w, h = dim(config.width, cols), dim(config.height, rows)

  -- Reuse a still-running session's buffer (a hidden, alive TUI) so toggling
  -- the window preserves state; otherwise start fresh.
  local fresh = not (state.buf and vim.api.nvim_buf_is_valid(state.buf))
  if fresh then
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

  if fresh then
    state.job = start_terminal()
  end
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
