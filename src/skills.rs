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

/// Parse one `SKILL.md`. Lenient (Claude rule):
/// - Opening `---` must be line 1, else whole file is body.
/// - YAML parse failure → usable body, name-only + note (direct invoke still works).
/// - Missing/empty `description` → first non-empty body line; truncate to MAX_DESC_CHARS.
/// - Frontmatter `name` ignored (folder wins). Unknown fields ignored.
/// - TODO(skills-extras): `disable-model-invocation`, `user-invocable`, `allowed-tools`, `when_to_use`.
pub fn parse_skill_file(name: &str, path: &std::path::Path) -> Result<Skill, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."));
    let (desc_opt, body, mut note) = split_frontmatter(&raw);
    let mut description = desc_opt.unwrap_or_default();
    if description.trim().is_empty() {
        description = first_nonempty_line(&body).unwrap_or_default();
        if note.is_none() { note = Some("no description; using first body line".to_string()); }
    }
    let description: String = description.chars().take(MAX_DESC_CHARS).collect();
    let siblings = list_siblings(&dir);
    Ok(Skill { name: name.to_string(), description, path: path.to_path_buf(), dir, siblings, body, note })
}

fn split_frontmatter(raw: &str) -> (Option<String>, String, Option<String>) {
    let mut lines = raw.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, raw.to_string(), Some("missing frontmatter; whole file is body".to_string()));
    }
    let mut fm = Vec::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" { closed = true; break; }
        fm.push(line);
    }
    if !closed {
        return (None, raw.to_string(), Some("unclosed frontmatter; whole file is body".to_string()));
    }
    // Body = lines after closing marker.
    let all: Vec<&str> = raw.lines().collect();
    let body_start = fm.len() + 2; // opening --- + fm lines + closing ---
    let body = all.iter().skip(body_start).cloned().collect::<Vec<_>>().join("\n");
    let fm_text = fm.join("\n");
    match parse_description(&fm_text) {
        Ok(d) => (d, body, None),
        Err(e) => (None, body, Some(format!("bad YAML ({e}); loaded name-only"))),
    }
}

/// Minimal YAML: find `description:` scalar (inline or `|`/`>` block). Returns Ok(None) when absent.
/// Err only on obvious corruption we flag (e.g. unclosed flow); otherwise Ok.
fn parse_description(fm: &str) -> Result<Option<String>, String> {
    let lines: Vec<&str> = fm.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed.starts_with("description:") {
            let rest = trimmed["description:".len()..].trim();
            if rest.starts_with('[') && !rest.contains(']') { return Err("unclosed flow".to_string()); }
            if rest.starts_with('{') && !rest.contains('}') { return Err("unclosed flow".to_string()); }
            if rest == "|" || rest == ">" {
                i += 1;
                let mut buf = Vec::new();
                while i < lines.len() && (lines[i].starts_with(' ') || lines[i].starts_with('\t') || lines[i].trim().is_empty()) {
                    buf.push(lines[i].trim());
                    i += 1;
                }
                let joined = buf.join(" ");
                let t = joined.trim();
                if t.is_empty() { return Ok(None); }
                return Ok(Some(t.to_string()));
            }
            let v = rest.trim_matches('"').trim_matches('\'').trim().to_string();
            if v.is_empty() { return Ok(None); }
            return Ok(Some(v));
        }
        i += 1;
    }
    Ok(None)
}

fn first_nonempty_line(body: &str) -> Option<String> {
    body.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

fn list_siblings(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out; };
    let mut stack: Vec<std::path::PathBuf> = entries.filter_map(|e| e.ok().map(|x| x.path())).collect();
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            if let Ok(es) = std::fs::read_dir(&p) {
                for e in es.filter_map(|x| x.ok()) { stack.push(e.path()); }
            }
            continue;
        }
        if p.file_name().map(|n| n == "SKILL.md").unwrap_or(false) { continue; }
        if let Ok(rel) = p.strip_prefix(dir) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write_tmp(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("SKILL.md");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn parses_clean_frontmatter() {
        let dir = std::env::temp_dir().join("rem-skill-test-clean");
        let _ = std::fs::remove_dir_all(&dir);
        let p = write_tmp(&dir, "---\ndescription: Helps with PDFs.\n---\n\n# PDF\nDo things.\n");
        let s = parse_skill_file("pdf", &p).unwrap();
        assert_eq!(s.description, "Helps with PDFs.");
        assert!(s.body.contains("# PDF"));
        assert!(s.note.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_description_falls_back_to_first_body_line() {
        let dir = std::env::temp_dir().join("rem-skill-test-nodesc");
        let _ = std::fs::remove_dir_all(&dir);
        let p = write_tmp(&dir, "---\nname: ignored\n---\n\nFirst line here.\nSecond.\n");
        let s = parse_skill_file("pdf", &p).unwrap();
        assert_eq!(s.description, "First line here.");
        assert!(s.note.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_yaml_still_loads_name_only() {
        let dir = std::env::temp_dir().join("rem-skill-test-badyaml");
        let _ = std::fs::remove_dir_all(&dir);
        let p = write_tmp(&dir, "---\ndescription: [unclosed\n---\n\nBody line.\n");
        let s = parse_skill_file("pdf", &p).unwrap();
        assert_eq!(s.name, "pdf");
        assert!(!s.body.is_empty());
        assert!(s.note.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_siblings_sorted_excluding_skill_md() {
        let dir = std::env::temp_dir().join("rem-skill-test-sib");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("SKILL.md"), "x\n").unwrap();
        std::fs::write(dir.join("reference.md"), "x\n").unwrap();
        std::fs::write(dir.join("scripts/helper.py"), "x\n").unwrap();
        let sibs = list_siblings(&dir);
        assert_eq!(sibs, vec!["reference.md".to_string(), "scripts/helper.py".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_frontmatter_uses_whole_file() {
        let dir = std::env::temp_dir().join("rem-skill-test-nofm");
        let _ = std::fs::remove_dir_all(&dir);
        let p = write_tmp(&dir, "# Just markdown\nNo frontmatter.\n");
        let s = parse_skill_file("pdf", &p).unwrap();
        assert_eq!(s.description, "# Just markdown");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
