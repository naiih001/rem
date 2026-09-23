pub mod bash;
pub mod edit;
pub mod git;
pub mod glob_tool;
pub mod grep;
pub mod list_directory;
pub mod read;
pub mod write;

pub use bash::BashTool;
pub use edit::EditTool;
pub use git::{GitDiffTool, GitStatusTool};
pub use glob_tool::GlobTool;
pub use grep::GrepTool;
pub use list_directory::ListDirectoryTool;
pub use read::ReadTool;
pub use write::WriteTool;

use std::fmt;

/// Shared error type for all tools. Rig's `Tool::Error` must implement
/// `std::error::Error`, so we wrap a plain message.
#[derive(Debug)]
pub struct ToolError(pub String);

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ToolError {}
