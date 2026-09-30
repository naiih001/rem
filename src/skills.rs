//! Claude-compatible skills: `<dir>/<name>/SKILL.md` bundles.
//! Discovery: `<project>/.agents/skills` + `~/.config/rem/skills` + `~/.agents/skills`.
//! Identity = folder name; frontmatter `name` ignored (TODO(skills-extras)).

/// Max stacked `/skill:name` mentions expanded per message (Claude: first + 5 more).
pub const MAX_STACKED: usize = 6;
/// Per-skill description cap in preamble listing (Claude: description+when_to_use 1536).
pub const MAX_DESC_CHARS: usize = 1536;
/// Max sibling entries listed per skill in preamble (rest as `+N more`).
pub const MAX_SIBLINGS: usize = 20;

#[derive(Debug, Clone)]
pub struct Skill {
    /// Folder name — canonical identity. Frontmatter `name` ignored.
    pub name: String,
    /// Frontmatter `description` or first non-empty body line fallback.
    pub description: String,
    /// Absolute path to `SKILL.md`.
    pub path: std::path::PathBuf,
    /// Absolute skill dir (bundle root).
    pub dir: std::path::PathBuf,
    /// Sibling files relative to `dir`, sorted, excluding `SKILL.md`.
    pub siblings: Vec<String>,
    /// Full body markdown (after frontmatter, or whole file on lenient fallback).
    pub body: String,
    /// Lenient-parse note (`None` when clean). Surfaced in `/skills` list.
    pub note: Option<String>,
}
