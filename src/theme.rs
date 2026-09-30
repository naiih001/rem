use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use ratatui::style::Color;
use serde::Deserialize;

use crate::config::Config;

/// Full application theme. Every hardcoded color in the app maps to a field
/// here. Missing keys in the TOML fall back to `Theme::default()` values.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    // Pane
    pub pane_bg: Color,
    pub rail_bg: Color,
    pub accent: Color,
    pub placeholder: Color,
    pub menu_sel_bg: Color,
    // Diff — additions
    pub add_bg: Color,
    pub add_fg: Color,
    pub add_mark: Color,
    // Diff — deletions
    pub del_bg: Color,
    pub del_fg: Color,
    pub del_mark: Color,
    // Markdown
    pub code_bg: Color,
    pub heading_h1: Color,
    pub heading_h2: Color,
    pub heading_h3: Color,
    pub link_fg: Color,
    pub link_url_fg: Color,
    pub code_fg: Color,
    pub blockquote_fg: Color,
}

// ── Default (current dark palette) ──────────────────────────────────────

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "default".into(),
            pane_bg: Color::Rgb(0x19, 0x19, 0x19),
            rail_bg: Color::Rgb(0x19, 0x19, 0x19),
            accent: Color::Rgb(242, 118, 79),
            placeholder: Color::Rgb(0x4d, 0x4d, 0x4d),
            menu_sel_bg: Color::Rgb(0x2e, 0x2e, 0x2e),
            add_bg: Color::Rgb(0x12, 0x24, 0x0f),
            add_fg: Color::Rgb(0x9e, 0xcb, 0x84),
            add_mark: Color::Rgb(0x5f, 0x8f, 0x4a),
            del_bg: Color::Rgb(0x2a, 0x15, 0x18),
            del_fg: Color::Rgb(0xe0, 0x8b, 0x8b),
            del_mark: Color::Rgb(0xa3, 0x4f, 0x4f),
            code_bg: Color::Rgb(0x19, 0x19, 0x19),
            heading_h1: Color::Cyan,
            heading_h2: Color::Blue,
            heading_h3: Color::Magenta,
            link_fg: Color::DarkGray,
            link_url_fg: Color::DarkGray,
            code_fg: Color::Gray,
            blockquote_fg: Color::DarkGray,
        }
    }
}

// ── TOML deserialization ────────────────────────────────────────────────

/// Shape of a `~/.config/rem/themes/<name>.toml` theme file.
/// (Theme files use `name`; the main `config.toml` points at them with a
/// top-level `theme = "<name>"` parsed via [`Config`].)
#[derive(Debug, Deserialize, Default)]
struct ThemeToml {
    name: Option<String>,
    colors: Option<ColorsToml>,
}

#[derive(Debug, Deserialize, Default)]
struct ColorsToml {
    accent: Option<String>,
    pane_bg: Option<String>,
    rail_bg: Option<String>,
    placeholder: Option<String>,
    menu_sel_bg: Option<String>,
    add: Option<DiffToml>,
    del: Option<DiffToml>,
    markdown: Option<MarkdownToml>,
}

#[derive(Debug, Deserialize, Default)]
struct DiffToml {
    bg: Option<String>,
    fg: Option<String>,
    mark: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct MarkdownToml {
    code_bg: Option<String>,
    heading_h1: Option<String>,
    heading_h2: Option<String>,
    heading_h3: Option<String>,
    link_fg: Option<String>,
    link_url_fg: Option<String>,
    code_fg: Option<String>,
    blockquote_fg: Option<String>,
}

// ── Hex parsing ─────────────────────────────────────────────────────────

fn parse_hex(hex: &str) -> Result<Color> {
    let hex = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).context("invalid hex red component")?;
            let g = u8::from_str_radix(&hex[2..4], 16).context("invalid hex green component")?;
            let b = u8::from_str_radix(&hex[4..6], 16).context("invalid hex blue component")?;
            Ok(Color::Rgb(r, g, b))
        }
        _ => anyhow::bail!("hex color must be #RRGGBB (6 digits), got: {hex}"),
    }
}

fn opt_color(s: Option<&str>, fallback: Color) -> Color {
    s.and_then(|v| parse_hex(v).ok()).unwrap_or(fallback)
}

// ── Loading ─────────────────────────────────────────────────────────────

/// Insert or replace the top-level `theme = "<name>"` line in the raw
/// `config.toml` text. Every other line passes through untouched, so
/// comments and the `[api]` / `[model]` blocks survive a `/theme` switch.
fn upsert_theme_line(existing: &str, name: &str) -> String {
    let replacement = format!("theme = \"{name}\"");
    let mut replaced = false;
    let mut out: Vec<String> = Vec::new();
    for line in existing.lines() {
        let trimmed = line.trim_start();
        // Skip comments and indented keys inside `[tables]`.
        let is_top_level_theme = !line.starts_with([' ', '\t'])
            && (trimmed == "theme" || trimmed.starts_with("theme ="));
        if is_top_level_theme && !replaced {
            out.push(replacement.clone());
            replaced = true;
        } else if is_top_level_theme {
            // Drop duplicate theme lines; the first one wins.
            continue;
        } else {
            out.push(line.to_string());
        }
    }
    if !replaced {
        out.insert(0, replacement);
    }
    let mut text = out.join("\n");
    // Preserve the file's trailing newline convention.
    if existing.ends_with('\n') || !existing.is_empty() {
        text.push('\n');
    }
    text
}

impl Theme {
    /// Parse a `.toml` theme file. Missing keys fall back to `Theme::default()`.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading theme file {}", path.display()))?;
        let toml: ThemeToml =
            toml::from_str(&text).with_context(|| format!("parsing theme {}", path.display()))?;
        let def = Self::default();
        let colors = toml.colors.unwrap_or_default();
        let add = colors.add.unwrap_or_default();
        let del = colors.del.unwrap_or_default();
        let md = colors.markdown.unwrap_or_default();
        let name = toml.name.unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });

        Ok(Self {
            name,
            pane_bg: opt_color(colors.pane_bg.as_deref(), def.pane_bg),
            rail_bg: opt_color(colors.rail_bg.as_deref(), def.rail_bg),
            accent: opt_color(colors.accent.as_deref(), def.accent),
            placeholder: opt_color(colors.placeholder.as_deref(), def.placeholder),
            menu_sel_bg: opt_color(colors.menu_sel_bg.as_deref(), def.menu_sel_bg),
            add_bg: opt_color(add.bg.as_deref(), def.add_bg),
            add_fg: opt_color(add.fg.as_deref(), def.add_fg),
            add_mark: opt_color(add.mark.as_deref(), def.add_mark),
            del_bg: opt_color(del.bg.as_deref(), def.del_bg),
            del_fg: opt_color(del.fg.as_deref(), def.del_fg),
            del_mark: opt_color(del.mark.as_deref(), def.del_mark),
            code_bg: opt_color(md.code_bg.as_deref(), def.code_bg),
            heading_h1: opt_color(md.heading_h1.as_deref(), def.heading_h1),
            heading_h2: opt_color(md.heading_h2.as_deref(), def.heading_h2),
            heading_h3: opt_color(md.heading_h3.as_deref(), def.heading_h3),
            link_fg: opt_color(md.link_fg.as_deref(), def.link_fg),
            link_url_fg: opt_color(md.link_url_fg.as_deref(), def.link_url_fg),
            code_fg: opt_color(md.code_fg.as_deref(), def.code_fg),
            blockquote_fg: opt_color(md.blockquote_fg.as_deref(), def.blockquote_fg),
        })
    }

    /// Path to the themes directory. Derived from [`Config::path`] so the
    /// config location has a single source of truth.
    fn themes_dir() -> Result<PathBuf> {
        let config_file = Config::path().map_err(anyhow::Error::msg)?;
        Ok(config_file
            .parent()
            .map(|p| p.join("themes"))
            .unwrap_or_else(|| PathBuf::from("themes")))
    }

    /// List available theme names (basenames without `.toml`).
    pub fn list_themes() -> Result<Vec<String>> {
        let dir = Self::themes_dir()?;
        if !dir.is_dir() {
            return Ok(vec![]);
        }
        let mut names: Vec<String> = fs::read_dir(&dir)
            .with_context(|| format!("reading themes dir {}", dir.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "toml")
                    .unwrap_or(false)
            })
            .filter_map(|e| {
                e.path()
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// Load a named theme from the themes dir (`<name>.toml`).
    pub fn load_named(name: &str) -> Result<Self> {
        let path = Self::themes_dir()?.join(format!("{name}.toml"));
        Self::load(&path)
    }

    /// Load the active theme named by `config.toml` (via [`Config`]).
    /// Falls back to `Theme::default()` when unset or unloadable.
    pub fn load_active() -> Self {
        let name = Config::from_file().ok().and_then(|c| c.theme.name);
        match name {
            Some(n) => Self::load_named(&n).unwrap_or_default(),
            None => Self::default(),
        }
    }

    /// Persist the active theme name to `config.toml`.
    /// Only the top-level `theme = "<name>"` line is touched — every other
    /// line (comments, `[api]`, `[model]`) is preserved byte-for-byte.
    pub fn save_active(name: &str) -> Result<()> {
        let config_file = Config::path().map_err(anyhow::Error::msg)?;
        if let Some(parent) = config_file.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }

        let updated = match fs::read_to_string(&config_file) {
            Ok(existing) => upsert_theme_line(&existing, name),
            // No config file yet: the app cannot have started (startup
            // requires `[api]` + `[model]`), so this is unreachable in
            // practice. Still, write a valid top-level line.
            Err(_) => format!("theme = \"{name}\"\n"),
        };
        fs::write(&config_file, updated)
            .with_context(|| format!("writing config file {}", config_file.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_valid() {
        assert_eq!(parse_hex("#ff00ff").unwrap(), Color::Rgb(255, 0, 255));
        assert_eq!(parse_hex("000000").unwrap(), Color::Rgb(0, 0, 0));
        assert_eq!(parse_hex("#FFFFFF").unwrap(), Color::Rgb(255, 255, 255));
    }

    #[test]
    fn parse_hex_invalid() {
        assert!(parse_hex("#fff").is_err());
        assert!(parse_hex("zzzzzz").is_err());
    }

    #[test]
    fn load_full_toml_parses_every_section() {
        let dir = std::env::temp_dir().join("rem-theme-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gruvbox.toml");
        std::fs::write(
            &path,
            r##"
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
"##,
        )
        .unwrap();
        let t = Theme::load(&path).unwrap();
        assert_eq!(t.name, "gruvbox");
        assert_eq!(t.accent, Color::Rgb(0xfa, 0xbd, 0x2f));
        assert_eq!(t.add_fg, Color::Rgb(0xb8, 0xbb, 0x26));
        assert_eq!(t.del_mark, Color::Rgb(0xcc, 0x24, 0x1d));
        assert_eq!(t.heading_h2, Color::Rgb(0x83, 0xa5, 0x98));
        assert_eq!(t.code_fg, Color::Rgb(0xeb, 0xdb, 0xb2));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_partial_toml_falls_back_per_key() {
        let dir = std::env::temp_dir().join("rem-theme-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("partial.toml");
        // Only accent set; everything else must equal the default.
        // A bad hex value also falls back instead of erroring.
        std::fs::write(
            &path,
            "[colors]\naccent = \"#ff0000\"\nrail_bg = \"nope\"\n",
        )
        .unwrap();
        let t = Theme::load(&path).unwrap();
        let def = Theme::default();
        assert_eq!(t.accent, Color::Rgb(0xff, 0, 0));
        assert_eq!(t.rail_bg, def.rail_bg);
        assert_eq!(t.pane_bg, def.pane_bg);
        assert_eq!(t.add_bg, def.add_bg);
        assert_eq!(t.heading_h1, def.heading_h1);
        // Name defaults to the file stem when unset.
        assert_eq!(t.name, "partial");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_rejects_missing_file_and_corrupt_toml() {
        let dir = std::env::temp_dir().join("rem-theme-test");
        assert!(Theme::load(&dir.join("does-not-exist.toml")).is_err());
        let path = dir.join("corrupt.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "[colors\nthis is not toml").unwrap();
        assert!(Theme::load(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn upsert_theme_line_preserves_api_and_model() {
        let existing = "# my config\n\n[api]\nkey = \"sk-x\"\nbase_url = \"https://x\"\n\n[model]\ndefault = \"m\"\neffort = \"high\"\n";
        let out = upsert_theme_line(existing, "gruvbox");
        assert!(
            out.contains("theme = \"gruvbox\""),
            "theme line missing: {out}"
        );
        assert!(out.contains("[api]"), "api block lost: {out}");
        assert!(out.contains("key = \"sk-x\""), "api key lost: {out}");
        assert!(out.contains("[model]"), "model block lost: {out}");
        assert!(out.contains("# my config"), "comment lost: {out}");
        // The result must still parse as a full Config.
        let cfg: crate::config::Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.theme.name.as_deref(), Some("gruvbox"));
        assert_eq!(cfg.api.key, "sk-x");
        assert_eq!(cfg.model.effort, "high");
    }

    #[test]
    fn upsert_theme_line_replaces_existing() {
        let existing = "theme = \"old\"\n\n[api]\nkey = \"k\"\n";
        let out = upsert_theme_line(existing, "new");
        assert_eq!(
            out.matches("theme =").count(),
            1,
            "duplicate theme lines: {out}"
        );
        assert!(out.contains("theme = \"new\""), "not replaced: {out}");
        assert!(out.contains("[api]"), "api block lost: {out}");
    }

    #[test]
    fn default_theme_has_all_fields() {
        let t = Theme::default();
        assert_eq!(t.name, "default");
        // Just spot-check a few — Default impl is the source of truth.
        assert_eq!(t.accent, Color::Rgb(242, 118, 79));
        assert_eq!(t.pane_bg, Color::Rgb(0x19, 0x19, 0x19));
    }
}
