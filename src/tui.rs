use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
        KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Terminal,
};
use unicode_width::UnicodeWidthStr;
use tokio::task::JoinHandle;

use crate::agent::{AgentLoop, ToolEvent};
use crate::permissions::{ApprovalDecision, ApprovalRequest, ApprovalRx};

/// Swappable UI abstraction. `RatatuiBackend` is the Ratatui implementation;
/// the old Cursive backend was removed in favor of this Aster-styled UI.
pub trait TuiBackend {
    fn run(
        self,
        agent: impl AgentLoop + Send + Sync + 'static,
        approval_rx: ApprovalRx,
    ) -> anyhow::Result<()>;
}

pub struct RatatuiBackend;

impl RatatuiBackend {
    pub fn new() -> Self {
        Self
    }
}

impl TuiBackend for RatatuiBackend {
    fn run(
        self,
        agent: impl AgentLoop + Send + Sync + 'static,
        approval_rx: ApprovalRx,
    ) -> anyhow::Result<()> {
        run_app(agent, approval_rx)
    }
}

/// Outcome of one completed turn, sent from the worker task to the UI loop.
struct TurnResult {
    result: Result<String, String>,
    events: Vec<ToolEvent>,
    /// True LLM reasoning texts captured this turn (may be empty).
    reasoning: Vec<String>,
    /// Generation tag from `App::turn_generation` at submit time (D2).
    seq: u64,
}

/// One live tool observation pushed from the hook while a turn is running.
struct ThinkMsg {
    name: String,
    preview: String,
    ok: bool,
    summary: String,
}

fn run_app(
    agent: impl AgentLoop + Send + Sync + 'static,
    approval_rx: ApprovalRx,
) -> anyhow::Result<()> {
    let model = agent.model_name();
    let agent = Arc::new(agent);
    let (tx, rx) = mpsc::channel::<TurnResult>();
    let (think_tx, think_rx) = mpsc::channel::<ThinkMsg>();

    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let mut app = App::new(model);
    // The approval channel is tokio mpsc; the TUI loop is sync crossterm, so
    // poll with try_recv (never block the 50ms frame).
    let mut approval_rx = approval_rx;
    let outcome = event_loop(
        &mut terminal,
        &mut app,
        agent,
        &rx,
        tx,
        &think_rx,
        think_tx,
        &mut approval_rx,
    );

    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();
    outcome
}

/// Stable id for one message block. Index into `App.blocks`.
type BlockId = usize;

/// Collapsed output budget: a tool/thinking body taller than this renders
/// truncated with an expand hint until toggled open.
const COLLAPSED_LINES: usize = 10;

/// Cap on stored blocks; oldest blocks drop off the top like the old
/// 5000-line transcript cap.
const MAX_BLOCKS: usize = 1000;

/// Braille spinner frames for the busy status row (same set Aster uses).
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Shaded input band background (Aster `pane_bg`).
const PANE_BG: Color = Color::Rgb(0x19, 0x19, 0x19);
/// Warm orange accent for the prompt, spinner, and mode glyph.
const ACCENT: Color = Color::Rgb(242, 118, 79);
/// Faint placeholder gray (Aster `placeholder`).
const PLACEHOLDER: Color = Color::Rgb(0x4d, 0x4d, 0x4d);

/// One selectable row in the messages area. Claude Code style: the user
/// prompt, the assistant reply, one block per tool call (name + key args +
/// status + output), and one collapsed-by-default thinking row per turn that
/// produced true LLM reasoning.
#[derive(Debug, Clone)]
enum MessageBlock {
    User {
        text: String,
    },
    Reply {
        text: String,
    },
    ToolCall {
        name: String,
        args_preview: String,
        ok: bool,
        summary: String,
        output: String,
        expanded: bool,
    },
    Thinking {
        text: String,
        expanded: bool,
    },
    System {
        text: String,
    },
    /// In-progress tool feed while a turn runs: a live row per tool that has
    /// resolved so far, replaced by final blocks when the turn completes.
    LiveTools {
        tools: Vec<LiveTool>,
    },
    Error {
        text: String,
    },
    /// `Done (N tools)` trailer after a successful tool turn.
    Trailer {
        text: String,
    },
}

impl MessageBlock {
    /// Whether this block can be expanded/collapsed.
    fn expandable(&self) -> bool {
        match self {
            MessageBlock::ToolCall { output, .. } => {
                output.lines().count() + 1 > COLLAPSED_LINES
            }
            MessageBlock::Thinking { text, .. } => {
                text.lines().count() > COLLAPSED_LINES
            }
            _ => false,
        }
    }

    fn is_expanded(&self) -> bool {
        match self {
            MessageBlock::ToolCall { expanded, .. } => *expanded,
            MessageBlock::Thinking { expanded, .. } => *expanded,
            _ => false,
        }
    }

    fn toggle(&mut self) {
        match self {
            MessageBlock::ToolCall { expanded, .. } => *expanded = !*expanded,
            MessageBlock::Thinking { expanded, .. } => *expanded = !*expanded,
            _ => {}
        }
    }
}

struct App {
    model: String,
    cwd: String,
    /// Selectable block list: the messages area. Replaces `transcript`.
    blocks: Vec<MessageBlock>,
    input: String,
    cursor: usize, // char index into `input`
    history: Vec<String>,
    hist_idx: Option<usize>,
    busy: bool,
    busy_since: Instant,
    status: Status,
    last_prompt: Option<String>,
    turns: usize,
    pinned: bool,
    scroll: u16,
    view_h: u16,
    /// Wrapped line count from the last render; anchors scrollback.
    rendered_total: usize,
    dirty: bool,
    /// Selected block for keyboard expand/collapse. `None` = input focus
    /// (Enter submits, Up/Down = history). `Some` = block focus (Enter/Space
    /// toggles, Up/Down moves selection, Esc clears).
    selected: Option<BlockId>,
    /// Pending human approvals (FIFO). Head renders as a blocking modal;
    /// resolving it resumes the parked agent worker in the same run.
    pending_approvals: VecDeque<ApprovalRequest>,
    /// In-flight turn worker + live-feed watcher (Task 1 / ADR-0005).
    /// D1: `tokio::spawn` tasks are independent — aborting the outer chat
    /// task does NOT stop a nested watcher, so `submit()` spawns the 80ms
    /// watcher as a sibling and both handles are stored here for `abort_turn`.
    current_turn: Option<JoinHandle<()>>,
    current_watcher: Option<JoinHandle<()>>,
    /// Turn generation guard (D2): bumped on every `submit` and every
    /// `abort_turn`; the `event_loop` drain drops any `TurnResult` whose
    /// `seq` no longer matches (send-then-abort race).
    turn_generation: u64,
}

/// Resolve a queued approval: send the decision over the oneshot and record
/// an audit block. A dropped/closed oneshot means the worker already moved
/// on; the request is simply forgotten.
fn resolve_approval(app: &mut App, decision: ApprovalDecision) {
    let Some(req) = app.pending_approvals.pop_front() else {
        return;
    };
    let label = match decision {
        ApprovalDecision::Approve => "approved",
        ApprovalDecision::ApproveAlways => "approved (always this session)",
        ApprovalDecision::Deny => "denied",
        ApprovalDecision::AbortTurn => "aborted turn",
    };
    let _ = req.reply.send(decision);
    app.blocks.push(MessageBlock::System {
        text: format!(
            "permission {}: {}({})",
            label, req.tool_name, req.args_preview
        ),
    });
    app.cap_blocks();
    app.pinned = true;
    app.dirty = true;
}

/// Abort the in-flight turn worker + live-feed watcher (Task 1 / ADR-0005).
/// Keeps partial output: the live feed row becomes final `ToolCall` blocks,
/// then a system `interrupted.` marker + `Interrupted (N tools)` trailer.
/// D1: the watcher is a sibling task (see `submit`), so both stored handles
/// are aborted explicitly — aborting the outer task alone would orphan it.
/// D2: bumps `turn_generation` so a `TurnResult` that wins the
/// send-then-abort race is dropped by the `event_loop` drain (`seq` mismatch).
fn abort_turn(app: &mut App) {
    if !app.busy {
        return;
    }
    if let Some(handle) = app.current_turn.take() {
        handle.abort();
    }
    if let Some(handle) = app.current_watcher.take() {
        handle.abort();
    }
    app.turn_generation += 1;
    app.busy = false;
    app.turns += 1;
    app.status = Status::Ready;
    // Keep partial tools: convert the live feed row into final blocks.
    let partial: Vec<LiveTool> = match app.blocks.pop() {
        Some(MessageBlock::LiveTools { tools }) => tools,
        Some(other) => {
            app.blocks.push(other);
            Vec::new()
        }
        None => Vec::new(),
    };
    let n = partial.len();
    for tool in partial {
        app.blocks.push(MessageBlock::ToolCall {
            name: tool.name,
            args_preview: tool.preview,
            ok: tool.ok,
            summary: tool.summary,
            output: String::new(),
            expanded: false,
        });
    }
    if n > 0 {
        app.blocks.push(MessageBlock::System {
            text: String::new(),
        });
    }
    app.blocks.push(MessageBlock::System {
        text: "interrupted.".to_string(),
    });
    if n > 0 {
        app.blocks.push(MessageBlock::Trailer {
            text: format!("Interrupted ({} tool{})", n, if n == 1 { "" } else { "s" }),
        });
    }
    app.cap_blocks();
    app.selected = None;
    app.pinned = true;
    app.dirty = true;
}

/// One tool observation streamed live during a busy turn.
#[derive(Debug, Clone)]
struct LiveTool {
    name: String,
    preview: String,
    ok: bool,
    summary: String,
}

#[derive(Clone)]
enum Status {
    Ready,
    Error,
}

impl App {
    fn new(model: String) -> Self {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string());
        let cwd = short_home(&cwd);
        let mut app = Self {
            model,
            cwd,
            blocks: Vec::new(),
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            hist_idx: None,
            busy: false,
            busy_since: Instant::now(),
            status: Status::Ready,
            last_prompt: None,
            turns: 0,
            pinned: true,
            scroll: 0,
            view_h: 10,
            rendered_total: 0,
            dirty: true,
            selected: None,
            pending_approvals: VecDeque::new(),
            current_turn: None,
            current_watcher: None,
            turn_generation: 0,
        };
        app.push_system("rem — esc interrupt · ^D quit when empty · ^C clear · /quit quit · /clear clear · tab selects blocks.");
        app
    }

    fn push_user(&mut self, prompt: &str) {
        if !self.blocks.is_empty() {
            self.blocks.push(MessageBlock::System {
                text: String::new(),
            });
        }
        self.blocks.push(MessageBlock::User {
            text: prompt.to_string(),
        });
    }

    /// Replay one completed turn as inline blocks: one collapsed-by-default
    /// thinking row (only when the provider emitted true reasoning), one
    /// tool block per event, then the reply. Removes the live feed row.
    fn push_turn(
        &mut self,
        events: &[ToolEvent],
        reasoning: &[String],
        result: &Result<String, String>,
    ) {
        self.remove_live_row();
        for text in reasoning {
            if !text.trim().is_empty() {
                self.blocks.push(MessageBlock::Thinking {
                    text: text.clone(),
                    expanded: false,
                });
            }
        }
        for ev in events {
            self.blocks.push(MessageBlock::ToolCall {
                name: ev.name.clone(),
                args_preview: ev.arg_preview(),
                ok: ev.ok,
                summary: ev.summary.clone(),
                output: ev.output.clone(),
                expanded: false,
            });
        }
        if !events.is_empty() {
            self.blocks.push(MessageBlock::System {
                text: String::new(),
            });
        }
        match result {
            Ok(text) => self.push_reply(text),
            Err(e) => {
                self.blocks.push(MessageBlock::Error {
                    text: format!("[error] {e}"),
                });
                self.status = Status::Error;
            }
        }
        if !events.is_empty() && result.is_ok() {
            let n = events.len();
            self.blocks.push(MessageBlock::Trailer {
                text: format!("Done ({} tool{})", n, if n == 1 { "" } else { "s" }),
            });
        }
        self.cap_blocks();
        self.selected = None;
        self.pinned = true;
        self.dirty = true;
    }

    /// Live row maintenance while a turn runs: append or update the trailing
    /// `LiveTools` row so in-progress tools stream inline in the messages
    /// area. Created on demand by the first live tool message.
    fn push_live_tool(&mut self, msg: ThinkMsg) {
        let tool = LiveTool {
            name: msg.name,
            preview: msg.preview,
            ok: msg.ok,
            summary: msg.summary,
        };
        match self.blocks.last_mut() {
            Some(MessageBlock::LiveTools { tools }) => tools.push(tool),
            _ => self.blocks.push(MessageBlock::LiveTools { tools: vec![tool] }),
        }
        self.cap_blocks();
        self.pinned = true;
        self.dirty = true;
    }

    fn remove_live_row(&mut self) {
        if matches!(
            self.blocks.last(),
            Some(MessageBlock::LiveTools { .. })
        ) {
            self.blocks.pop();
        }
    }

    fn push_reply(&mut self, text: &str) {
        match text.trim().is_empty() {
            true => self.blocks.push(MessageBlock::Reply {
                text: "(empty reply)".to_string(),
            }),
            false => self.blocks.push(MessageBlock::Reply {
                text: text.to_string(),
            }),
        }
    }

    fn push_system(&mut self, msg: &str) {
        self.blocks.push(MessageBlock::System {
            text: msg.to_string(),
        });
        self.dirty = true;
    }

    fn cap_blocks(&mut self) {
        if self.blocks.len() > MAX_BLOCKS {
            let drop = self.blocks.len() - MAX_BLOCKS;
            self.blocks.drain(..drop);
            // Dropped indices shift: clamp selection into range.
            self.selected = self.selected.and_then(|s| s.checked_sub(drop));
        }
    }
}

/// Lightweight markdown/diff tint for assistant replies, Aster-style:
/// green additions, red deletions, dim fences, bold headers.
fn reply_style(line: &str) -> Style {
    if line.starts_with("```") {
        Style::default().fg(Color::DarkGray)
    } else if line.starts_with('+') && !line.starts_with("++") {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::default().fg(Color::Red)
    } else if line.starts_with('#') {
        Style::default().add_modifier(Modifier::BOLD)
    } else if line.starts_with('>') {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    }
}

fn short_home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => match path.strip_prefix(&home) {
            Some(rest) => format!("~{rest}"),
            None => path.to_string(),
        },
        _ => path.to_string(),
    }
}

/// Main loop: drains turn/live/approval channels, renders, routes input.
/// Eight args is one over the default lint: the five channels plus terminal,
/// app, and agent are each a distinct pipe and bundling them would obscure
/// the drain order below.
#[allow(clippy::too_many_arguments)]
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    agent: Arc<impl AgentLoop + Send + Sync + 'static>,
    rx: &mpsc::Receiver<TurnResult>,
    tx: mpsc::Sender<TurnResult>,
    think_rx: &mpsc::Receiver<ThinkMsg>,
    think_tx: mpsc::Sender<ThinkMsg>,
    approval_rx: &mut ApprovalRx,
) -> anyhow::Result<()> {
    loop {
        // Drain live tool observations: each resolved tool appends to the
        // trailing live row inline in the messages area. Gated on `busy`:
        // a ThinkMsg already in flight when the watcher aborts must not
        // resurrect a live row after `abort_turn` synthesized the trailer.
        while let Ok(msg) = think_rx.try_recv() {
            if app.busy {
                app.push_live_tool(msg);
            }
        }
        // Drain approval requests into the modal queue. The agent worker is
        // parked on its oneshot; the same run resumes on resolve.
        while let Ok(req) = approval_rx.try_recv() {
            app.pending_approvals.push_back(req);
            app.dirty = true;
        }
        // Drain completed turns without blocking the UI.
        // D2: a `tx.send` that wins the send-then-abort race arrives with a
        // stale `seq`; drop it so an aborted turn never replays as complete.
        while let Ok(turn) = rx.try_recv() {
            if turn.seq != app.turn_generation {
                continue;
            }
            app.current_turn.take();
            if let Some(watcher) = app.current_watcher.take() {
                watcher.abort();
            }
            app.busy = false;
            app.turns += 1;
            if turn.result.is_ok() {
                app.status = Status::Ready;
            }
            app.push_turn(&turn.events, &turn.reasoning, &turn.result);
        }

        if app.dirty || app.busy || !app.pending_approvals.is_empty() {
            terminal
                .draw(|f| render(f, app))
                .context("draw frame")?;
            // Place the hardware cursor on the middle row of the input band.
            // Layout: header(1) body(?) gap(1) status(0/1) input(3) footer(1),
            // so the text line is always the third row from the bottom.
            let area = terminal.size().unwrap_or_default();
            let x = app.cursor_x(area.width);
            let y = area.height.saturating_sub(3);
            terminal
                .set_cursor_position(ratatui::layout::Position::new(x, y))
                .ok();
            terminal.show_cursor().ok();
            app.dirty = false;
        }

        if event::poll(Duration::from_millis(50)).context("poll events")? {
            match event::read().context("read event")? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if handle_key(app, &agent, &tx, &think_tx, key.code, key.modifiers) {
                        return Ok(());
                    }
                    app.dirty = true;
                }
                Event::Mouse(mouse) => {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        // Clicks land in the body region: rows 1..1+view_h
                        // (header row 0, gap + status + input + footer below).
                        let area = terminal.size().unwrap_or_default();
                        let body_h = app.view_h;
                        let body_top: u16 = 1;
                        if mouse.row >= body_top
                            && mouse.row < body_top.saturating_add(body_h)
                            && app.click_toggle(mouse.row - body_top, area.width)
                        {
                            app.dirty = true;
                        }
                    }
                }
                Event::Resize(_, _) => {
                    app.dirty = true;
                }
                _ => {}
            }
        }
    }
}

/// Returns true when the app should quit.
fn handle_key(
    app: &mut App,
    agent: &Arc<impl AgentLoop + Send + Sync + 'static>,
    tx: &mpsc::Sender<TurnResult>,
    think_tx: &mpsc::Sender<ThinkMsg>,
    code: KeyCode,
    mods: KeyModifiers,
) -> bool {
    // Approval modal owns every keystroke while pending: y/a/n/x/Esc
    // (+Enter as approve). Esc resolves AbortTurn (same as `x`) — never
    // silently dismisses (ADR-0003 Q7); the parked worker gets a decision.
    if !app.pending_approvals.is_empty() {
        // Ctrl combos are ignored in the modal (no input line to clear) —
        // except Ctrl+D, which quits via `handle_ctrl` (empty input only;
        // the `d` arm resolves AbortTurn before quitting).
        if mods.contains(KeyModifiers::CONTROL) {
            if code == KeyCode::Char('d') {
                return handle_ctrl(app, code);
            }
            return false;
        }
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                resolve_approval(app, ApprovalDecision::Approve);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                resolve_approval(app, ApprovalDecision::ApproveAlways);
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                resolve_approval(app, ApprovalDecision::Deny);
            }
            KeyCode::Char('x') | KeyCode::Char('X') => {
                resolve_approval(app, ApprovalDecision::AbortTurn);
            }
            KeyCode::Esc => {
                // Task 2 / ADR-0005: modal Esc is AbortTurn, same path
                // as `x` (→ ToolCallAction::stop); never silently dismissed.
                resolve_approval(app, ApprovalDecision::AbortTurn);
            }
            _ => {}
        }
        return false;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        return handle_ctrl(app, code);
    }
    // Esc matrix (Task 2 / ADR-0005): busy-first so selected+busy
    // interrupts; selected+idle deselects; idle is a no-op. No Esc
    // sequence quits (double-Esc quit removed).
    if code == KeyCode::Esc {
        if app.busy {
            abort_turn(app);
            return false;
        }
        if app.selected.is_some() {
            app.selected = None;
            app.dirty = true;
        }
        return false;
    }
    // Block focus: selection commands win over input editing.
    if app.selected.is_some() {
        return handle_selected_key(app, code);
    }
    match code {
        KeyCode::Enter => return submit(app, agent, tx, think_tx),
        // Tab enters block selection on the newest expandable block.
        // Crossterm reports Shift+Tab as BackTab; both handled here since
        // input focus has no BackTab branch to conflict with.
        KeyCode::Tab | KeyCode::BackTab => {
            app.select_newest(mods.contains(KeyModifiers::SHIFT));
        }
        KeyCode::Backspace => {
            if app.cursor > 0 {
                app.cursor -= 1;
                remove_char_at(&mut app.input, app.cursor);
            }
        }
        KeyCode::Delete => {
            remove_char_at(&mut app.input, app.cursor);
        }
        KeyCode::Left => {
            app.cursor = app.cursor.saturating_sub(1);
        }
        KeyCode::Right => {
            app.cursor = (app.cursor + 1).min(app.input.chars().count());
        }
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => {
            // End snaps the transcript back to live and moves cursor to EOL.
            app.cursor = app.input.chars().count();
            app.pinned = true;
            app.scroll = 0;
        }
        KeyCode::Up => {
            if mods.contains(KeyModifiers::SHIFT) {
                app.pinned = false;
                app.scroll = app.scroll.saturating_sub(1);
            } else {
                recall_history(app, true);
            }
        }
        KeyCode::Down => {
            if mods.contains(KeyModifiers::SHIFT) {
                app.pinned = false;
                app.scroll = app.scroll.saturating_add(1);
            } else {
                recall_history(app, false);
            }
        }
        KeyCode::PageUp => {
            app.pinned = false;
            app.scroll = app.scroll.saturating_sub(app.view_h.max(1));
        }
        KeyCode::PageDown => {
            app.pinned = false;
            let max = app.max_scroll_offset();
            app.scroll = app.scroll.saturating_add(app.view_h.max(1)).min(max);
        }
        KeyCode::Char(c) => {
            insert_char_at(&mut app.input, &mut app.cursor, c);
        }
        _ => {}
    }
    false
}

/// Keys while a block is selected. Enter/Space toggles, Up/Down (or
/// Tab/Shift+Tab) moves selection, Esc deselects (or aborts when busy —
/// busy wins). Never quits.
/// Typing a printable char drops selection and inserts into input so no
/// keystroke is lost.
fn handle_selected_key(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Enter | KeyCode::Char(' ') => {
            app.toggle_selected();
            false
        }
        KeyCode::Up => {
            app.move_selection(true);
            false
        }
        KeyCode::Down => {
            app.move_selection(false);
            false
        }
        KeyCode::Tab => {
            app.move_selection(false);
            false
        }
        KeyCode::BackTab => {
            app.move_selection(true);
            false
        }
        KeyCode::Esc => {
            // Task 2 / ADR-0005: busy wins over selection; idle Esc
            // only deselects. Never quits.
            if app.busy {
                abort_turn(app);
                return false;
            }
            app.selected = None;
            app.dirty = true;
            false
        }
        KeyCode::Char(c) => {
            // Drop selection, keep the keystroke: focus returns to input.
            app.selected = None;
            insert_char_at(&mut app.input, &mut app.cursor, c);
            app.dirty = true;
            false
        }
        KeyCode::Backspace => {
            app.selected = None;
            if app.cursor > 0 {
                app.cursor -= 1;
                remove_char_at(&mut app.input, app.cursor);
            }
            app.dirty = true;
            false
        }
        _ => false,
    }
}

fn handle_ctrl(app: &mut App, code: KeyCode) -> bool {
    match code {
        // Task 4 / ADR-0005: Ctrl+C is strict clear-only — clears the
        // input line, never interrupts, never quits (quit is Ctrl+D-on-empty
        // or /quit). Ignored in the modal via the CONTROL early-return in
        // `handle_key` above.
        KeyCode::Char('c') => {
            app.input.clear();
            app.cursor = 0;
            false
        }
        KeyCode::Char('u') => {
            app.input.clear();
            app.cursor = 0;
            false
        }
        KeyCode::Char('w') => {
            delete_word_before(&mut app.input, &mut app.cursor);
            false
        }
        KeyCode::Char('d') => {
            // Task 3 / ADR-0005 (D3): Ctrl+D quits only when the input
            // line is empty — non-empty is a no-op (Unix convention, never
            // deletes). Empty + modal: resolve the head approval as
            // AbortTurn (same path as `x`/Esc → ToolCallAction::stop) so
            // the parked worker gets a decision via the existing Stop
            // path, then quit. Empty + busy: `abort_turn` synthesizes the
            // interrupted trailer (and bumps the generation so a late
            // TurnResult is stale-dropped), then quit. Empty + idle:
            // quit directly.
            // /quit parity (flagged, not matched): `/quit` pushes its
            // line to history before quitting, but Ctrl+D only fires on
            // empty input so there is nothing to push; neither path needs
            // to clear `selected` since the process exits.
            if !app.input.is_empty() {
                false
            } else {
                if !app.pending_approvals.is_empty() {
                    resolve_approval(app, ApprovalDecision::AbortTurn);
                }
                if app.busy {
                    abort_turn(app);
                }
                true
            }
        }
        _ => false,
    }
}

fn submit(
    app: &mut App,
    agent: &Arc<impl AgentLoop + Send + Sync + 'static>,
    tx: &mpsc::Sender<TurnResult>,
    think_tx: &mpsc::Sender<ThinkMsg>,
) -> bool {
    let text = app.input.trim().to_string();
    if text.is_empty() || app.busy {
        return false;
    }
    app.input.clear();
    app.cursor = 0;
    app.history.push(text.clone());
    app.hist_idx = None;

    if text == "/quit" {
        return true;
    }
    if text == "/clear" {
        app.blocks.clear();
        app.push_system("cleared.");
        app.selected = None;
        app.pinned = true;
        app.scroll = 0;
        return false;
    }
    if text.starts_with('/') {
        app.push_system(&format!(
            "unknown command \"{text}\". Try /clear or /quit."
        ));
        return false;
    }

    app.push_user(&text);
    app.last_prompt = Some(text.clone());
    app.busy = true;
    app.busy_since = Instant::now();
    app.status = Status::Ready;
    app.pinned = true;
    app.scroll = 0;
    app.selected = None;
    // Fresh live feed for this turn; worker pushes resolved tools into it.
    app.remove_live_row();

    let agent = Arc::clone(agent);
    let tx = tx.clone();
    let think_tx = think_tx.clone();
    // Snapshot how many tools the recorder already holds so the watcher only
    // forwards tools from THIS turn.
    let seen = agent.last_tool_events().len();
    app.turn_generation += 1;
    let seq = app.turn_generation;
    // D1: the 80ms live-feed watcher is a SIBLING of the chat task, not a
    // child — `JoinHandle::abort` on the outer task does not propagate to
    // nested tasks, so both handles are stored on `App` and `abort_turn`
    // aborts each explicitly.
    let watch_agent = Arc::clone(&agent);
    let watcher_handle = tokio::spawn(async move {
        // Poll the recorder and forward new tools live. Chat is blocking, so
        // this is the live feed without switching to streaming.
        let mut forwarded = seen;
        loop {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let events = watch_agent.last_tool_events();
            if events.len() <= forwarded {
                continue;
            }
            for ev in events.iter().skip(forwarded) {
                let _ = think_tx.send(ThinkMsg {
                    name: ev.name.clone(),
                    preview: ev.arg_preview(),
                    ok: ev.ok,
                    summary: ev.summary.clone(),
                });
            }
            forwarded = events.len();
        }
    });
    let watcher_abort = watcher_handle.abort_handle();
    app.current_watcher = Some(watcher_handle);
    app.current_turn = Some(tokio::spawn(async move {
        let result = agent.chat(&text).await;
        watcher_abort.abort();
        // Forward anything the 80ms poll missed between last probe and return.
        let events = agent.last_tool_events();
        let reasoning = agent.last_reasoning();
        let _ = tx.send(TurnResult {
            result,
            events,
            reasoning,
            seq,
        });
    }));
    false
}

fn recall_history(app: &mut App, older: bool) {
    if app.history.is_empty() {
        return;
    }
    let next = match (app.hist_idx, older) {
        (None, true) => Some(app.history.len() - 1),
        (None, false) => None,
        (Some(i), true) => Some(i.saturating_sub(1)),
        (Some(i), false) => {
            if i + 1 >= app.history.len() {
                None
            } else {
                Some(i + 1)
            }
        }
    };
    app.hist_idx = next;
    app.input = next.map(|i| app.history[i].clone()).unwrap_or_default();
    app.cursor = app.input.chars().count();
}

fn byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn insert_char_at(input: &mut String, cursor: &mut usize, c: char) {
    input.insert(byte_idx(input, *cursor), c);
    *cursor += 1;
}

fn remove_char_at(input: &mut String, char_idx: usize) {
    let chars: Vec<char> = input.chars().collect();
    if char_idx < chars.len() {
        let b = byte_idx(input, char_idx);
        let e = byte_idx(input, char_idx + 1);
        input.drain(b..e);
    }
}

fn delete_word_before(input: &mut String, cursor: &mut usize) {
    let chars: Vec<char> = input.chars().collect();
    let mut i = *cursor;
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    let b = byte_idx(input, i);
    let e = byte_idx(input, *cursor);
    input.drain(b..e);
    *cursor = i;
}

impl App {
    /// Toggle the selected block's expanded state. No-op for non-expandable.
    fn toggle_selected(&mut self) {
        if let Some(id) = self.selected
            && let Some(block) = self.blocks.get_mut(id)
        {
            block.toggle();
        }
        self.dirty = true;
    }

    /// Move selection to the next/previous expandable block, wrapping around.
    /// Falls back to any block when none is expandable.
    fn move_selection(&mut self, up: bool) {
        if self.blocks.is_empty() {
            return;
        }
        let n = self.blocks.len();
        let start = self.selected.unwrap_or(match up {
            true => 0,
            false => n - 1,
        });
        // Prefer expandable blocks; accept any block as fallback.
        for step in 1..=n {
            let idx = match up {
                true => (start + n - step) % n,
                false => (start + step) % n,
            };
            if self.blocks[idx].expandable() {
                self.selected = Some(idx);
                self.ensure_selected_visible();
                self.dirty = true;
                return;
            }
        }
        let idx = match up {
            true => (start + n - 1) % n,
            false => (start + 1) % n,
        };
        self.selected = Some(idx);
        self.ensure_selected_visible();
        self.dirty = true;
    }

    /// Select the newest expandable block (`any` = newest block of any kind).
    fn select_newest(&mut self, any: bool) {
        let found = self.blocks.iter().rposition(|b| any || b.expandable());
        self.selected = found;
        self.ensure_selected_visible();
        self.dirty = true;
    }

    /// Unpin and scroll just enough to show the selected block's first line.
    fn ensure_selected_visible(&mut self) {
        let Some(id) = self.selected else { return };
        let width = 80usize; // refined on next render; view width unknown here
        let mut start = 0usize;
        for (i, block) in self.blocks.iter().enumerate() {
            if i == id {
                break;
            }
            start += block_height(block, width);
        }
        let view = self.view_h.max(1) as usize;
        let bottom = self.rendered_total.saturating_sub(view);
        let cur = match self.pinned {
            true => bottom,
            false => self.scroll as usize,
        };
        if start < cur {
            self.pinned = false;
            self.scroll = start.min(bottom) as u16;
        } else if start >= cur + view {
            self.pinned = false;
            self.scroll = start.saturating_sub(view.saturating_sub(1)).min(bottom) as u16;
        }
    }

    /// Click at `row` lines below the body top: map to a block and toggle it.
    /// Returns true when a block was toggled. Uses wrapped heights so the
    /// mapping agrees with what the `Paragraph` actually rendered.
    fn click_toggle(&mut self, row: u16, width: u16) -> bool {
        let scroll = match self.pinned {
            true => self.rendered_total.saturating_sub(self.view_h as usize) as u16,
            false => self.scroll,
        };
        let target = scroll.saturating_add(row) as usize;
        let w = (width as usize).max(1);
        // Flatten like render_body, then walk wrapped line counts.
        let mut flat: Vec<Line<'static>> = Vec::new();
        let mut starts: Vec<usize> = Vec::with_capacity(self.blocks.len());
        for block in self.blocks.iter() {
            starts.push(flat.len());
            render_block(block, false, &mut flat);
        }
        // Wrapped offset of each flattened line: prefix sums over div_ceil.
        let mut block_of_line: Vec<usize> = Vec::with_capacity(flat.len());
        for (id, _) in self.blocks.iter().enumerate() {
            let end = match id + 1 < starts.len() {
                true => starts[id + 1],
                false => flat.len(),
            };
            for line in &flat[starts[id]..end] {
                let line_w: usize = line
                    .spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                let h = line_w.div_ceil(w).max(1);
                for _ in 0..h {
                    block_of_line.push(id);
                }
            }
        }
        if target < block_of_line.len() {
            let id = block_of_line[target];
            self.selected = Some(id);
            if let Some(b) = self.blocks.get_mut(id) {
                b.toggle();
            }
            return true;
        }
        false
    }

    /// Max scroll offset in wrapped lines (for PageDown clamping).
    fn max_scroll_offset(&self) -> u16 {
        self.rendered_total
            .saturating_sub(self.view_h as usize) as u16
    }
}

impl App {
    /// Terminal x-coordinate of the cursor within the input line.
    /// Shares [`visible_window`] with the renderer so the hardware caret
    /// always sits on the displayed caret column, even mid-line in overflow.
    /// The text line sits inside the shaded band with a 1-column inset plus
    /// the 2-column `❯ ` prompt, hence the +3 and the -4 width budget.
    fn cursor_x(&self, term_width: u16) -> u16 {
        let max_w = (term_width as usize).saturating_sub(4);
        let (_, caret) = visible_window(&self.input, self.cursor, max_w);
        3u16.saturating_add(caret as u16)
    }
}

/// Visible slice of a single-line input plus the caret column within it.
/// Left-trims by display columns so the caret stays on screen: tail-anchored
/// while typing at the end, following the caret when it moves left.
fn visible_window(input: &str, cursor: usize, max_w: usize) -> (String, usize) {
    if max_w == 0 {
        return (String::new(), 0);
    }
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());
    let widths: Vec<usize> = chars
        .iter()
        .map(|c| UnicodeWidthStr::width(c.to_string().as_str()))
        .collect();
    let caret_col: usize = widths[..cursor].iter().sum();
    let keep = max_w.saturating_sub(1).max(1);
    let skip_col = caret_col.saturating_sub(keep);
    let mut consumed = 0usize;
    let mut start = 0usize;
    while start < chars.len() && consumed + widths[start] <= skip_col {
        consumed += widths[start];
        start += 1;
    }
    let mut visible = String::new();
    let mut shown = 0usize;
    for (i, c) in chars.iter().enumerate().skip(start) {
        if shown + widths[i] > max_w {
            break;
        }
        visible.push(*c);
        shown += widths[i];
    }
    (visible, caret_col.saturating_sub(consumed))
}

fn render(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    // Layout: header(1) / body / gap(1) / status(0 when idle, 1 when busy)
    // / input band(3) / footer(1). Aster-style: the transcript owns every
    // row above the gap, and the composer is a shaded band with padding.
    let status_h = match app.busy || !app.pending_approvals.is_empty() {
        true => 1,
        false => 0,
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),        // header
            Constraint::Min(0),           // blocks
            Constraint::Length(1),        // gap (terminal bg)
            Constraint::Length(status_h), // busy/approval status
            Constraint::Length(3),        // shaded input band
            Constraint::Length(1),        // footer
        ])
        .split(area);
    app.view_h = chunks[1].height;

    render_header(f, app, chunks[0]);
    render_body(f, app, chunks[1]);
    render_gap(f, chunks[2]);
    render_status(f, app, chunks[3]);
    render_input(f, app, chunks[4]);
    render_footer(f, app, chunks[5]);
    // Approval modal last: centered overlay over the whole frame.
    if let Some(req) = app.pending_approvals.front() {
        render_approval_modal(f, f.area(), req, app.pending_approvals.len());
    }
}

/// Centered approval modal for the head pending request. Blocks the frame
/// visually; `handle_key` blocks input routing until resolved.
fn render_approval_modal(f: &mut ratatui::Frame, area: Rect, req: &ApprovalRequest, queued: usize) {
    let w = (area.width.saturating_sub(8)).clamp(40, 76);
    let h = 11u16;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let modal = Rect::new(x, y, w, h);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .title(format!(
            " permission — approval needed{} ",
            match queued > 1 {
                true => format!(" (1 of {queued})"),
                false => String::new(),
            }
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(modal);
    f.render_widget(block, modal);
    let arg_line = match req.args_preview.chars().count() > inner.width as usize {
        true => format!(
            "{}…",
            req.args_preview.chars().take(inner.width as usize - 1).collect::<String>()
        ),
        false => req.args_preview.clone(),
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", req.tool_name),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("({arg_line})")),
        ]),
        Line::from(vec![Span::styled(
            req.reason.clone(),
            Style::default().fg(Color::DarkGray),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("[y] approve  ", Style::default().fg(Color::Green)),
            Span::styled("[a] always this session  ", Style::default().fg(Color::Green)),
            Span::styled("[n] deny  ", Style::default().fg(Color::Red)),
            Span::styled("[x] abort turn", Style::default().fg(Color::Red)),
        ]),
        Line::from(vec![Span::styled(
            "deny returns feedback so the model replans in the same run.",
            Style::default().fg(Color::DarkGray),
        )]),
    ];
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner,
    );
}

/// Messages area: flatten every block to styled lines, then scroll like the
/// old transcript `Paragraph` (pin-to-bottom or manual offset).
fn render_body(f: &mut ratatui::Frame, app: &mut App, area: ratatui::layout::Rect) {
    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (id, block) in app.blocks.iter().enumerate() {
        let selected = app.selected == Some(id);
        render_block(block, selected, &mut lines);
    }
    let body_h = area.height as usize;
    let total = count_wrapped(&lines, width);
    app.rendered_total = total;
    let bottom = total.saturating_sub(body_h) as u16;
    let scroll = match app.pinned {
        true => bottom,
        false => app.scroll.min(bottom),
    };
    let body = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(body, area);
}

/// Claude Code-style inline block renderer. One block flattens to 1+
/// styled lines: a header line plus a collapsible body. Long bodies render
/// truncated to [`COLLAPSED_LINES`] with an expand hint; toggling shows the
/// full stored text. Selected blocks get a `▸` cursor marker.
fn render_block(block: &MessageBlock, selected: bool, out: &mut Vec<Line<'static>>) {
    let cursor = match selected {
        true => "▸ ",
        false => "  ",
    };
    match block {
        MessageBlock::User { text } => {
            out.push(Line::from(vec![
                Span::styled(
                    format!("{cursor}› "),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(text.clone()),
            ]));
        }
        MessageBlock::Reply { text } => {
            for line in text.lines() {
                out.push(Line::from(vec![Span::styled(
                    line.to_string(),
                    reply_style(line),
                )]));
            }
            if text.is_empty() {
                out.push(Line::from(""));
            }
        }
        MessageBlock::ToolCall {
            name,
            args_preview,
            ok,
            summary,
            output,
            expanded,
        } => {
            let name_style = match ok {
                true => Style::default().add_modifier(Modifier::BOLD),
                false => Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            };
            let status = match ok {
                true => "· ok",
                false => "· ✗ err",
            };
            let status_style = match ok {
                true => Style::default().fg(Color::DarkGray),
                false => Style::default().fg(Color::Red),
            };
            let expand_hint = block_expand_hint(block).unwrap_or_default();
            out.push(Line::from(vec![
                Span::styled(
                    format!("{cursor}⏺ "),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(name.clone(), name_style),
                Span::styled(
                    format!("({args_preview}) "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(status.to_string(), status_style),
                Span::styled(expand_hint, Style::default().fg(Color::DarkGray)),
            ]));
            out.push(Line::from(vec![
                Span::styled("    └ ", Style::default().fg(Color::DarkGray)),
                Span::styled(summary.clone(), Style::default().fg(Color::DarkGray)),
            ]));
            // Body: first line is blank-padded output; collapse window applies.
            let body: Vec<&str> = output.lines().collect();
            let shown = match expanded {
                true => body.len(),
                false => body.len().min(COLLAPSED_LINES.saturating_sub(1)),
            };
            for line in body.iter().take(shown) {
                out.push(Line::from(vec![
                    Span::styled("      ", Style::default()),
                    Span::styled((*line).to_string(), Style::default().fg(Color::DarkGray)),
                ]));
            }
            if !expanded && body.len() > shown {
                let rest = body.len() - shown;
                out.push(Line::from(vec![Span::styled(
                    format!("      … +{rest} lines (Enter to expand)"),
                    Style::default().fg(Color::DarkGray),
                )]));
            }
        }
        MessageBlock::Thinking { text, expanded } => {
            let n = text.lines().count();
            let marker = match expanded {
                true => "▾",
                false => "▸",
            };
            let expand_hint = block_expand_hint(block).unwrap_or_default();
            out.push(Line::from(vec![
                Span::styled(
                    format!("{cursor}{marker} thinking "),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("({n} lines)"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(expand_hint, Style::default().fg(Color::DarkGray)),
            ]));
            if *expanded {
                for line in text.lines() {
                    out.push(Line::from(vec![
                        Span::styled("      ", Style::default()),
                        Span::styled(line.to_string(), Style::default().fg(Color::DarkGray)),
                    ]));
                }
            }
        }
        MessageBlock::System { text } => {
            // Empty text = visual separator. Must be a truly empty line:
            // WordWrapper with trim:false renders whitespace-only lines as
            // TWO rows, which would desync scroll accounting.
            match text.is_empty() {
                true => out.push(Line::from("")),
                false => out.push(Line::from(vec![Span::styled(
                    format!("  {text}"),
                    Style::default().fg(Color::DarkGray),
                )])),
            }
        }
        MessageBlock::Error { text } => {
            out.push(Line::from(vec![Span::styled(
                format!("  [error] {text}"),
                Style::default().fg(Color::Red),
            )]));
        }
        MessageBlock::Trailer { text } => {
            out.push(Line::from(vec![Span::styled(
                format!("  {text}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )]));
        }
        MessageBlock::LiveTools { tools } => {
            // In-progress feed: spinner header + one row per resolved tool.
            // Replaced by final blocks when the turn completes.
            let dots = "…";
            out.push(Line::from(vec![
                Span::styled(
                    format!("{cursor}◌ working{dots} "),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!(
                        "{} tool{}",
                        tools.len(),
                        if tools.len() == 1 { "" } else { "s" }
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            for t in tools.iter() {
                let mark = match t.ok {
                    true => "└",
                    false => "✗",
                };
                let mark_style = match t.ok {
                    true => Style::default().fg(Color::DarkGray),
                    false => Style::default().fg(Color::Red),
                };
                out.push(Line::from(vec![
                    Span::styled(format!("    {mark} "), mark_style),
                    Span::raw(format!("{} {}", t.name, t.preview)),
                ]));
                out.push(Line::from(vec![
                    Span::styled("      ", Style::default()),
                    Span::styled(t.summary.clone(), Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
    }
}

/// Expand/collapse hint suffix for a block header, e.g. ` · Enter to expand`.
/// Empty for non-expandable blocks and expanded blocks (which show collapse).
fn block_expand_hint(block: &MessageBlock) -> Option<String> {
    match block.expandable() {
        false => None,
        true => Some(match block.is_expanded() {
            true => " · Enter to collapse".to_string(),
            false => " · Enter to expand".to_string(),
        }),
    }
}

/// Rendered line count of one block at `width`: mirrors `render_block`
/// structure, then applies `Paragraph::wrap(trim: false)`-style widening so
/// click mapping and height accounting agree on wrapped terminals.
fn block_height(block: &MessageBlock, width: usize) -> usize {
    let w = width.max(1);
    let mut flat: Vec<Line<'static>> = Vec::new();
    render_block(block, false, &mut flat);
    count_wrapped(&flat, w)
}
/// Wrapped line count of the flattened body at `width`, mirroring
/// `Paragraph::wrap(trim: false)` behavior for scroll anchoring.
/// Conservative ceiling: matches empty lines (1) and wraps long lines up.
fn count_wrapped(lines: &[Line<'static>], width: usize) -> usize {
    let w = width.max(1);
    lines
        .iter()
        .map(|l| {
            let line_w: usize = l
                .spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            line_w.div_ceil(w).max(1)
        })
        .sum()
}

fn render_header(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let mut spans = vec![
        Span::styled(
            "rem",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", app.cwd),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    match &app.last_prompt {
        Some(p) => {
            spans.push(Span::styled("   › ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::raw(truncate_prompt(p, 90)));
        }
        None => {
            spans.push(Span::styled(
                "   new session",
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn truncate_prompt(p: &str, max: usize) -> String {
    let flat: String = p.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.chars().count() > max {
        true => format!("{}…", flat.chars().take(max - 1).collect::<String>()),
        false => flat,
    }
}

/// Gap row between the transcript and the bottom pane. Terminal
/// background, matching Aster's unshaded gap: the shaded band starts at
/// the input box, not here.
fn render_gap(f: &mut ratatui::Frame, area: ratatui::layout::Rect) {
    f.render_widget(Paragraph::new(Line::from("")), area);
}

/// Busy/approval status row above the input band (terminal background).
/// Idle frames give it zero height, so it costs no rows when quiet.
fn render_status(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    if area.height == 0 {
        return;
    }
    if !app.pending_approvals.is_empty() {
        let line = Line::from(vec![
            Span::styled("◌ ", Style::default().fg(ACCENT)),
            Span::styled(
                format!("waiting approval ({} queued)", app.pending_approvals.len()),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                " · y approve · n deny · x abort",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    if app.busy {
        let elapsed = app.busy_since.elapsed();
        let spinner = SPINNER[(elapsed.as_millis() / 100) as usize % SPINNER.len()];
        let line = Line::from(vec![
            Span::styled(format!("{spinner} "), Style::default().fg(ACCENT)),
            Span::styled("working", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(" · {:.1}s · esc to interrupt", elapsed.as_secs_f32()),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    f.render_widget(Paragraph::new(Line::from("")), area);
}

/// Aster-style composer: a 3-row shaded band (1-row vertical padding around
/// the text line). Single-line editing: the text line is a 1-column inset
/// plus `❯ ` plus the visible input window; the placeholder is italic
/// faint when empty, and a busy hint replaces it while a turn runs.
fn render_input(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    f.render_widget(Block::default().style(Style::default().bg(PANE_BG)), area);
    if area.height < 3 || area.width < 8 {
        return;
    }
    let mid = ratatui::layout::Rect::new(area.x, area.y + 1, area.width, 1);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            "❯ ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
    ];
    if app.input.is_empty() {
        let hint = match app.busy {
            true => "…  (esc to interrupt)",
            false => "Message rem…  (/ for commands)",
        };
        spans.push(Span::styled(
            hint,
            Style::default()
                .fg(PLACEHOLDER)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        let max_w = (area.width as usize).saturating_sub(4);
        let (visible, _) = visible_window(&app.input, app.cursor, max_w);
        spans.push(Span::raw(visible));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), mid);
}

/// Aster-style footer: one left-aligned line — orange `▶▶▶ edit`, faint
/// model, turn count, and key hints. Busy state lives in the status row
/// above the input band, so the footer stays quiet during a turn.
fn render_footer(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let faint = Style::default().fg(Color::DarkGray);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled("▶▶▶ edit", Style::default().fg(ACCENT)),
        Span::styled(format!("  ·  {}", app.model), faint),
        Span::styled(
            format!(
                "  ·  {} turn{}",
                app.turns,
                if app.turns == 1 { "" } else { "s" }
            ),
            faint,
        ),
        Span::styled(
            match app.selected {
                Some(_) => "  ·  enter/space expand · esc input",
                None => "  ·  tab selects block · esc interrupt · ^D quit",
            },
            faint,
        ),
    ];
    if matches!(app.status, Status::Error) {
        spans.push(Span::styled(
            "  ·  error — see log",
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_styles_cover_diff_headers_and_body() {
        assert_eq!(reply_style("+ added"), Style::default().fg(Color::Green));
        assert_eq!(reply_style("- removed"), Style::default().fg(Color::Red));
        assert_eq!(reply_style("+++ b/file"), Style::default());
        assert_eq!(reply_style("--- a/file"), Style::default());
        assert_eq!(
            reply_style("```rust"),
            Style::default().fg(Color::DarkGray)
        );
        assert_eq!(
            reply_style("# Title"),
            Style::default().add_modifier(Modifier::BOLD)
        );
        assert_eq!(reply_style("plain"), Style::default());
    }

    #[test]
    fn char_helpers_respect_unicode_boundaries() {
        let mut s = "héllo".to_string();
        let mut cursor = 2;
        insert_char_at(&mut s, &mut cursor, 'X');
        assert_eq!(s, "héXllo");
        assert_eq!(cursor, 3);
        remove_char_at(&mut s, 2);
        assert_eq!(s, "héllo");

        let mut s = "foo bar".to_string();
        let mut cursor = 7;
        delete_word_before(&mut s, &mut cursor);
        assert_eq!(s, "foo ");
        assert_eq!(cursor, 4);
    }

    #[test]
    fn truncate_prompt_collapses_whitespace() {
        assert_eq!(truncate_prompt("fix\n  the   bug", 90), "fix the bug");
        assert_eq!(truncate_prompt("abcdef", 5), "abcd…");
    }

    #[test]
    fn count_wrapped_matches_empty_and_wrapped_lines() {
        let lines = vec![
            Line::from(""),
            Line::from("1234567890"),
            Line::from("12345678901"),
        ];
        assert_eq!(count_wrapped(&lines, 10), 1 + 1 + 2);
        assert_eq!(count_wrapped(&[], 10), 0);
    }

    #[test]
    fn visible_window_keeps_caret_on_screen() {
        // Fits: whole input, caret at end.
        let (v, c) = visible_window("hi", 2, 10);
        assert_eq!((v.as_str(), c), ("hi", 2));
        // Overflow at end: tail anchored, caret on last column.
        let (v, c) = visible_window("abcdefgh", 8, 5);
        assert_eq!(v, "efgh");
        assert_eq!(c, 4);
        // Overflow mid-line: window follows the caret.
        let (v, c) = visible_window("abcdefgh", 2, 5);
        assert_eq!(v, "abcde");
        assert_eq!(c, 2);
    }

    #[test]
    fn tool_block_truncates_long_output_until_expanded() {
        let output = (0..30).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let mut block = MessageBlock::ToolCall {
            name: "bash".to_string(),
            args_preview: "cargo test".to_string(),
            ok: true,
            summary: "ok".to_string(),
            output,
            expanded: false,
        };
        assert!(block.expandable());
        assert!(!block.is_expanded());
        // Collapsed: header + summary + 9 body + "+N lines" hint = 12.
        assert_eq!(block_height(&block, 80), 12);
        let mut lines = Vec::new();
        render_block(&block, false, &mut lines);
        assert_eq!(lines.len(), 12);
        assert!(lines[0].to_string().contains("Enter to expand"));
        // Expanded: header + summary + 30 body = 32.
        block.toggle();
        assert!(block.is_expanded());
        assert_eq!(block_height(&block, 80), 32);
        let mut lines = Vec::new();
        render_block(&block, false, &mut lines);
        assert_eq!(lines.len(), 32);
        assert!(lines[0].to_string().contains("Enter to collapse"));
    }

    #[test]
    fn short_tool_block_is_not_expandable() {
        let block = MessageBlock::ToolCall {
            name: "read".to_string(),
            args_preview: "src/main.rs".to_string(),
            ok: true,
            summary: "42 lines".to_string(),
            output: "line 1\nline 2".to_string(),
            expanded: false,
        };
        assert!(!block.expandable());
        // header + summary + 2 body, no hint row.
        assert_eq!(block_height(&block, 80), 4);
    }

    #[test]
    fn thinking_block_collapses_by_default() {
        let text = (0..20).map(|i| format!("thought {i}")).collect::<Vec<_>>().join("\n");
        let mut block = MessageBlock::Thinking {
            text,
            expanded: false,
        };
        assert!(block.expandable());
        assert_eq!(block_height(&block, 80), 1);
        let mut lines = Vec::new();
        render_block(&block, true, &mut lines);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].to_string().contains("▸ thinking (20 lines)"));
        assert!(lines[0].to_string().contains("▸ ›") == false);
        block.toggle();
        assert_eq!(block_height(&block, 80), 21);
    }

    #[test]
    fn selection_moves_between_expandable_blocks() {
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        app.blocks.push(MessageBlock::System {
            text: "hi".to_string(),
        });
        app.blocks.push(MessageBlock::ToolCall {
            name: "bash".to_string(),
            args_preview: "x".to_string(),
            ok: true,
            summary: "s".to_string(),
            output: (0..20).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n"),
            expanded: false,
        });
        app.blocks.push(MessageBlock::Reply {
            text: "done".to_string(),
        });
        // Newest expandable = the tool block at index 1.
        app.select_newest(false);
        assert_eq!(app.selected, Some(1));
        // Moving down wraps within the single expandable block.
        app.move_selection(false);
        assert_eq!(app.selected, Some(1));
        // Toggle flips expanded.
        app.toggle_selected();
        assert!(app.blocks[1].is_expanded());
    }

    #[test]
    fn headless_full_frame_renders_without_old_chrome() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("test-model".to_string());
        app.blocks.clear();
        app.push_user("list files");
        let output = (0..25).map(|i| format!("file{i}.rs")).collect::<Vec<_>>().join("\n");
        app.push_turn(
            &[crate::agent::ToolEvent {
                name: "bash".to_string(),
                args: serde_json::json!({"command": "ls"}),
                ok: true,
                summary: "25 files".to_string(),
                output,
            }],
            &["I should list files first, then summarize.".to_string()],
            &Ok("Here are your files.".to_string()),
        );
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let text = terminal.backend().to_string();
        // New chrome present…
        assert!(text.contains("test-model"));
        assert!(text.contains("bash(ls)"));
        assert!(text.contains("thinking (1 lines)"));
        assert!(text.contains("Here are your files."));
        // …old chrome gone: no sweeper bar, no live think region, no ctrl+d.
        assert!(!text.contains("ctrl+d"));
        assert!(!text.contains("thinking ·"));
        // No line is a full-width bar of ─/━ run (old activity bar).
        for line in text.lines() {
            let bar_run = line.chars().filter(|c| *c == '─' || *c == '━').count();
            assert!(bar_run < 70, "activity bar remnant: {line}");
        }
    }

    /// Row text helper for chrome-position assertions: TestBackend exposes
    /// cells, not string rows, so read the buffer directly.
    fn row_text(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
        y: u16,
        w: u16,
    ) -> String {
        let buf = terminal.backend().buffer();
        (0..w)
            .map(|x| buf.get(x, y).symbol().to_string())
            .collect::<String>()
    }

    #[test]
    fn input_band_renders_aster_chrome() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("test-model".to_string());
        app.blocks.clear();
        app.input = "hello".to_string();
        app.cursor = 5;
        terminal.draw(|f| render(f, &mut app)).unwrap();
        // Idle 24-row frame: header 0, body 1..=18, gap 19, band 20..=22,
        // footer 23. Status takes zero rows when idle.
        let band_top = 24u16 - 4;
        // Shaded band rows carry the pane background; gap stays terminal bg.
        for y in band_top..24 - 1 {
            assert_eq!(
                terminal.backend().buffer().get(0, y).bg,
                PANE_BG,
                "band row {y} should be shaded"
            );
        }
        assert_eq!(
            terminal.backend().buffer().get(0, band_top - 1).bg,
            ratatui::style::Color::Reset,
            "gap row should be terminal bg"
        );
        // Prompt + typed text on the band's middle row.
        let mid = row_text(&terminal, band_top + 1, 80);
        assert!(mid.contains('❯'), "prompt missing: {mid}");
        assert!(mid.contains("hello"), "typed text missing: {mid}");
        // Footer: Aster-style mode/model/turns line.
        let footer = row_text(&terminal, 24 - 1, 80);
        assert!(footer.contains("▶▶▶ edit"), "footer mode missing: {footer}");
        assert!(
            footer.contains("test-model"),
            "footer model missing: {footer}"
        );
        // Caret accounts for the 1-column inset + 2-column prompt.
        assert_eq!(app.cursor_x(80), 3 + 5);
    }

    #[test]
    fn idle_band_shows_placeholder_and_busy_shows_status() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("test-model".to_string());
        app.blocks.clear();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let mid = row_text(&terminal, 21, 80);
        assert!(mid.contains("Message rem…"), "placeholder missing: {mid}");

        app.busy = true;
        app.busy_since = Instant::now();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        // Busy frame steals one row for the status above the band.
        let status = row_text(&terminal, 19, 80);
        assert!(status.contains("working"), "status missing: {status}");
        assert!(
            status.contains("esc to interrupt"),
            "hint missing: {status}"
        );
        let mid = row_text(&terminal, 21, 80);
        assert!(mid.contains("esc to interrupt"), "busy hint missing: {mid}");
    }

    #[test]
    fn approval_queues_status_above_input_band() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("test-model".to_string());
        app.blocks.clear();
        let (req, _rx) = approval_req("bash", "cargo test");
        app.pending_approvals.push_back(req);
        terminal.draw(|f| render(f, &mut app)).unwrap();
        // Status row sits above the band and outside the centered modal.
        let status = row_text(&terminal, 19, 80);
        assert!(
            status.contains("waiting approval"),
            "approval status missing: {status}"
        );
        // Input band still shaded underneath.
        assert_eq!(terminal.backend().buffer().get(0, 21).bg, PANE_BG);
    }

    #[test]
    fn click_maps_row_to_block_and_toggles() {
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        app.blocks.push(MessageBlock::ToolCall {
            name: "bash".to_string(),
            args_preview: "x".to_string(),
            ok: true,
            summary: "s".to_string(),
            output: (0..20).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n"),
            expanded: false,
        });
        app.view_h = 24;
        app.pinned = true;
        app.rendered_total = 12;
        assert!(app.click_toggle(0, 80));
        assert_eq!(app.selected, Some(0));
        assert!(app.blocks[0].is_expanded());
    }

    fn approval_req(tool: &str, preview: &str) -> (ApprovalRequest, tokio::sync::oneshot::Receiver<ApprovalDecision>) {
        let (reply, rx) = tokio::sync::oneshot::channel();
        let req = ApprovalRequest {
            tool_name: tool.to_string(),
            args_preview: preview.to_string(),
            full_args: serde_json::json!({}),
            reason: "needs approval".to_string(),
            reply,
        };
        (req, rx)
    }

    #[test]
    fn modal_keys_resolve_approval_with_audit_block() {
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        let (req, rx) = approval_req("bash", "cargo test");
        app.pending_approvals.push_back(req);
        // Bare keys only: y approves.
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('y'), KeyModifiers::empty()));
        assert!(app.pending_approvals.is_empty());
        assert!(rx.blocking_recv().is_ok());
        assert!(app.blocks.iter().any(|b| matches!(b, MessageBlock::System { text } if text.contains("permission approved"))));
    }

    #[test]
    fn modal_deny_and_abort_map_to_decisions() {
        for (key, expect) in [
            (KeyCode::Char('n'), ApprovalDecision::Deny),
            (KeyCode::Char('x'), ApprovalDecision::AbortTurn),
            (KeyCode::Char('a'), ApprovalDecision::ApproveAlways),
        ] {
            let mut app = App::new("model".to_string());
            app.blocks.clear();
            let (req, rx) = approval_req("write", "a.txt");
            app.pending_approvals.push_back(req);
            let agent = std::sync::Arc::new(StubAgent);
            let (tx, _rx) = mpsc::channel::<TurnResult>();
            let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
            assert!(!handle_key(&mut app, &agent, &tx, &think_tx, key, KeyModifiers::empty()));
            assert_eq!(rx.blocking_recv().unwrap(), expect);
        }
    }

    #[test]
    fn modal_renders_tool_and_key_hints() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("test-model".to_string());
        app.blocks.clear();
        let (req, _rx) = approval_req("bash", "cargo test");
        app.pending_approvals.push_back(req);
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("approval needed"), "got: {text}");
        assert!(text.contains("cargo test"), "got: {text}");
        assert!(text.contains("[y] approve"), "got: {text}");
        assert!(text.contains("[n] deny"), "got: {text}");
    }

    #[test]
    fn esc_while_busy_aborts_turn_and_keeps_partial() {
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        app.busy = true;
        app.turn_generation = 3;
        app.blocks.push(MessageBlock::LiveTools {
            tools: vec![LiveTool {
                name: "bash".to_string(),
                preview: "cargo test".to_string(),
                ok: true,
                summary: "25 files".to_string(),
            }],
        });
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // Busy Esc interrupts (false = don't quit); idle double-Esc below
        // is untouched.
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Esc, KeyModifiers::empty()));
        assert!(!app.busy);
        assert!(app.current_turn.is_none());
        assert!(app.current_watcher.is_none());
        // Partial tool kept as a final block + interrupted marker + trailer.
        assert!(app.blocks.iter().any(|b| matches!(b, MessageBlock::ToolCall { name, .. } if name == "bash")));
        assert!(app.blocks.iter().any(|b| matches!(b, MessageBlock::System { text } if text == "interrupted.")));
        assert!(app.blocks.iter().any(|b| matches!(b, MessageBlock::Trailer { text } if text == "Interrupted (1 tool)")));
        // D2: generation bumped, so a late TurnResult with seq 3 is stale.
        assert_eq!(app.turn_generation, 4);
    }

    #[test]
    fn abort_turn_without_live_row_still_marks_interrupted() {
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        app.busy = true;
        abort_turn(&mut app);
        assert!(!app.busy);
        assert!(app.blocks.iter().any(|b| matches!(b, MessageBlock::System { text } if text == "interrupted.")));
        assert!(!app.blocks.iter().any(|b| matches!(b, MessageBlock::Trailer { .. })));
    }

    #[test]
    fn abort_turn_is_noop_when_idle() {
        let mut app = App::new("model".to_string());
        let before = app.blocks.len();
        abort_turn(&mut app);
        assert_eq!(app.blocks.len(), before);
        assert_eq!(app.turn_generation, 0);
    }

    #[test]
    fn ctrl_d_with_text_is_noop() {
        let mut app = App::new("model".to_string());
        app.input = "hi".to_string();
        app.cursor = 2;
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "hi");
    }

    #[test]
    fn ctrl_d_empty_idle_quits() {
        let mut app = App::new("model".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('d'), KeyModifiers::CONTROL));
    }

    #[test]
    fn ctrl_d_empty_busy_aborts_then_quits() {
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        app.busy = true;
        app.turn_generation = 5;
        app.blocks.push(MessageBlock::LiveTools {
            tools: vec![LiveTool {
                name: "bash".to_string(),
                preview: "cargo test".to_string(),
                ok: true,
                summary: "25 files".to_string(),
            }],
        });
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(!app.busy);
        assert_eq!(app.turn_generation, 6);
        assert!(app.blocks.iter().any(|b| matches!(b, MessageBlock::System { text } if text == "interrupted.")));
        assert!(app.blocks.iter().any(|b| matches!(b, MessageBlock::Trailer { text } if text == "Interrupted (1 tool)")));
    }

    #[test]
    fn ctrl_d_empty_modal_resolves_abort_then_quits() {
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        app.busy = true;
        let (req, rx) = approval_req("bash", "cargo test");
        app.pending_approvals.push_back(req);
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // Modal CONTROL exemption lets Ctrl+D through; head resolves as
        // AbortTurn (D3), busy turn aborts, then quit.
        assert!(handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(rx.blocking_recv().unwrap(), ApprovalDecision::AbortTurn);
        assert!(app.pending_approvals.is_empty());
        assert!(!app.busy);
    }

    #[test]
    fn ctrl_d_empty_selected_quits() {
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        app.blocks.push(MessageBlock::System { text: "hi".to_string() });
        app.selected = Some(0);
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // CONTROL routes before selection commands, so Ctrl+D quits even
        // with a block selected.
        assert!(handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('d'), KeyModifiers::CONTROL));
    }

    #[test]
    fn ctrl_c_idle_clears_input_without_quit() {
        let mut app = App::new("model".to_string());
        app.input = "hello".to_string();
        app.cursor = 5;
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.input.is_empty());
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn ctrl_c_while_busy_clears_only_never_interrupts() {
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // Busy + typed input: clears the line, the turn keeps running.
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        app.busy = true;
        app.turn_generation = 7;
        app.input = "partial".to_string();
        app.cursor = 7;
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.input.is_empty());
        assert_eq!(app.cursor, 0);
        assert!(app.busy, "Ctrl+C must never interrupt a busy turn");
        assert_eq!(app.turn_generation, 7);
        assert!(!app.blocks.iter().any(|b| matches!(b, MessageBlock::System { text } if text == "interrupted.")));
        // Busy + empty input: no-op, still no interrupt.
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.busy);
        assert_eq!(app.turn_generation, 7);
    }

    #[test]
    fn ctrl_c_in_modal_is_ignored() {
        let mut app = App::new("model".to_string());
        app.blocks.clear();
        app.busy = true;
        app.input = "typed".to_string();
        app.cursor = 5;
        let (req, mut rx) = approval_req("bash", "cargo test");
        app.pending_approvals.push_back(req);
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // Modal CONTROL blanket-ignore: no clear, no resolve, no quit.
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "typed");
        assert_eq!(app.cursor, 5);
        assert_eq!(app.pending_approvals.len(), 1);
        assert!(rx.try_recv().is_err(), "modal Ctrl+C must not resolve the approval");
        assert!(app.busy);
    }

    struct StubAgent;

    #[async_trait::async_trait]
    impl AgentLoop for StubAgent {
        async fn chat(&self, _prompt: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }
}
