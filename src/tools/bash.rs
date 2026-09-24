use std::time::Duration;

use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::json;

use super::ToolError;

const TIMEOUT_SECS: u64 = 30;
const MAX_BYTES: usize = 8000;

#[derive(Debug, Deserialize)]
pub struct BashArgs {
    pub command: String,
}

#[derive(Debug)]
pub struct BashTool;

impl Tool for BashTool {
    const NAME: &'static str = "bash";
    type Args = BashArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "Run a shell command via `sh -c` in the project directory. 30s timeout. Stdout and stderr are combined and truncated to 8000 bytes. Unrestricted.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run" }
            },
            "required": ["command"]
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&args.command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ToolError(format!("bash spawn: {e}")))?;

        let output =
            tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), child.wait_with_output())
                .await
                .map_err(|_| {
                    ToolError(format!(
                        "bash: timed out after {TIMEOUT_SECS}s: {}",
                        args.command
                    ))
                })?
                .map_err(|e| ToolError(format!("bash wait: {e}")))?;

        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str("[stderr]\n");
            combined.push_str(&stderr);
        }

        let mut note = String::new();
        if combined.len() > MAX_BYTES {
            combined.truncate(MAX_BYTES);
            note = format!(" [truncated to {MAX_BYTES} bytes]");
        }

        Ok(format!(
            "exit: {}{note}\n{combined}",
            output
                .status
                .code()
                .map_or("signal".to_string(), |c| c.to_string())
        ))
    }
}
