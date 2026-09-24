use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::json;

use super::ToolError;

const MAX_MATCHES: usize = 500;

#[derive(Debug, Deserialize)]
pub struct GlobArgs {
    pub pattern: String,
}

#[derive(Debug)]
pub struct GlobTool;

impl Tool for GlobTool {
    const NAME: &'static str = "glob";
    type Args = GlobArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "Find files by glob `pattern` (e.g. `src/**/*.rs`). Results sorted, capped at 500 paths. Respects `.gitignore`-style skips for `.git/` and `target/` implicitly by filtering them out.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. 'src/**/*.rs'" }
            },
            "required": ["pattern"]
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        if args.pattern.is_empty() {
            return Err(ToolError("glob: pattern must not be empty".to_string()));
        }
        let mut paths: Vec<String> = Vec::new();
        let entries = glob::glob(&args.pattern)
            .map_err(|e| ToolError(format!("glob {:?}: bad pattern: {e}", args.pattern)))?;
        for entry in entries {
            match entry {
                Ok(p) => {
                    let s = p.to_string_lossy().into_owned();
                    if s.contains(".git/")
                        || s.starts_with(".git")
                        || s.contains("target/")
                        || s.starts_with("target")
                    {
                        continue;
                    }
                    paths.push(s);
                    if paths.len() >= MAX_MATCHES {
                        break;
                    }
                }
                Err(e) => {
                    return Err(ToolError(format!("glob {:?}: {e}", args.pattern)));
                }
            }
        }
        paths.sort();
        if paths.is_empty() {
            Ok(format!("no files match {:?}", args.pattern))
        } else if paths.len() >= MAX_MATCHES {
            Ok(format!(
                "{}\n[truncated to {MAX_MATCHES} paths]",
                paths.join("\n")
            ))
        } else {
            Ok(paths.join("\n"))
        }
    }
}
