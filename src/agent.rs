use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rig::{
    agent::AgentBuilder,
    agent::{
        AgentHook, CompletionResponseEvent, HookContext, ObservationAction, ToolResultAction,
        ToolResultEvent,
    },
    client::CompletionClient,
    completion::{Chat, Prompt},
    message::{AssistantContent, ToolChoice},
    providers::openai,
};

use crate::{
    config::Config,
    context::Context,
    permissions::{ApprovalTx, PermissionHook},
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
    /// Tool calls recorded during the most recent `chat` turn, in order.
    /// Empty before the first turn. Used by the TUI to render the Aster-style
    /// work tree. Defaulted so other `AgentLoop` impls need no changes.
    fn last_tool_events(&self) -> Vec<ToolEvent> {
        Vec::new()
    }
    /// True LLM reasoning texts captured during the most recent `chat` turn,
    /// in order. Empty when the provider emitted no `Reasoning` blocks.
    /// Defaulted so other `AgentLoop` impls need no changes. Recorded
    /// internally but hidden from the transcript (ADR-0005).
    #[allow(dead_code)]
    fn last_reasoning(&self) -> Vec<String> {
        Vec::new()
    }
    /// Model id for the TUI footer. Defaulted for other impls.
    fn model_name(&self) -> String {
        "model".to_string()
    }
    /// Effort level for the TUI footer and the `reasoning_effort` request
    /// param. Defaulted for other impls.
    fn effort_name(&self) -> String {
        "medium".to_string()
    }
    async fn export_messages_json(&self) -> Result<String, String> { Err("not supported".to_string()) }
    async fn import_messages_json(&self, _json: &str) -> Result<(), String> { Err("not supported".to_string()) }
    fn export_sync(&self) -> Result<String, String> { Err("not supported".to_string()) }
    fn import_sync(&self, _json: &str) -> Result<(), String> { Err("not supported".to_string()) }
}

/// One tool execution observed during a turn: what ran, with what args,
/// whether it succeeded, a short human-readable result summary, and the full
/// output text (for inline expandable display).
#[derive(Debug, Clone)]
pub struct ToolEvent {
    pub name: String,
    pub args: serde_json::Value,
    pub ok: bool,
    pub summary: String,
    /// Full rendered tool output; may be multi-line. Truncated to
    /// [`FULL_OUTPUT_CAP`] chars so the transcript stores a bounded copy.
    pub output: String,
}

impl ToolEvent {
    /// Short arg preview for the tree leaf, e.g. `src/main.rs` or `cargo test`.
    pub fn arg_preview(&self) -> String {
        preview_args(&self.name, &self.args)
    }
}

fn preview_args(name: &str, args: &serde_json::Value) -> String {
    let get = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string);
    match name {
        "read" | "write" | "edit" | "list_directory" => {
            get("path").unwrap_or_else(|| compact_args(args))
        }
        "bash" => get("command").unwrap_or_else(|| compact_args(args)),
        "grep" => match (get("pattern"), get("path")) {
            (Some(p), Some(path)) => format!("{p} in {path}"),
            (Some(p), None) => p,
            _ => compact_args(args),
        },
        "glob" => get("pattern").unwrap_or_else(|| compact_args(args)),
        "git_diff" => get("path").unwrap_or_else(|| "repo".to_string()),
        "git_status" => "repo".to_string(),
        "web_fetch" => get("url").unwrap_or_else(|| compact_args(args)),
        "web_search" => get("query").unwrap_or_else(|| compact_args(args)),
        _ => compact_args(args),
    }
}

fn compact_args(args: &serde_json::Value) -> String {
    const N: usize = 80;
    let s = args.to_string();
    match s.chars().count() > N {
        true => format!("{}…", s.chars().take(N - 1).collect::<String>()),
        false => s,
    }
}

/// Bound on the full output text stored per tool event for inline display.
/// Long outputs render truncated with expand; the stored copy stays bounded.
pub const FULL_OUTPUT_CAP: usize = 4000;

/// [`AgentHook`] that records every tool result plus true LLM reasoning text
/// into shared vecs. Registered via `AgentBuilder::add_hook`; fires for
/// successes, failures, skips, and refusals without altering agent behavior
/// (always `Keep` / `Continue`).
#[derive(Debug, Clone, Default)]
struct ToolRecorder {
    events: Arc<Mutex<Vec<ToolEvent>>>,
    reasoning: Arc<Mutex<Vec<String>>>,
}

impl AgentHook for ToolRecorder {
    /// Capture true LLM reasoning text from each model response. Only
    /// `Reasoning` content blocks count; plain text and tool calls are not
    /// reasoning. Never alters behavior (always `Continue`).
    async fn on_completion_response(
        &self,
        _ctx: &HookContext,
        event: CompletionResponseEvent<'_>,
    ) -> ObservationAction {
        let mut texts = Vec::new();
        for content in event.content.iter() {
            if let AssistantContent::Reasoning(r) = content {
                let text = r.display_text();
                if !text.trim().is_empty() {
                    texts.push(text);
                }
            }
        }
        if !texts.is_empty()
            && let Ok(mut guard) = self.reasoning.lock()
        {
            guard.extend(texts);
        }
        ObservationAction::Continue
    }

    async fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        let ok = event.raw_result.is_success() || event.raw_result.is_skipped();
        let detail = event.presentation.render();
        let summary = match ok {
            true => first_line(&detail, 120),
            false => format!("failed: {}", first_line(&detail, 120)),
        };
        let output = truncate_chars(&detail, FULL_OUTPUT_CAP);
        let parsed: serde_json::Value =
            serde_json::from_str(event.args).unwrap_or(serde_json::Value::Null);
        if let Ok(mut guard) = self.events.lock() {
            guard.push(ToolEvent {
                name: event.tool_name.to_string(),
                args: parsed,
                ok,
                summary,
                output,
            });
        }
        ToolResultAction::keep()
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.chars().count() > max {
        true => format!(
            "{}… [truncated]",
            s.chars().take(max - 1).collect::<String>()
        ),
        false => s.to_string(),
    }
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    const ELLIPSIS: char = '…';
    match line.chars().count() > max {
        true => format!(
            "{}…",
            line.chars().take(max - 1).collect::<String>().trim_end()
        ),
        false => line.to_string().replace(ELLIPSIS, "..."),
    }
}

/// Rig-backed agent: OpenAI-compatible `/chat/completions` model + preamble +
/// the 11 file/shell/git/search/web tools, using Rig's built-in multi-step loop (no hand-rolled ReAct).
/// Conversation history is caller-owned in [`Context`]: tool outputs are
/// truncated on entry and old turns are summarized past 100 messages.
pub struct RigAgent {
    agent: rig::agent::Agent,
    context: tokio::sync::Mutex<Context>,
    recorder: ToolRecorder,
    model_name: String,
    effort: String,
}

impl RigAgent {
    pub fn new(
        cfg: &Config,
        approval_tx: ApprovalTx,
        project_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        // Explicit `.base_url()` per spec — no reliance on OPENAI_BASE_URL env.
        let client = openai::CompletionsClient::builder()
            .api_key(cfg.api.key.clone())
            .base_url(cfg.api.base_url.clone())
            .build()
            .map_err(|e| format!("failed to build LLM client: {e}"))?;

        let model = client.completion_model(cfg.model.default.clone());

        let recorder = ToolRecorder::default();
        // Observer first (see everything), gate second (steers). ADR-0001.
        let gate = PermissionHook::new(approval_tx, project_root);

        let agent = AgentBuilder::new(model)
            .preamble(
                "You are rem, a coding agent in a terminal TUI. \
                 Use the read/write/edit/bash/list_directory/git_status/git_diff/grep/glob/web_fetch/web_search tools to inspect and change files. \
                 list_directory lists a dir, glob finds files by pattern, grep searches contents, git_status/git_diff inspect git state. \
                 web_search searches the web (DuckDuckGo, no key), web_fetch reads a URL as text. \
                 Bash runs `sh -c` in the project dir (30s timeout). Destructive shell patterns are blocked outright; other mutations and network access ask the human for approval mid-run — if a call is denied, replan without it. \
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
            .default_max_turns(100)
            .additional_params(serde_json::json!({"reasoning_effort": cfg.model.effort.clone()}))
            .add_hook(recorder.clone())
            .add_hook(gate)
            .build();

        Ok(Self {
            agent,
            context: tokio::sync::Mutex::new(Context::new()),
            recorder,
            model_name: cfg.model.default.clone(),
            effort: cfg.model.effort.clone(),
        })
    }

    pub async fn message_count(&self) -> usize {
        self.context.lock().await.len()
    }

    pub async fn generate_title(&self) -> Result<String, String> {
        self.agent
            .prompt("Give this coding session a short 2-5 word title, reply with title only")
            .preamble("You name coding sessions. Reply with a short title only.")
            .tool_choice(ToolChoice::None)
            .await
            .map_err(|e| e.to_string())
            .map(|s| s.trim().to_string())
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

        // Fresh per-turn tool + reasoning logs: the hooks append as the turn runs.
        if let Ok(mut guard) = self.recorder.events.lock() {
            guard.clear();
        }
        if let Ok(mut guard) = self.recorder.reasoning.lock() {
            guard.clear();
        }

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
                    restored.append(ctx.messages_mut());
                    *ctx.messages_mut() = restored;
                    return Err(format!("context compaction failed, history restored: {e}"));
                }
            }
        }

        Ok(reply)
    }

    fn last_tool_events(&self) -> Vec<ToolEvent> {
        self.recorder
            .events
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    fn last_reasoning(&self) -> Vec<String> {
        self.recorder
            .reasoning
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    fn model_name(&self) -> String {
        self.model_name.clone()
    }

    fn effort_name(&self) -> String {
        self.effort.clone()
    }
    async fn export_messages_json(&self) -> Result<String, String> { self.context.lock().await.to_json() }
    async fn import_messages_json(&self, json: &str) -> Result<(), String> { let ctx = Context::from_json(json)?; *self.context.lock().await = ctx; Ok(()) }
    // Sync variants for the sync TUI submit path via try_lock (safe on runtime thread).
    fn export_sync(&self) -> Result<String, String> { match self.context.try_lock() { Ok(g) => g.to_json(), Err(_) => Err("context busy, try again".to_string()) } }
    fn import_sync(&self, json: &str) -> Result<(), String> { let ctx = Context::from_json(json)?; match self.context.try_lock() { Ok(mut g) => { *g = ctx; Ok(()) }, Err(_) => Err("context busy, try again".to_string()) } }
}
