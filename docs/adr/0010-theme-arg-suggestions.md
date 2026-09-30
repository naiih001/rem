# ADR-0010: Theme-name suggestions in the slash menu

Status: Accepted
Date: 2026-09-26
Context: Typing `/theme ` closed the slash menu (whitespace ends the
bare-`/token` mode from ADR-0007), so users had to know theme names by
heart or run bare `/theme` first and retype. The 10 installed themes in
`~/.config/rem/themes/` were invisible at the moment of typing.

## Decision

- `/theme <partial>` reopens the same popup in a second mode: installed
  theme names filtered by prefix (`match_theme_names`, case-sensitive like
  `menu_matches`). Bare `/theme ` (empty partial) lists everything.
- Rows render as `▸ <name>` (no slash, no desc); the active theme gets a
  trailing `●` so the list answers "which is active" inline.
- Keys mirror the command menu: `Up`/`Down` wrap, `Tab` completes the
  highlighted name into the input, `Enter` switches to it immediately,
  `Esc` dismisses keeping typed text.
- Modes are mutually exclusive by construction (whitespace decides):
  `menu_row_count()` returns one mode's count, so layout, height, and
  selection helpers (`is_menu_open`, `menu_height`, `clamp_menu_sel`)
  needed no branching — only the row source changed.
- Bare `/theme` (no space) stays in command-menu mode; multi-token args
  (`/theme a b`) skip suggestions and dispatch directly to the existing
  unknown-theme error.
- No themes installed → menu stays closed, old behavior unchanged.
- Filesystem errors from `list_themes()` → no suggestions, never a crash.

## Rationale

- One popup, two row sources: reuses the ADR-0007 layout/selection
  machinery (capacity, overflow `+N more`, gap absorption) instead of a
  second widget.
- `theme_arg_partial()` is a pure function over the input string, so the
  trigger logic is unit-testable without theme files on disk; only
  `theme_arg_matches()` touches the filesystem.
- `Enter`-accept mirrors the command prefix-run: highlighted suggestion
  resolves before dispatch, so muscle memory transfers.

## Consequences

- `test`s: `theme_arg_partial_only_matches_single_arg_form` pins the
  trigger (incl. `/themedark` / `/themes` lookalikes); 
  `match_theme_names_filters_by_prefix_in_order` pins filtering;
  `theme_arg_menu_opens_for_partial_and_tab_completes` drives the live
  menu end-to-end (open → filter → navigate → Tab → Enter-switch) and
  restores `~/.config/rem/config.toml` byte-for-byte afterward;
  `theme_arg_enter_without_suggestions_dispatches_normally` pins the
  no-suggestion fallback;
  `theme_arg_menu_renders_name_rows_above_composer` pins painted rows.
- `docs/themes.md` gains an "Inline suggestions" section.
