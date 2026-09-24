use rig::message::{AssistantContent, Message, ToolResultContent, UserContent};

/// Caller-owned conversation history with tool-output truncation and
/// message-count compaction. In-memory only: dies with the process.
///
/// Budget: tool result texts are cut to [`Context::TOOL_BUDGET`] chars on
/// history entry. When the message count exceeds [`Context::TRIGGER`], the
/// oldest messages are summarized by the same model into a single user
/// message at index 0 and only [`Context::KEEP`] newest are retained.
pub struct Context {
    messages: Vec<Message>,
}

impl Context {
    pub const TOOL_BUDGET: usize = 2000;
    pub const TRIGGER: usize = 100;
    pub const KEEP: usize = 40;

    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn messages_mut(&mut self) -> &mut Vec<Message> {
        &mut self.messages
    }

    #[cfg(test)]
    fn messages(&self) -> &Vec<Message> {
        &self.messages
    }

    /// Truncate `ToolResult` text items in `messages[from..]` to TOOL_BUDGET
    /// chars with a `[truncated N chars]` note. Call with the history length
    /// from before the latest turn so only newly committed messages are cut.
    pub fn truncate_new(&mut self, from: usize) {
        for msg in self.messages.iter_mut().skip(from) {
            if let Message::User { content } = msg {
                for item in content.iter_mut() {
                    if let UserContent::ToolResult(result) = item {
                        for c in result.content.iter_mut() {
                            if let ToolResultContent::Text(t) = c
                                && t.text.len() > Self::TOOL_BUDGET
                            {
                                let dropped = t.text.len() - Self::TOOL_BUDGET;
                                t.text.truncate(Self::TOOL_BUDGET);
                                t.text.push_str(&format!(" [truncated {dropped} chars]"));
                            }
                        }
                    }
                }
            }
        }
    }

    /// If over TRIGGER, split off the oldest messages for summarization.
    /// Returns them (caller summarizes, then calls [`Context::apply_summary`]).
    /// Keeps the newest KEEP verbatim. Never compacts twice without new turns:
    /// after compaction `len <= KEEP + 1`, well under TRIGGER.
    pub fn take_for_compaction(&mut self) -> Option<Vec<Message>> {
        if self.messages.len() <= Self::TRIGGER {
            return None;
        }
        let split_at = self.messages.len() - Self::KEEP;
        Some(self.messages.drain(..split_at).collect())
    }

    /// Replace drained messages with a single summary at index 0.
    pub fn apply_summary(&mut self, summary: String) {
        let entry = Message::user(format!("Prior conversation summary: {summary}"));
        self.messages.insert(0, entry);
    }

    /// Render drained messages as compact `role: text` lines for the summarizer.
    pub fn render_for_summary(msgs: &[Message]) -> String {
        msgs.iter()
            .map(|m| match m {
                Message::System { content } => format!("system: {content}"),
                Message::User { content } => {
                    let parts: Vec<String> = content
                        .iter()
                        .map(|c| match c {
                            UserContent::Text(t) => t.text.chars().take(500).collect(),
                            UserContent::ToolResult(r) => {
                                let texts: Vec<&str> =
                                    r.content.iter().filter_map(|c| c.as_text()).collect();
                                let joined = texts.join(" | ");
                                let cut: String = joined.chars().take(500).collect();
                                format!("[{} result] {cut}", r.name)
                            }
                            _ => "[non-text content]".to_string(),
                        })
                        .collect();
                    format!("user: {}", parts.join(" | "))
                }
                Message::Assistant { content, .. } => {
                    let parts: Vec<String> = content
                        .iter()
                        .map(|c| match c {
                            AssistantContent::Text(t) => t.text.chars().take(500).collect(),
                            AssistantContent::ToolCall(tc) => {
                                format!("[tool call: {}]", tc.function.name)
                            }
                            _ => "[non-text content]".to_string(),
                        })
                        .collect();
                    format!("assistant: {}", parts.join(" | "))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::message::ToolResult;

    fn tool_msg(text: &str) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: "test-call".to_string().try_into().unwrap(),
                provider: None,
                name: "read".to_string(),
                content: vec![ToolResultContent::text(text)],
            })],
        }
    }

    #[test]
    fn truncates_long_tool_results_with_note() {
        let mut ctx = Context::new();
        ctx.messages_mut().push(tool_msg(&"x".repeat(2500)));
        ctx.truncate_new(0);
        let Message::User { content } = &ctx.messages()[0] else {
            panic!("expected user msg");
        };
        let UserContent::ToolResult(r) = &content[0] else {
            panic!("expected tool result");
        };
        let text = r.content[0].as_text().unwrap();
        assert!(text.len() <= Context::TOOL_BUDGET + 40);
        assert!(text.contains("[truncated 500 chars]"));
    }

    #[test]
    fn leaves_short_results_alone() {
        let mut ctx = Context::new();
        ctx.messages_mut().push(tool_msg("short"));
        ctx.truncate_new(0);
        let Message::User { content } = &ctx.messages()[0] else {
            panic!("expected user msg");
        };
        let UserContent::ToolResult(r) = &content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(r.content[0].as_text().unwrap(), "short");
    }

    #[test]
    fn only_truncates_from_offset() {
        let mut ctx = Context::new();
        ctx.messages_mut().push(tool_msg(&"y".repeat(2500)));
        ctx.messages_mut().push(Message::user("hello"));
        ctx.truncate_new(1); // skip the long tool message
        let Message::User { content } = &ctx.messages()[0] else {
            panic!("expected user msg");
        };
        let UserContent::ToolResult(r) = &content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(r.content[0].as_text().unwrap().len(), 2500);
    }

    #[test]
    fn compaction_keeps_newest_and_applies_summary() {
        let mut ctx = Context::new();
        for i in 0..Context::TRIGGER + 10 {
            ctx.messages_mut().push(Message::user(format!("msg {i}")));
        }
        let drained = ctx.take_for_compaction().expect("should compact");
        assert_eq!(drained.len(), Context::TRIGGER + 10 - Context::KEEP);
        assert_eq!(ctx.len(), Context::KEEP);
        // newest retained
        let Message::User { content } = &ctx.messages()[Context::KEEP - 1] else {
            panic!("expected user msg");
        };
        assert!(format!("{content:?}").contains("msg 109"));
        ctx.apply_summary("did stuff".to_string());
        assert_eq!(ctx.len(), Context::KEEP + 1);
        let rendered = Context::render_for_summary(&drained);
        assert!(rendered.contains("msg 0"));
    }

    #[test]
    fn no_compaction_under_trigger() {
        let mut ctx = Context::new();
        ctx.messages_mut().push(Message::user("hi"));
        assert!(ctx.take_for_compaction().is_none());
    }
}
