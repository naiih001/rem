# ADR-0009: Themes follow the shared Config struct pattern

Status: Accepted
Date: 2026-09-26
Context: The theme system (merged from `feat/themes`) parsed `config.toml`
through its own private `ThemeToml` struct with a duplicated XDG path, and
`Theme::save_active()` overwrote the whole file with a single
`theme = "<name>"` line — wiping `[api]` and `[model]` on every `/theme`
switch. The config-file side of themes bypassed the `Config` /
`ApiConfig` / `ModelConfig` sub-struct pattern from the TOML-config work.

## Decision

- The active-theme pointer lives in the shared `Config` struct
  (`src/config.rs`) as `ThemeConfig`, alongside `ApiConfig` and
  `ModelConfig`. It is a `#[serde(transparent)]` wrapper over
  `Option<String>` so the TOML keeps the user-chosen one-line form:
  top-level `theme = "<name>"` (not a `[theme]` table). `None` = built-in
  default palette.
- `Config::path()` is the single source of truth for the config file
  location. `Theme::themes_dir()` derives from it; the duplicated
  `config_dir()` / `config_file()` helpers are gone.
- `Theme::load_active()` reads the pointer via `Config::from_file()`, then
  loads `~/.config/rem/themes/<name>.toml`. Unset or unloadable → default.
- `Theme::save_active()` rewrites only the top-level `theme = ...` line in
  place (`upsert_theme_line`): comments, `[api]`, and `[model]` pass through
  byte-for-byte. No-config-file is unreachable in practice (startup requires
  `[api]` + `[model]`) but still writes a valid single line.
- Theme *files* (`themes/<name>.toml`) keep their own shape (`name` +
  `[colors]` tables with per-key default fallback) — that format is
  documented in `docs/themes.md`, not folded into `Config`, because theme
  files are many and user-authored.
- `config.example.toml` gains a commented `# theme = ...` line;
  `docs/themes.md` documents the file format, `/theme` commands, and the
  two pattern rules above.

## Rationale

- Transparent wrapper keeps both promises: Rust code sees the same
  sub-struct pattern as `api`/`model`, while the TOML keeps the one-line
  form the user picked (no migration for existing `theme = ...` files).
- Line-level upsert instead of re-serialization preserves comments and key
  order, which a `toml::to_string` round-trip would destroy.
- Deriving the themes dir from `Config::path()` removes the second XDG
  path computation that could drift.

## Consequences

- `test`s: `theme_defaults_to_none_when_missing` and
  `theme_parses_top_level_string` pin the `Config` side;
  `upsert_theme_line_preserves_api_and_model` and
  `upsert_theme_line_replaces_existing` pin the non-clobbering save
  (the saved text re-parses as a full `Config`).
- Quirk kept deliberately: `ThemeToml` reuses one struct for theme files
  (`name` key) — it no longer parses `config.toml`, so the old
  `theme`-vs-`name` dual-key fallback is gone.
- The pre-existing `live_search_returns_numbered_results` failure (live
  network test, fails on clean `dev`) is unrelated and untouched.
