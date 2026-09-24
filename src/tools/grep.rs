use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::json;

use super::ToolError;

const MAX_MATCHES: usize = 200;
const MAX_LINE_LEN: usize = 500;

#[derive(Debug, Deserialize)]
pub struct GrepArgs {
    pub pattern: String,
    pub path: Option<String>,
}

#[derive(Debug)]
pub struct GrepTool;

impl Tool for GrepTool {
    const NAME: &'static str = "grep";
    type Args = GrepArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "Search file contents for a literal substring `pattern`. Optional `path` is a file or directory (default '.'). Skips `.git/`, `target/`, and binary files. Returns `file:line: text` matches, capped at 200.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Literal substring to search for (not a regex)" },
                "path": { "type": "string", "description": "File or directory to search. Omit for '.'." }
            },
            "required": ["pattern"]
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        if args.pattern.is_empty() {
            return Err(ToolError("grep: pattern must not be empty".to_string()));
        }
        let root = args.path.clone().unwrap_or_else(|| ".".to_string());
        let mut hits: Vec<String> = Vec::new();
        let mut files_scanned: usize = 0;
        search_path(
            std::path::Path::new(&root),
            &args.pattern,
            &mut hits,
            &mut files_scanned,
        )?;
        if hits.is_empty() {
            Ok(format!(
                "no matches for {:?} under {root} ({files_scanned} files)",
                args.pattern
            ))
        } else {
            let mut out = hits.join("\n");
            if hits.len() >= MAX_MATCHES {
                out.push_str(&format!("\n[truncated to {MAX_MATCHES} matches]"));
            }
            Ok(out)
        }
    }
}

fn search_path(
    path: &std::path::Path,
    pattern: &str,
    hits: &mut Vec<String>,
    files_scanned: &mut usize,
) -> Result<(), ToolError> {
    if hits.len() >= MAX_MATCHES {
        return Ok(());
    }
    let rel = path.to_string_lossy();
    if rel.contains(".git/")
        || rel.starts_with(".git")
        || rel.contains("target/")
        || rel.starts_with("target")
    {
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| ToolError(format!("grep {}: {e}", path.display())))?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        let entries = std::fs::read_dir(path)
            .map_err(|e| ToolError(format!("grep {}: {e}", path.display())))?;
        for entry in entries {
            let entry = entry.map_err(|e| ToolError(format!("grep {}: {e}", path.display())))?;
            search_path(&entry.path(), pattern, hits, files_scanned)?;
            if hits.len() >= MAX_MATCHES {
                return Ok(());
            }
        }
        return Ok(());
    }
    // Regular file: skip likely binaries by extension + NUL-byte sniff.
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if matches!(
            ext,
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "ico"
                | "pdf"
                | "zip"
                | "gz"
                | "o"
                | "so"
                | "rlib"
                | "rmeta"
                | "lockb"
        ) {
            return Ok(());
        }
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return Ok(()), // unreadable (permissions): skip, don't fail whole search
    };
    if bytes.contains(&0u8) {
        return Ok(());
    }
    let content = String::from_utf8_lossy(&bytes);
    *files_scanned += 1;
    for (i, line) in content.lines().enumerate() {
        if line.contains(pattern) {
            let snippet: String = line.chars().take(MAX_LINE_LEN).collect();
            hits.push(format!("{}:{}: {}", path.display(), i + 1, snippet.trim()));
            if hits.len() >= MAX_MATCHES {
                return Ok(());
            }
        }
    }
    Ok(())
}
