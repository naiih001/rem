use std::time::Duration;

use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::json;

use super::ToolError;

const TIMEOUT_SECS: u64 = 30;
const MAX_BYTES: usize = 8000;

#[derive(Debug, Deserialize)]
pub struct GitStatusArgs {}

#[derive(Debug)]
pub struct GitStatusTool;

impl Tool for GitStatusTool {
    const NAME: &'static str = "git_status";
    type Args = GitStatusArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "Run `git status --short` in the project directory. 30s timeout. Output truncated to 8000 bytes.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, _args: Self::Args) -> Result<String, ToolError> {
        run_git(vec!["status".to_string(), "--short".to_string()]).await
    }
}

#[derive(Debug, Deserialize)]
pub struct GitDiffArgs {
    pub path: Option<String>,
    pub staged: Option<bool>,
}

#[derive(Debug)]
pub struct GitDiffTool;

impl Tool for GitDiffTool {
    const NAME: &'static str = "git_diff";
    type Args = GitDiffArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "Run `git diff` in the project directory. Optional `path` limits to one file, optional `staged` shows `git diff --staged`. 30s timeout, truncated to 8000 bytes.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Optional file path to limit the diff to" },
                "staged": { "type": "boolean", "description": "If true, run `git diff --staged`. Omit for unstaged." }
            }
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        let mut argv: Vec<String> = vec!["diff".to_string()];
        if args.staged.unwrap_or(false) {
            argv.push("--staged".to_string());
        }
        if let Some(p) = args.path.as_deref()
            && !p.is_empty()
        {
            argv.push("--".to_string());
            argv.push(p.to_string());
        }
        run_git(argv).await
    }
}

async fn run_git(argv: Vec<String>) -> Result<String, ToolError> {
    let child = tokio::process::Command::new("git")
        .args(&argv)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ToolError(format!("git spawn: {e}")))?;

    let output = tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), child.wait_with_output())
        .await
        .map_err(|_| {
            ToolError(format!(
                "git {}: timed out after {TIMEOUT_SECS}s",
                argv.join(" ")
            ))
        })?
        .map_err(|e| ToolError(format!("git wait: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ToolError(format!(
            "git {} failed (exit {}): {}",
            argv.join(" "),
            output
                .status
                .code()
                .map_or("signal".to_string(), |c| c.to_string()),
            stderr.trim()
        )));
    }

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

    if combined.trim().is_empty() {
        Ok("(clean)".to_string())
    } else {
        Ok(format!("{combined}{note}"))
    }
}
