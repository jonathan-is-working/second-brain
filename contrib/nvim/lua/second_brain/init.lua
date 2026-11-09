-- second_brain.nvim: the nvim side of `sb` (optional).
--
-- What it adds:
--   :SbDash            `sb dash` in a floating terminal; notes it opens land in this nvim
--   SbOpen(path)       called by `sb dash` (through $NVIM) to open a note here
--   SbTheme()          called by `sb theme` / the dashboard (T) to apply the theme here
--   :Sb <args>         run any `sb` command in the background and show its output
--   :SbToday           open today's journal
--
-- Setup (lazy.nvim):
--   { dir = "~/path/to/second-brain/contrib/nvim", opts = {} }
-- or anywhere in your config:
--   require("second_brain").setup({ home = "~/second-brain", keys = true })
--
-- Nothing here calls the model on its own: ingest / connect / synth only run when
-- you ask (`:Sb ingest`, or the <leader>si / sc / ss keys if `keys = true`).

local M = {}

local defaults = {
  --- Brain directory (the one `sb init` created): holds vault/ and data/.
  home = vim.env.SB_HOME or "~/second-brain",
  --- Apply the theme picked in `sb dash` (data/theme.json) at startup.
  theme = true,
  --- Default <leader>s… keymaps (false to define your own).
  keys = false,
  --- Size of the dashboard float (fraction of the editor).
  dash = { width = 0.96, height = 0.92 },
}

M.config = vim.deepcopy(defaults)

local function home() return vim.fs.normalize(vim.fn.expand(M.config.home)) end
local function vault() return home() .. "/vault" end

-- Theme ---------------------------------------------------------------------

--- The theme saved by `sb theme` / the dashboard, or nil.
function M.theme()
  local f = io.open(home() .. "/data/theme.json", "r")
  if not f then return nil end
  local ok, t = pcall(vim.json.decode, f:read("*a"))
  f:close()
  if ok and type(t) == "table" and t.colorscheme then return t end
  return nil
end

--- Apply the saved theme's colorscheme (warns if it is not installed).
function M.apply_theme()
  local t = M.theme()
  if not t then return end
  vim.o.background = t.light and "light" or "dark"
  if not pcall(vim.cmd.colorscheme, t.colorscheme) then
    vim.notify("second-brain: colorscheme " .. t.colorscheme .. " is not installed", vim.log.levels.WARN)
  end
end

-- Dashboard -------------------------------------------------------------------

local dash = { buf = nil, win = nil }

--- A window where a note can be opened (not a float, a terminal or a file tree).
local function editing_window()
  local function usable(w)
    if not vim.api.nvim_win_is_valid(w) or vim.api.nvim_win_get_config(w).relative ~= "" then return false end
    local b = vim.api.nvim_win_get_buf(w)
    return vim.bo[b].buftype == "" and vim.bo[b].filetype ~= "neo-tree" and vim.bo[b].filetype ~= "NvimTree"
  end
  local cur = vim.api.nvim_get_current_win()
  if usable(cur) then return cur end
  local prev = vim.fn.win_getid(vim.fn.winnr("#"))
  if prev ~= 0 and usable(prev) then return prev end
  for _, w in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if usable(w) then return w end
  end
  return nil
end

local function dash_hide()
  if dash.win and vim.api.nvim_win_is_valid(dash.win) then vim.api.nvim_win_hide(dash.win) end
  dash.win = nil
end

--- Toggle `sb dash` in a floating terminal (kept alive between toggles).
function M.dash()
  if dash.win and vim.api.nvim_win_is_valid(dash.win) then return dash_hide() end
  local w = math.floor(vim.o.columns * M.config.dash.width)
  local h = math.floor(vim.o.lines * M.config.dash.height)
  local new = not (dash.buf and vim.api.nvim_buf_is_valid(dash.buf))
  if new then dash.buf = vim.api.nvim_create_buf(false, true) end
  dash.win = vim.api.nvim_open_win(dash.buf, true, {
    relative = "editor",
    width = w,
    height = h,
    row = math.floor((vim.o.lines - h) / 2) - 1,
    col = math.floor((vim.o.columns - w) / 2),
    border = "rounded",
    style = "minimal",
  })
  if new then
    vim.fn.jobstart({ "sb", "dash" }, {
      term = true,
      on_exit = function()
        vim.schedule(function()
          dash_hide()
          if dash.buf and vim.api.nvim_buf_is_valid(dash.buf) then vim.api.nvim_buf_delete(dash.buf, { force = true }) end
          dash.buf = nil
        end)
      end,
    })
  end
  vim.cmd.startinsert()
end

--- Open a note in this nvim (called by `sb dash` instead of starting a nested editor).
function M.open(path)
  vim.schedule(function()
    dash_hide()
    local win = editing_window()
    if win then
      vim.api.nvim_set_current_win(win)
    else
      vim.cmd("botright vsplit")
    end
    -- Notes outside the cwd confuse some file trees: work in the vault for this tab.
    if vim.startswith(path, vault() .. "/") and not vim.startswith(path, vim.fn.getcwd() .. "/") then
      vim.cmd.tcd(vim.fn.fnameescape(vault()))
    end
    vim.cmd.edit(vim.fn.fnameescape(path))
  end)
  return ""
end

-- Commands --------------------------------------------------------------------

--- Run `sb <args>` in the background and show its output.
function M.run(args)
  local title = table.concat(args, " ")
  vim.notify("sb " .. title .. "…", vim.log.levels.INFO, { title = "Second Brain" })
  vim.system(vim.list_extend({ "sb" }, args), { text = true, env = { SB_NO_SPINNER = "1" } }, function(r)
    vim.schedule(function()
      local out = vim.trim((r.stdout or "") .. "\n" .. (r.stderr or ""))
      vim.notify(out ~= "" and out or "done", r.code == 0 and vim.log.levels.INFO or vim.log.levels.ERROR, {
        title = "sb " .. title,
      })
      vim.cmd("silent! checktime") -- reload notes `sb` regenerated
    end)
  end)
end

function M.today()
  vim.cmd.edit(vim.fn.fnameescape(vault() .. "/journal/" .. os.date("%Y-%m-%d") .. ".md"))
end

function M.setup(opts)
  M.config = vim.tbl_deep_extend("force", vim.deepcopy(defaults), opts or {})

  -- Entry points used by `sb` through `nvim --server … --remote-expr`.
  _G.SbOpen = M.open
  _G.SbTheme = function()
    vim.schedule(M.apply_theme)
    return 0
  end

  vim.api.nvim_create_user_command("SbDash", M.dash, { desc = "Second Brain dashboard" })
  vim.api.nvim_create_user_command("SbToday", M.today, { desc = "Today's journal" })
  vim.api.nvim_create_user_command("Sb", function(c) M.run(c.fargs) end, {
    nargs = "+",
    desc = "Run an sb command",
    complete = function()
      return { "ingest", "connect", "synth", "today", "plan", "briefing", "tasks", "task", "calendar", "theme", "stats", "doctor" }
    end,
  })

  if M.config.theme then
    if vim.v.vim_did_enter == 1 then
      M.apply_theme()
    else
      vim.api.nvim_create_autocmd("VimEnter", { once = true, callback = M.apply_theme })
    end
  end

  if M.config.keys then
    local map = function(lhs, fn, desc) vim.keymap.set("n", lhs, fn, { desc = desc }) end
    map("<leader>sd", M.dash, "Dashboard (sb dash)")
    map("<leader>sj", M.today, "Today's journal")
    map("<leader>si", function() M.run({ "ingest" }) end, "Ingest journals (AI)")
    map("<leader>sc", function() M.run({ "connect" }) end, "Connect ideas (AI)")
    map("<leader>ss", function() M.run({ "synth" }) end, "Daily synthesis (AI)")
  end
end

return M
