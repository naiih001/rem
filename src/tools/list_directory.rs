use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::json;

use super::ToolError;

const MAX_ENTRIES: usize = 500;

#[derive(Debug, Deserialize)]
pub struct ListDirectoryArgs {
    pub path: String,
}

#[derive(Debug)]
pub struct ListDirectoryTool;

impl Tool for ListDirectoryTool {
    const NAME: &'static str = "list_directory";
    type Args = ListDirectoryArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "List files and subdirectories at `path` (non-recursive). Each entry is prefixed with `file:` or `dir:`. Errors if the path cannot be read.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list. Use '.' for the project root." }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        let dir = std::path::Path::new(&args.path);
        let entries = std::fs::read_dir(dir)
            .map_err(|e| ToolError(format!("list_directory {}: {e}", args.path)))?;
        let mut lines: Vec<String> = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|e| ToolError(format!("list_directory {}: {e}", args.path)))?;
            let ft = entry
                .file_type()
                .map_err(|e| ToolError(format!("list_directory {}: {e}", args.path)))?;
            let kind = if ft.is_dir() { "dir" } else { "file" };
            lines.push(format!("{kind}: {}", entry.file_name().to_string_lossy()));
            if lines.len() >= MAX_ENTRIES {
                lines.push(format!("[truncated to {MAX_ENTRIES} entries]"));
                break;
            }
        }
        lines.sort();
        if lines.is_empty() {
            Ok(format!("{}: (empty directory)", args.path))
        } else {
            Ok(lines.join("\n"))
        }
    }
}
