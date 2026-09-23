use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::json;

use super::ToolError;

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct SingleEdit {
    pub oldText: String,
    pub newText: String,
}

#[derive(Debug, Deserialize)]
pub struct EditArgs {
    pub path: String,
    pub edits: Vec<SingleEdit>,
}

#[derive(Debug)]
pub struct EditTool;

impl Tool for EditTool {
    const NAME: &'static str = "edit";
    type Args = EditArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "Apply exact string replacements to a file. Each `oldText` must occur exactly once or the whole call fails. Edits apply sequentially.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path of the file to edit" },
                "edits": {
                    "type": "array",
                    "description": "Replacements to apply in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "Exact text to find (must be unique in the file)" },
                            "newText": { "type": "string", "description": "Replacement text" }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        let mut content = std::fs::read_to_string(&args.path)
            .map_err(|e| ToolError(format!("edit {}: {e}", args.path)))?;
        let mut applied = 0;
        for e in &args.edits {
            let count = content.matches(e.oldText.as_str()).count();
            if count == 0 {
                return Err(ToolError(format!(
                    "edit {}: oldText not found: {:?}",
                    args.path,
                    trunc(&e.oldText)
                )));
            }
            if count > 1 {
                return Err(ToolError(format!(
                    "edit {}: oldText matches {count} times, must be unique: {:?}",
                    args.path,
                    trunc(&e.oldText)
                )));
            }
            content = content.replacen(e.oldText.as_str(), &e.newText, 1);
            applied += 1;
        }
        std::fs::write(&args.path, &content)
            .map_err(|e| ToolError(format!("edit {}: write back: {e}", args.path)))?;
        Ok(format!(
            "applied {applied} edit(s) to {} ({} bytes)",
            args.path,
            content.len()
        ))
    }
}

fn trunc(s: &str) -> String {
    const N: usize = 120;
    if s.len() <= N {
        s.to_string()
    } else {
        format!("{}…", &s[..N])
    }
}
