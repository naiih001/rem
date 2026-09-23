use async_trait::async_trait;
use rig::{agent::AgentBuilder, client::CompletionClient, completion::Prompt, providers::openai};

use crate::{
    config::Config,
    tools::{BashTool, EditTool, ReadTool, WriteTool},
};

/// Swappable orchestrator abstraction. `RigAgent` is the Rig implementation;
/// a different loop (hand-rolled ReAct, another framework) implements this trait.
#[async_trait]
pub trait AgentLoop {
    async fn chat(&self, prompt: &str) -> Result<String, String>;
}

/// Rig-backed agent: OpenAI-compatible `/chat/completions` model + preamble +
/// the 4 file/shell tools, using Rig's built-in multi-step loop (no hand-rolled ReAct).
pub struct RigAgent {
    agent: rig::agent::Agent,
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
                 Use the read/write/edit/bash tools to inspect and change files. \
                 Bash runs `sh -c` in the project dir (30s timeout) and is unrestricted. \
                 Prefer reading a file before editing it. Keep replies concise.",
            )
            .tool(ReadTool)
            .tool(WriteTool)
            .tool(EditTool)
            .tool(BashTool)
            .default_max_turns(10)
            .build();

        Ok(Self { agent })
    }
}

#[async_trait]
impl AgentLoop for RigAgent {
    async fn chat(&self, prompt: &str) -> Result<String, String> {
        self.agent.prompt(prompt).await.map_err(|e| e.to_string())
    }
}
