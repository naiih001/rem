use async_trait::async_trait;
use rig::{
    agent::AgentBuilder,
    client::CompletionClient,
    completion::{Chat, Prompt},
    message::ToolChoice,
    providers::openai,
};

use crate::{
    config::Config,
    context::Context,
    tools::{
        BashTool, EditTool, GitDiffTool, GitStatusTool, GlobTool, GrepTool, ListDirectoryTool,
        ReadTool, WebFetchTool, WebSearchTool, WriteTool,
    },
};

/// Swappable orchestrator abstraction. `RigAgent` is the Rig implementation;
/// a different loop (hand-rolled ReAct, another framework) implements this trait.
#[async_trait]
pub trait AgentLoop {
    async fn chat(&self, prompt: &str) -> Result<String, String>;
}

/// Rig-backed agent: OpenAI-compatible `/chat/completions` model + preamble +
/// the 11 file/shell/git/search/web tools, using Rig's built-in multi-step loop (no hand-rolled ReAct).
/// Conversation history is caller-owned in [`Context`]: tool outputs are
/// truncated on entry and old turns are summarized past 100 messages.
pub struct RigAgent {
    agent: rig::agent::Agent,
    context: tokio::sync::Mutex<Context>,
}

impl RigAgent {
    pub fn new(cfg: &Config) -> Result<Self, String> {
        // Explicit `.base_url()` per spec — no reliance on OPENAI_BASE_URL env.
        let client = openai::CompletionsClient::builder()
            .api_key(cfg.api_key.clone())
            .base_url(cfg.base_url.clone())
            .build()
            .map_err(|e| format!("failed to build LLM client: {e}"))?;

        let model = client.completion_model(cfg.model.clone());

        let agent = AgentBuilder::new(model)
            .preamble(
                "You are rem, a coding agent in a terminal TUI. \
                 Use the read/write/edit/bash/list_directory/git_status/git_diff/grep/glob/web_fetch/web_search tools to inspect and change files. \
                 list_directory lists a dir, glob finds files by pattern, grep searches contents, git_status/git_diff inspect git state. \
                 web_search searches the web (DuckDuckGo, no key), web_fetch reads a URL as text. \
                 Bash runs `sh -c` in the project dir (30s timeout) and is unrestricted. \
                 Prefer reading a file before editing it. Keep replies concise.",
            )
            .tool(ReadTool)
            .tool(WriteTool)
            .tool(EditTool)
            .tool(BashTool)
            .tool(ListDirectoryTool)
            .tool(GitStatusTool)
            .tool(GitDiffTool)
            .tool(GrepTool)
            .tool(GlobTool)
            .tool(WebFetchTool)
            .tool(WebSearchTool)
            .default_max_turns(10)
            .build();

        Ok(Self {
            agent,
            context: tokio::sync::Mutex::new(Context::new()),
        })
    }
}

#[async_trait]
impl AgentLoop for RigAgent {
    async fn chat(&self, prompt: &str) -> Result<String, String> {
        const SUMMARIZER_PROMPT: &str = "Summarize this conversation so far for continued work. \
            Preserve: filenames touched, decisions made, errors seen, and any pending tasks. \
            Keep it under 1500 chars. Reply with the summary only.";

        // Lock once for the whole turn: history mutation + compaction are atomic
        // w.r.t. other chat() calls. The TUI serializes turns anyway.
        let mut ctx = self.context.lock().await;
        let before = ctx.len();

        let reply = Chat::chat(&self.agent, prompt, ctx.messages_mut())
            .await
            .map_err(|e| e.to_string())?;

        // Truncate newly committed tool outputs so they don't eat the window.
        ctx.truncate_new(before);

        // Compact past the trigger: summarize oldest, keep newest verbatim.
        if let Some(drained) = ctx.take_for_compaction() {
            let material = Context::render_for_summary(&drained);
            // Summarize WITHOUT tools and WITHOUT history: a plain one-shot
            // call so the summary can't recurse into compaction or tool loops.
            match self
                .agent
                .prompt(material)
                .preamble(SUMMARIZER_PROMPT)
                .tool_choice(ToolChoice::None)
                .await
            {
                Ok(summary) => ctx.apply_summary(summary),
                Err(e) => {
                    // Summarizer failed: put the drained messages back instead
                    // of silently dropping history. Next turn retries.
                    let mut restored = drained;
                    restored.extend(ctx.messages_mut().drain(..));
                    *ctx.messages_mut() = restored;
                    return Err(format!("context compaction failed, history restored: {e}"));
                }
            }
        }

        Ok(reply)
    }
}
