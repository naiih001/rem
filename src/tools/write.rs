use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::json;

use super::ToolError;

#[derive(Debug, Deserialize)]
pub struct WriteArgs {
    pub path: String,
    pub content: String,
}

#[derive(Debug)]
pub struct WriteTool;

impl Tool for WriteTool {
    const NAME: &'static str = "write";
    type Args = WriteArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "Create or overwrite a file with the given content. Parent directories are created as needed.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path of the file to write" },
                "content": { "type": "string", "description": "Full content to write to the file" }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        if let Some(parent) = std::path::Path::new(&args.path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError(format!("write {}: create parent dirs: {e}", args.path)))?;
        }
        std::fs::write(&args.path, &args.content)
            .map_err(|e| ToolError(format!("write {}: {e}", args.path)))?;
        Ok(format!(
            "wrote {} ({} bytes)",
            args.path,
            args.content.len()
        ))
    }
}
