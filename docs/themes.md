# Themes

`rem` reads every color from the active theme. No hardcoded colors remain in
the render path: `history.rs`, `markdown.rs`, and `tui.rs` all take a
`&Theme`.

## Files

- Main config: `~/.config/rem/config.toml` (`[api]`, `[model]`, plus one
  top-level line `theme = "<name>"`).
- Theme files: `~/.config/rem/themes/<name>.toml`.

## Switching

- `/theme` — list installed themes and the current one.
- `/theme <name>` — load `<name>.toml`, apply it live, and save the name to
  `config.toml` so the next launch uses it.
- Unknown names queue an error notice; nothing crashes.

## Inline suggestions (menu)

Typing `/theme ` keeps the slash-menu popup open, now listing installed
theme names filtered by what you typed (`/theme gruvbox` narrows to
`gruvbox-*`). The active theme carries a trailing `●`. Keys work exactly
like the command menu:

- `Up` / `Down` — move through suggestions (wraps, never touches history).
- `Tab` — complete the highlighted name into the input line.
- `Enter` — switch to the highlighted suggestion immediately.
- `Esc` — dismiss suggestions, keep your typed text.

With no themes installed the popup stays closed and `/theme` behaves as
before (prints the current theme name).

## Theme file format

```toml
name = "gruvbox"

[colors]
accent = "#fabd2f"
pane_bg = "#282828"
rail_bg = "#282828"
placeholder = "#665c54"
menu_sel_bg = "#3c3836"

[colors.add]
bg = "#282828"
fg = "#b8bb26"
mark = "#98971a"

[colors.del]
bg = "#282828"
fg = "#fb4934"
mark = "#cc241d"

[colors.markdown]
code_bg = "#282828"
heading_h1 = "#fabd2f"
heading_h2 = "#83a598"
heading_h3 = "#d3869b"
link_fg = "#83a598"
link_url_fg = "#928374"
code_fg = "#ebdbb2"
blockquote_fg = "#928374"
```

Missing keys fall back to the built-in default palette per key. A bad hex
value also falls back instead of erroring. All colors are `#RRGGBB`.

## How it fits the config pattern

The active-theme pointer lives in the shared `Config` struct
(`src/config.rs`) as `ThemeConfig` — the same sub-struct pattern as
`ApiConfig` (`[api]`) and `ModelConfig` (`[model]`). It is a transparent
wrapper so the TOML stays a plain top-level string (`theme = "gruvbox"`).

Two rules keep the pattern honest:

1. `Config::path()` is the single source of truth for where `config.toml`
   lives. `Theme::themes_dir()` derives from it — no second copy of the
   XDG path logic.
2. `Theme::save_active()` edits only the `theme = ...` line in place.
   Comments, `[api]`, and `[model]` survive a `/theme` switch byte-for-byte.
