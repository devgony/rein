-- rein.nvim — toggle the `rein` TUI in a floating terminal window.
--
-- Install with lazy.nvim:
--   { "devgony/rein", opts = {}, cmd = "Rein",
--     keys = { { "<leader>ru", "<cmd>Rein<cr>", desc = "Toggle rein UI" } } }
--
-- `:Rein` (or the configured keymap) opens the dashboard in a centered float;
-- quitting the TUI (its own `q`) closes the window, and toggling again from
-- normal mode closes/reopens it.

local M = {}

local config = {
  cmd = "rein ui", -- string (split on spaces) or an argv list
  width = 0.9, -- <= 1: fraction of the editor; > 1: absolute columns
  height = 0.9, -- <= 1: fraction of the editor; > 1: absolute rows
  border = "rounded", -- any nvim_open_win() border style
  title = " rein ",
  keymap = "<leader>ru", -- normal-mode toggle; set to false to skip the mapping
}

local state = { buf = nil, win = nil, job = nil }

local function argv()
  if type(config.cmd) == "table" then
    return config.cmd
  end
  return vim.split(config.cmd, " ", { trimempty = true })
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
  if vim.fn.has("nvim-0.10") == 1 then
    return vim.fn.jobstart(argv(), { term = true, on_exit = on_exit })
  end
  return vim.fn.termopen(argv(), { on_exit = on_exit }) -- nvim < 0.10
end

local function open()
  local cols, rows = vim.o.columns, vim.o.lines
  local w, h = dim(config.width, cols), dim(config.height, rows)

  -- Reuse a still-running session's buffer (a hidden, alive TUI) so toggling
  -- the window preserves state; otherwise start fresh.
  local fresh = not (state.buf and vim.api.nvim_buf_is_valid(state.buf))
  if fresh then
    state.buf = vim.api.nvim_create_buf(false, true)
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
  if vim.fn.executable(argv()[1]) == 0 then
    vim.notify("rein: `" .. argv()[1] .. "` not found on $PATH", vim.log.levels.ERROR)
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
