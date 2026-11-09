# second_brain.nvim (optional)

The nvim side of `sb`. Plain Lua, no required dependency.

| | |
|---|---|
| `:SbDash` | `sb dash` in a floating terminal. Notes opened from it (Enter) open **in this nvim** instead of a nested editor. |
| `:SbToday` | today's journal (`vault/journal/YYYY-MM-DD.md`) |
| `:Sb <args>` | run any `sb` command in the background, output as a notification |
| theme | the theme picked in `sb dash` (`T`) or with `sb theme <name>` is applied at startup **and live** in every running nvim |

## Install

lazy.nvim:

```lua
{
  dir = "~/src/second-brain/contrib/nvim", -- where you cloned the repository
  opts = {
    home = "~/second-brain", -- or $SB_HOME
    keys = true,             -- <leader>sd dash, sj journal, si/sc/ss AI commands
  },
}
```

Without a plugin manager:

```lua
vim.opt.rtp:append("~/src/second-brain/contrib/nvim")
require("second_brain").setup({ keys = true })
```

## Themes

`sb theme` lists the themes. Each one names an nvim colorscheme, which must be
installed for nvim to follow:

| `sb` theme | colorscheme plugin |
|---|---|
| `kanso-*` | [webhooked/kanso.nvim](https://github.com/webhooked/kanso.nvim) |
| `tokyonight-*` | [folke/tokyonight.nvim](https://github.com/folke/tokyonight.nvim) |
| `astrodark` | [AstroNvim/astrotheme](https://github.com/AstroNvim/astrotheme) |
| `eldritch` | [eldritch-theme/eldritch.nvim](https://github.com/eldritch-theme/eldritch.nvim) |
| `morta` | [philosofonusus/morta.nvim](https://github.com/philosofonusus/morta.nvim) |

Live switching reaches nvim through its server socket: `$NVIM` (the nvim running
`sb dash`) and, on Linux, the default sockets in `$XDG_RUNTIME_DIR`. On other
systems only the nvim hosting the dashboard follows live; others pick the theme
up at their next start.

## Nice with it (not required)

- [render-markdown.nvim](https://github.com/MeanderingProgrammer/render-markdown.nvim) for styled notes
- [obsidian.nvim](https://github.com/obsidian-nvim/obsidian.nvim) for `[[links]]`, backlinks and daily notes (set its daily folder to `journal`, date format `YYYY-MM-DD`)
- [snacks.nvim](https://github.com/folke/snacks.nvim) `image` for images inside notes (kitty / ghostty)

Nothing here calls the model on its own: `ingest`, `connect` and `synth` only run
when you ask.
