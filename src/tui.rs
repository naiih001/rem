use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use crossterm::{
    event::{
        self, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::{Backend, ClearType, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget, Wrap},
};
use tokio::task::JoinHandle;
use unicode_width::UnicodeWidthStr;

use crate::agent::{AgentLoop, ToolEvent};
use rig::message::{AssistantContent, Message, UserContent};
use crate::history;
use crate::permissions::{ApprovalDecision, ApprovalRequest, ApprovalRx};
use crate::modes::Mode;
use crate::theme::Theme;

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
/// Reasoning is intentionally absent (ADR-0005 hidden thinking): the worker
/// still records it internally, but nothing reaches the transcript.
struct TurnResult {
    result: Result<String, String>,
    events: Vec<ToolEvent>,
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

struct TerminalInputMode {
    keyboard_enhancement: bool,
    active: bool,
}

impl TerminalInputMode {
    fn enable() -> anyhow::Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        let keyboard_enhancement = matches!(
            crossterm::terminal::supports_keyboard_enhancement(),
            Ok(true)
        );
        if keyboard_enhancement
            && let Err(error) = crossterm::execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
        {
            disable_raw_mode().ok();
            return Err(error).context("enable terminal keyboard enhancement");
        }
        Ok(Self {
            keyboard_enhancement,
            active: true,
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let keyboard_result = if self.keyboard_enhancement {
            crossterm::execute!(io::stdout(), PopKeyboardEnhancementFlags)
        } else {
            Ok(())
        };
        let raw_mode_result = disable_raw_mode();
        keyboard_result.and(raw_mode_result)
    }
}

impl Drop for TerminalInputMode {
    fn drop(&mut self) {
        if self.active {
            if self.keyboard_enhancement {
                crossterm::execute!(io::stdout(), PopKeyboardEnhancementFlags).ok();
            }
            disable_raw_mode().ok();
        }
    }
}

fn run_app(
    agent: impl AgentLoop + Send + Sync + 'static,
    approval_rx: ApprovalRx,
) -> anyhow::Result<()> {
    let model = agent.model_name();
    let effort = agent.effort_name();
    let agent = Arc::new(agent);
    let (tx, rx) = mpsc::channel::<TurnResult>();
    let (think_tx, think_rx) = mpsc::channel::<ThinkMsg>();

    // The bottom-anchored pane leaves finished transcript rows in terminal
    // scrollback via `insert_before`. No mouse capture: it would steal the
    // terminal's own selection over the scrollback transcript.
    let mut input_mode = TerminalInputMode::enable()?;
    // Wipe the terminal so rem owns the full screen from the start,
    // matching Aster's clear_screen on launch. Purge clears the
    // scrollback (cargo output, shell prompt) above the viewport.
    // The inline viewport is recreated at its new height as the editor grows.
    use crossterm::{
        cursor::MoveTo,
        execute,
        terminal::{Clear, ClearType, size as term_size},
    };
    let (_, h) = term_size().context("terminal size")?;
    let pane_height = PANE_ROWS.min(h);
    execute!(
        io::stdout(),
        Clear(ClearType::All),
        Clear(ClearType::Purge),
        MoveTo(0, h.saturating_sub(pane_height))
    )?;
    let backend = CrosstermBackend::new(io::stdout());
    let outcome = {
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(pane_height),
            },
        )
        .context("create terminal")?;

        let mut app = App::new(model, effort);
        // Skills index for `/skill:<partial>` suggestions (Task 14 cache).
        app.skill_names = agent.list_skills().iter().map(|s| s.name.clone()).collect();
        // Resume: on relaunch with REM_SESSION_ID set, replay the stored
        // transcript so the pane shows prior turns before new input.
        if let Ok(sid) = std::env::var("REM_SESSION_ID") {
            if let Ok(conn) = crate::sessions::open() {
                if let Ok(Some(s)) = crate::sessions::get_session(&conn, &sid) {
                    app.replay_json(&s.messages_json);
                }
            }
        }
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

        // Sync final id for the post-exit resume hint in main.rs: /resume,
        // /fork, and the picker mutate app.session_id after startup, and both
        // quit paths (/quit, Ctrl+D) return through here.
        unsafe {
            std::env::set_var("REM_SESSION_ID", &app.session_id);
        }
        outcome
    };

    input_mode
        .restore()
        .context("restore terminal input mode")?;
    println!();
    outcome
}

/// Minimum bottom-pane height: gap(1) + status(1) + input band(3) + footer(1).
const PANE_ROWS: u16 = 6;
const MAX_INPUT_LINES: usize = 5;

fn pane_viewport_area(width: u16, height: u16, pane_height: u16) -> Rect {
    let pane_height = pane_height.min(height);
    Rect::new(0, height.saturating_sub(pane_height), width, pane_height)
}

fn resize_pane_viewport<B: Backend>(
    terminal: &mut Terminal<B>,
    backend: impl FnOnce() -> B,
    width: u16,
    height: u16,
    pane_height: u16,
) -> io::Result<()> {
    let area = pane_viewport_area(width, height, pane_height);
    if terminal.get_frame().area() == area {
        return Ok(());
    }
    let previous = terminal.get_frame().area();
    let clear_from = previous.y.min(area.y).min(height.saturating_sub(1));
    let mut backend = backend();
    backend.set_cursor_position(Position::new(0, clear_from))?;
    backend.clear_region(ClearType::AfterCursor)?;
    backend.set_cursor_position(Position::new(0, area.y))?;
    *terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(area.height),
        },
    )?;
    Ok(())
}

/// Braille spinner frames for the busy status row (same set Aster uses).
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Cycling busy-status verbs (Claude Code / Aster style): sequential rotation,
/// restarting at `working` each task. Word index derives from busy-elapsed
/// time so it advances with the existing 50ms frame loop — no new plumbing.
const BUSY_VERBS: [&str; 10] = [
    "working",
    "thinking",
    "cooking",
    "pondering",
    "reasoning",
    "crafting",
    "brewing",
    "scheming",
    "conjuring",
    "noodling",
];
/// Seconds each busy verb stays on screen before rotating to the next.
const BUSY_VERB_SECS: u64 = 2;
/// Slash-menu popup budget (Aster `menu_lines` 10-cap): max command rows;
/// one extra `+N more` row appears on overflow.
const MENU_MAX_ROWS: usize = 10;
/// Preferred visible command rows when the menu is open. The pane grows to
/// this size when the terminal has room, while `menu_row_capacity` still
/// keeps the popup bounded on short terminals.
const MENU_PREFERRED_ROWS: usize = 7;
/// Scrollback transcript (ADR-0005): finished rows print into the
/// terminal's own scrollback via `insert_before` and are never touched
/// again. `App` holds only a queue of pending `Line` groups; the event
/// loop drains it above the bottom-anchored viewport. No selection, no
/// expand/collapse, no in-app scroll — the terminal owns all of that.
///
/// Slash-command registry (ADR-0007): single source of truth for menu
/// rows and `/help` output. `takes_arg` is reserved for future use.
struct Command {
    name: &'static str,
    takes_arg: bool,
    desc: &'static str,
}

static COMMANDS: &[Command] = &[
    Command {
        name: "quit",
        takes_arg: false,
        desc: "quit the app",
    },
    Command {
        name: "clear",
        takes_arg: false,
        desc: "clear transcript",
    },
    Command {
        name: "help",
        takes_arg: false,
        desc: "list commands",
    },
    Command {
        name: "theme",
        takes_arg: true,
        desc: "list or switch color themes",
    },
    Command {
        name: "init",
        takes_arg: false,
        desc: "generate AGENTS.md for this project",
    },
    Command {
        name: "resume",
        takes_arg: true,
        desc: "resume a saved session",
    },
    Command {
        name: "rename",
        takes_arg: true,
        desc: "rename current session",
    },
    Command {
        name: "fork",
        takes_arg: false,
        desc: "fork session from here",
    },
    Command {
        name: "skills",
        takes_arg: true,
        desc: "list or reload skills",
    },
];

/// Fixed instruction for `/init` (ADR-0009): runs as a normal agent turn so
/// the model inspects the repo with read/glob/grep tools and overwrites
/// project-root `AGENTS.md`. Takes effect on next restart (preamble is
/// frozen at startup).
const INIT_PROMPT: &str = "Generate an AGENTS.md file for this project. \
Inspect the repository with the read/list_directory/glob/grep/git_status tools to learn its layout, languages, build/test commands, and conventions. \
Then write a concise AGENTS.md to the project root with the write tool, overwriting any existing file. \
Keep it factual and repo-specific: project overview, layout, build/test/lint commands, code conventions, and anything an agent needs to work here. \
Reply with a one-line summary of what you wrote.";

/// Split `/name arg...` into the registry command + trailing arg.
/// Returns `None` when the input is not a slash command or the name is
/// unknown. The arg is the trimmed remainder (may be empty).
fn parse_command(text: &str) -> Option<(&'static Command, String)> {
    let rest = text.trim().strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim().to_string();
    let cmd = COMMANDS.iter().find(|c| c.name == name)?;
    Some((cmd, arg))
}

/// Slash-menu prefix filter: the menu lives only while the input is a bare
/// `/token` — must start with `/` and contain no whitespace. Empty prefix
/// (`/`) matches all. Case-sensitive.
fn menu_matches(input: &str) -> Vec<&'static Command> {
    let Some(token) = input.strip_prefix('/') else {
        return Vec::new();
    };
    if token.chars().any(|c| c.is_whitespace()) {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(token))
        .collect()
}

/// Theme-arg partial: the menu suggests installed theme names while the
/// input is `/theme <partial>` — the `theme` command, exactly one
/// whitespace run, then a partial name with no whitespace. Returns the
/// partial (empty = bare `/theme `, list everything). `None` for anything
/// else: bare `/theme` belongs to the command menu, multi-token args
/// dispatch directly. Case-sensitive, mirroring `menu_matches`.
fn theme_arg_partial(input: &str) -> Option<String> {
    let rest = input.strip_prefix('/')?;
    let partial = rest.strip_prefix("theme")?;
    if partial.is_empty() || !partial.starts_with(char::is_whitespace) {
        return None;
    }
    let partial = partial.trim_start();
    if partial.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(partial.to_string())
}

/// Prefix-filter installed theme names, order preserved (`list_themes`
/// already sorts). Pure seam for tests; the live path composes this with
/// `Theme::list_themes()`.
fn match_theme_names(names: &[String], partial: &str) -> Vec<String> {
    names
        .iter()
        .filter(|n| n.starts_with(partial))
        .cloned()
        .collect()
}

/// Live theme suggestions for `/theme <partial>`: installed names filtered
/// by prefix. Filesystem errors → no suggestions (menu stays closed).
fn theme_arg_matches(input: &str) -> Vec<String> {
    let Some(partial) = theme_arg_partial(input) else {
        return Vec::new();
    };
    match Theme::list_themes() {
        Ok(names) => match_theme_names(&names, &partial),
        Err(_) => Vec::new(),
    }
}

/// Skill-arg partial: the menu suggests discovered skill names while the
/// input is `/skill:<partial>` — colon form, partial name with no
/// whitespace. Returns the partial (empty = bare `/skill:`, list
/// everything). `None` for anything else. Case-sensitive, mirroring
/// `menu_matches`. Unlike themes (live filesystem), names come from the
/// `App.skill_names` cache — see `skill_arg_matches_for`.
fn skill_arg_partial(input: &str) -> Option<String> {
    let rest = input.strip_prefix('/')?;
    let partial = rest.strip_prefix("skill:")?;
    if partial.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(partial.to_string())
}

/// Prefix-filter skill names, order preserved. Pure seam for tests; the live
/// path composes this with the `App.skill_names` cache.
fn match_skill_names(names: &[String], partial: &str) -> Vec<String> {
    names
        .iter()
        .filter(|n| n.starts_with(partial))
        .cloned()
        .collect()
}

/// Skill suggestions for an input + cached name list.
fn skill_arg_matches_for(input: &str, names: &[String]) -> Vec<String> {
    let Some(partial) = skill_arg_partial(input) else {
        return Vec::new();
    };
    match_skill_names(names, &partial)
}

/// Total menu rows: command rows for a bare `/token`, theme-name rows for
/// `/theme <partial>`, skill rows for `/skill:<partial>`. Mutually exclusive
/// by construction, so this is one mode's count — never a mixed sum.
fn menu_row_count(input: &str) -> usize {
    let n = menu_matches(input).len();
    if n > 0 {
        n
    } else {
        theme_arg_matches(input).len()
    }
}

/// Menu rows for a live `App`: command/theme counts plus skill suggestions
/// from the cache.
fn menu_row_count_app(app: &App) -> usize {
    let n = menu_row_count(&app.input);
    if n > 0 {
        n
    } else {
        skill_arg_matches_for(&app.input, &app.skill_names).len()
    }
}

/// True when the slash menu should show: at least one match.
fn is_menu_open(app: &App) -> bool {
    menu_row_count_app(app) > 0
}

fn menu_status_height(app: &App) -> u16 {
    if is_menu_open(app) && !app.busy && app.pending_approvals.is_empty() {
        0
    } else {
        1
    }
}

fn menu_row_capacity(app: &App) -> u16 {
    app.pane_height(app.term_width)
        .saturating_sub(menu_status_height(app) + app.input_band_height(app.term_width, true) + 1)
}

fn menu_height(app: &App) -> u16 {
    if !is_menu_open(app) {
        return 0;
    }
    (menu_row_count_app(app).min(MENU_MAX_ROWS) as u16).min(menu_row_capacity(app))
}

/// The gap absorbs unused menu rows so the composer stays on the same row.
fn menu_gap_height(app: &App) -> u16 {
    if is_menu_open(app) {
        menu_row_capacity(app).saturating_sub(menu_height(app))
    } else {
        1
    }
}

/// Reconcile `menu_sel` with the current input after an edit: menu open +
/// `None` → `Some(0)`; `Some(i)` → clamped; menu closed → `None`.
fn clamp_menu_sel(app: &mut App) {
    let n = menu_row_count_app(app);
    if n == 0 {
        app.menu_sel = None;
    } else {
        app.menu_sel = Some(app.menu_sel.map_or(0, |i| i.min(n - 1)));
    }
}

struct SessionPicker {
    items: Vec<crate::sessions::Session>,
    sel: usize,
    show_all: bool,
}

struct App {
    model: String,
    effort: String,
    mode: Mode,
    /// Active color theme. Swapped live by `/theme`; every render and
    /// history-row builder reads from here (no `const` colors remain).
    theme: Theme,
    /// Finished transcript groups waiting to print above the viewport.
    /// Each entry is one `history::` row group (already wrapped to
    /// `term_width` at enqueue time... actually wrapped at drain time;
    /// see `drain_print_queue`).
    print_queue: Vec<Vec<Line<'static>>>,
    input: String,
    cursor: usize, // char index into `input`
    input_scroll: usize,
    max_visible_input_lines: usize,
    history: Vec<String>,
    hist_idx: Option<usize>,
    session_id: String,
    session_title: String,
    session_picker: Option<SessionPicker>,
    busy: bool,
    busy_since: Instant,
    dirty: bool,
    /// Live tool rows already streamed this turn (count only — the rows
    /// themselves went straight to scrollback). Used by `abort_turn` to
    /// synthesize the `Interrupted (N tools)` trailer.
    streamed_tools: usize,
    /// Slash-menu selection index (ADR-0007). `None` = menu closed;
    /// `Some(i)` = menu open with row `i` highlighted.
    menu_sel: Option<usize>,
    /// Cached skill folder names for `/skill:<partial>` suggestions.
    /// Refreshed at startup and on `/skills reload`.
    skill_names: Vec<String>,
    /// Pending human approvals (FIFO). Head renders as a blocking modal;
    /// resolving it resumes the parked agent worker in the same run.
    pending_approvals: VecDeque<ApprovalRequest>,
    /// `/clear` flag: wipe screen + scrollback before the next drain.
    request_clear_screen: bool,
    /// In-flight turn worker + live-feed watcher (Task 1 / ADR-0006).
    /// D1: `tokio::spawn` tasks are independent — aborting the outer chat
    /// task does NOT stop a nested watcher, so `submit()` spawns the 80ms
    /// watcher as a sibling and both handles are stored here for `abort_turn`.
    current_turn: Option<JoinHandle<()>>,
    current_watcher: Option<JoinHandle<()>>,
    /// Turn generation guard (D2): bumped on every `submit` and every
    /// `abort_turn`; the `event_loop` drain drops any `TurnResult` whose
    /// `seq` no longer matches (send-then-abort race).
    turn_generation: u64,
    /// Last terminal width seen; row groups wrap to this at drain time.
    term_width: u16,
    /// Freeze transcript drain for one frame after a terminal resize so
    /// queued rows cannot race viewport repositioning.
    skip_drain_once: bool,
}

/// Resolve a queued approval: send the decision over the oneshot,
/// print the approval rows (ADR-0005), then queue an audit notice.
/// A dropped/closed oneshot means the worker already moved
/// on; the request is simply forgotten.
fn resolve_approval(app: &mut App, decision: ApprovalDecision) {
    let Some(req) = app.pending_approvals.pop_front() else {
        return;
    };
    // Print first so the decision rows land above the audit notice.
    app.enqueue_approval_rows(&req, app.pending_approvals.len());
    let label = match decision {
        ApprovalDecision::Approve => "approved",
        ApprovalDecision::ApproveAlways => "approved (always this session)",
        ApprovalDecision::Deny => "denied",
        ApprovalDecision::AbortTurn => "aborted turn",
    };
    let tool_name = req.tool_name.clone();
    let args_preview = req.args_preview.clone();
    let _ = req.reply.send(decision);
    app.enqueue_notice(format!("permission {label}: {tool_name}({args_preview})"));
    app.dirty = true;
}

/// Abort the in-flight turn worker + live-feed watcher (Task 1 / ADR-0006).
/// Live rows already streamed stay in scrollback; the trailer counts them
/// via `streamed_tools`. Then an `interrupted.` marker + `Interrupted`
/// trailer print above the viewport.
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
    let n = app.streamed_tools;
    app.streamed_tools = 0;
    app.enqueue_notice("interrupted.".to_string());
    if n > 0 {
        app.enqueue_trailer(format!(
            "Interrupted ({} tool{})",
            n,
            if n == 1 { "" } else { "s" }
        ));
    }
    app.dirty = true;
}

/// Format turn duration like Aster's trailer (`22.4s`).
fn fmt_elapsed(d: Duration) -> String {
    format!("{:.1}s", d.as_secs_f32())
}

impl App {
    fn new(model: String, effort: String) -> Self {
        Self::with_theme(model, effort, Theme::load_active())
    }

    fn with_theme(model: String, effort: String, theme: Theme) -> Self {
        let mut app = Self {
            model,
            effort,
            mode: Mode::default(),
            theme,
            print_queue: Vec::new(),
            input: String::new(),
            cursor: 0,
            input_scroll: 0,
            max_visible_input_lines: MAX_INPUT_LINES,
            history: Vec::new(),
            hist_idx: None,
            session_id: std::env::var("REM_SESSION_ID").unwrap_or_default(),
            session_title: std::env::var("REM_SESSION_TITLE").unwrap_or_default(),
            session_picker: None,
            busy: false,
            busy_since: Instant::now(),
            dirty: true,
            streamed_tools: 0,
            menu_sel: None,
            skill_names: Vec::new(),
            pending_approvals: VecDeque::new(),
            current_turn: None,
            current_watcher: None,
            turn_generation: 0,
            term_width: 80,
            request_clear_screen: false,
            skip_drain_once: false,
        };
        app.enqueue_notice(
            "rem — esc interrupt · ^D quit when empty · ^C clear · /quit quit · /clear clears."
                .to_string(),
        );
        app
    }

    /// Queue one finished row group for the scrollback drain. Row builders
    /// wrap to `term_width` at drain time; the queue holds a marker width
    /// so a resize between enqueue and drain re-wraps correctly.
    fn enqueue(&mut self, rows: Vec<Line<'static>>) {
        if !rows.is_empty() {
            self.print_queue.push(rows);
        }
    }

    fn enqueue_user(&mut self, prompt: &str) {
        let w = self.term_width;
        self.enqueue(history::user_row(prompt, w as usize, &self.theme));
    }

    fn enqueue_notice(&mut self, text: String) {
        let w = self.term_width;
        self.enqueue(history::notice_row(&text, w as usize));
    }

    fn enqueue_trailer(&mut self, text: String) {
        let w = self.term_width;
        self.enqueue(history::trailer_row(&text, w as usize));
    }

    fn enqueue_approval_rows(&mut self, req: &ApprovalRequest, queued: usize) {
        let w = self.term_width;
        self.enqueue(history::approval_rows(
            &req.tool_name,
            &req.args_preview,
            &req.reason,
            queued,
            w as usize,
            &self.theme,
        ));
    }

    /// Print one finished tool row group straight to scrollback (live
    /// streaming, ADR-0005). `git_diff` output renders as a patch row with
    /// `+N −M` counts + tinted bands; every other tool renders the flat
    /// label + summary + elided-output rows. Reasoning is hidden entirely.
    fn stream_tool(&mut self, ev: &ToolEvent) {
        let w = self.term_width as usize;
        let rows = match ev.name.as_str() {
            "git_diff" => {
                let path = ev.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                history::patch_row("Diff", path, &ev.output, w, &self.theme)
            }
            _ => history::tool_row(
                &ev.name,
                &ev.arg_preview(),
                ev.ok,
                &ev.summary,
                &ev.output,
                w,
            ),
        };
        self.streamed_tools += 1;
        self.enqueue(rows);
        self.dirty = true;
    }

    /// Finish a completed turn: stream any tools the 80ms poll hasn't
    /// forwarded yet, then the reply (or error), then the `Done` trailer.
    /// `started` is the turn's `busy_since`; the trailer carries elapsed.
    fn finish_turn(
        &mut self,
        events: &[ToolEvent],
        result: &Result<String, String>,
        started: Instant,
    ) {
        let w = self.term_width as usize;
        for ev in events {
            self.stream_tool(ev);
        }
        match result {
            Ok(text) => {
                let body = match text.trim().is_empty() {
                    true => "(empty reply)",
                    false => text,
                };
                self.enqueue(history::reply_rows(body, w, &self.theme));
            }
            Err(e) => {
                self.enqueue(history::error_row(&format!("[error] {e}"), w));
            }
        }
        if !events.is_empty() && result.is_ok() {
            let n = events.len();
            self.enqueue_trailer(format!(
                "Done ({} · {} tool{})",
                fmt_elapsed(started.elapsed()),
                n,
                if n == 1 { "" } else { "s" }
            ));
        }
        self.streamed_tools = 0;
        self.dirty = true;
    }
    fn replay_json(&mut self, json: &str) {
        let Ok(mut ctx) = crate::context::Context::from_json(json) else { return };
        let msgs = std::mem::take(ctx.messages_mut());
        self.replay_messages(&msgs);
    }
    fn replay_messages(&mut self, msgs: &[Message]) {
        for m in msgs {
            match m {
                Message::System { .. } => {}
                Message::User { content } => {
                    let mut texts = Vec::new();
                    let mut had_tool = false;
                    for c in content {
                        match c {
                            UserContent::Text(t) if t.text.starts_with("Prior conversation summary:") => self.enqueue_notice(t.text.clone()),
                            UserContent::Text(t) => texts.push(t.text.clone()),
                            UserContent::ToolResult(r) => {
                                had_tool = true;
                                let parts: Vec<&str> = r.content.iter().filter_map(|c| c.as_text()).collect();
                                let output = parts.join("\n");
                                let summary: String = output.lines().next().unwrap_or("").chars().take(120).collect();
                                let w = self.term_width as usize;
                                self.enqueue(crate::history::tool_row(&r.name, "", true, &summary, &output, w));
                            }
                            _ => {}
                        }
                    }
                    let body = texts.join("\n");
                    if !had_tool && !body.trim().is_empty() { self.enqueue_user(&body); }
                }
                Message::Assistant { content, .. } => {
                    let mut body = String::new();
                    for c in content {
                        if let AssistantContent::Text(t) = c { body.push_str(&t.text); }
                    }
                    if !body.trim().is_empty() {
                        let w = self.term_width as usize;
                        self.enqueue(crate::history::reply_rows(&body, w, &self.theme));
                    }
                }
            }
        }
    }
}

/// Main loop: drains turn/live/approval channels, prints finished rows
/// above the bottom-anchored pane, renders it, and routes input.
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
    // Track the viewport width so row groups wrap correctly; refreshed
    // every frame from the terminal size. Stores the REAL width for
    // wrapping; any layout floor stays at the render site, never written
    // back here (Phase 2/3).
    app.term_width = terminal.size().map(|s| s.width).unwrap_or(80).max(1);
    // Seed transcript: welcome notice prints above the pane on first frame.
    app.dirty = true;
    loop {
        // Drain live tool observations: each resolved tool streams its row
        // group straight to scrollback (ADR-0005). Gated on `busy`: a
        // ThinkMsg already in flight when the watcher aborts must not
        // print after `abort_turn` synthesized the trailer.
        while let Ok(msg) = think_rx.try_recv() {
            if app.busy {
                // Live feed carries no output (only resolved on completion);
                // stream the label + summary rows now, exactly once.
                let w = app.term_width as usize;
                app.enqueue(history::tool_row(
                    &msg.name,
                    &msg.preview,
                    msg.ok,
                    &msg.summary,
                    "",
                    w,
                ));
                app.streamed_tools += 1;
                app.dirty = true;
            }
        }
        // Drain approval requests into the modal queue. The agent worker is
        // parked on its oneshot; the same run resumes on resolve.
        // Approval rows print into scrollback the moment the modal owns
        // the keys (ADR-0005); the pane underneath keeps the status row.
        while let Ok(req) = approval_rx.try_recv() {
            let queued = app.pending_approvals.len() + 1;
            app.enqueue_approval_rows(&req, queued);
            app.pending_approvals.push_back(req);
            app.dirty = true;
        }
        // Drain completed turns without blocking the UI.
        // D2: a `tx.send` that wins the send-then-abort race arrives with a
        // stale `seq`; drop it so an aborted turn never replays as complete.
        // Live-streamed tools already printed; `finish_turn` streams only
        // the tools the 80ms poll missed, then reply + trailer.
        while let Ok(turn) = rx.try_recv() {
            if turn.seq != app.turn_generation {
                continue;
            }
            app.current_turn.take();
            if let Some(watcher) = app.current_watcher.take() {
                watcher.abort();
            }
            let started = app.busy_since;
            app.busy = false;
            // Full-fidelity completion: finish_turn prints every tool in
            // the final event list (authoritative output included), then
            // reply + trailer. Live rows already in scrollback stay as the
            // in-progress record; the completed rows are the final record.
            app.streamed_tools = 0;
            app.finish_turn(&turn.events, &turn.result, started);
            if turn.result.is_ok() {
                if let Ok(msgs) = agent.export_sync() {
                    if let Ok(conn) = crate::sessions::open() {
                        let _ = crate::sessions::save_messages(&conn, &app.session_id, &msgs);
                        let _ = crate::sessions::touch(&conn, &app.session_id);
                        // Fallback title from user text; TODO: LLM generate_title (needs async).
                        if app.session_title.is_empty() || app.session_title == "untitled" {
                            if let Some(first) = app.history.iter().rev().find(|h| !h.starts_with('/')) {
                                let t: String = first.chars().take(40).collect();
                                if !t.is_empty() { let _ = crate::sessions::update_title(&conn, &app.session_id, &t); app.session_title = t; }
                            }
                        }
                    }
                }
            }
        }

        // Print queued transcript groups above the viewport first, so the
        // pane draw below never overlaps them.
        drain_print_queue(terminal, app).context("print transcript")?;

        if app.dirty || app.busy || !app.pending_approvals.is_empty() {
            // Refresh width: a resize between frames re-wraps future rows.
            // Already-queued groups wrapped at enqueue width; rows are
            // short-lived (one frame) so drift is bounded to a frame.
            // Store the real width (no `.max(20)` lie); any layout floor
            // stays local to the render site, never written back here.
            if let Ok(size) = terminal.size() {
                app.term_width = size.width.max(1);
                app.set_available_height(size.height);
                resize_pane_viewport(
                    terminal,
                    || CrosstermBackend::new(io::stdout()),
                    size.width,
                    size.height,
                    app.pane_height(size.width),
                )
                .context("resize composer viewport")?;
            }
            terminal
                .draw(|f| render_pane(f, app))
                .context("draw frame")?;
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
                // No mouse handling (ADR-0005): the transcript is terminal
                // scrollback, so the terminal keeps selection and copy.
                Event::Resize(w, h) => {
                    // Use event dimensions directly and redraw immediately.
                    app.term_width = w.max(1);
                    app.set_available_height(h);
                    resize_pane_viewport(
                        terminal,
                        || CrosstermBackend::new(io::stdout()),
                        w,
                        h,
                        app.pane_height(w),
                    )
                    .context("resize composer viewport")?;
                    // Freeze transcript insertion for one frame after resize.
                    app.skip_drain_once = true;
                    terminal
                        .draw(|f| render_pane(f, app))
                        .context("draw on resize")?;
                    app.dirty = false;
                }
                _ => {}
            }
        }
    }
}

/// Print every queued transcript group above the bottom-anchored viewport.
/// Each group is one `history::` row block; `insert_before` scrolls it
/// into real scrollback. No-op when the queue is empty. A pending
/// `/clear` wipes the screen + scrollback first (like Aster's `clear_all`).
fn drain_print_queue(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    // Skip `insert_before` for one frame after a terminal resize so the
    // transcript drain cannot race viewport repositioning.
    if app.skip_drain_once {
        app.skip_drain_once = false;
        return Ok(());
    }
    if app.request_clear_screen {
        app.request_clear_screen = false;
        terminal.clear().context("clear screen")?;
    }
    for rows in app.print_queue.drain(..) {
        let height = rows.len().min(u16::MAX as usize) as u16;
        if height == 0 {
            continue;
        }
        terminal
            .insert_before(height, |buf| {
                Paragraph::new(rows).render(Rect::new(0, 0, buf.area.width, height), buf);
            })
            .context("insert transcript rows")?;
    }
    Ok(())
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
                // Task 2 / ADR-0006: modal Esc is AbortTurn, same path
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
    // Session picker owns keys while open (Phase 3b).
    if app.session_picker.is_some() {
        match code {
            KeyCode::Up => {
                if let Some(p) = app.session_picker.as_mut() {
                    let n = p.items.len().max(1);
                    p.sel = (p.sel + n - 1) % n;
                }
            }
            KeyCode::Down => {
                if let Some(p) = app.session_picker.as_mut() {
                    let n = p.items.len().max(1);
                    p.sel = (p.sel + 1) % n;
                }
            }
            KeyCode::Esc => {
                app.session_picker = None;
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                let show_all = !app.session_picker.as_ref().map(|p| p.show_all).unwrap_or(false);
                let root = picker_project_root();
                match crate::sessions::open() {
                    Err(e) => app.enqueue_notice(format!("sessions: {e}")),
                    Ok(conn) => {
                        let res = if show_all { crate::sessions::list_all(&conn) } else { crate::sessions::list_for_project(&conn, &root) };
                        match res {
                            Err(e) => app.enqueue_notice(format!("sessions: {e}")),
                            Ok(items) => {
                                app.session_picker = Some(SessionPicker { items, sel: 0, show_all });
                            }
                        }
                    }
                }
            }
            KeyCode::Enter => {
                let picked = app.session_picker.as_ref().and_then(|p| p.items.get(p.sel).map(|s| (s.id.clone(), s.title.clone())));
                app.session_picker = None;
                match picked {
                    None => app.enqueue_notice("no session selected".to_string()),
                    Some((id, title)) => match crate::sessions::open() {
                        Err(e) => app.enqueue_notice(format!("resume failed: {e}")),
                        Ok(conn) => match crate::sessions::get_session(&conn, &id) {
                            Ok(Some(s)) => {
                                app.session_id = s.id.clone();
                                app.session_title = s.title.clone();
                                match agent.import_sync(&s.messages_json) {
                                    Ok(()) => { app.replay_json(&s.messages_json); app.enqueue_notice(format!("resumed {title}")) },
                                    Err(e) => app.enqueue_notice(format!("resumed {title} (history import failed: {e})")),
                                }
                            }
                            Ok(None) => app.enqueue_notice(format!("no session {id}")),
                            Err(e) => app.enqueue_notice(format!("resume failed: {e}")),
                        },
                    },
                }
            }
            _ => {}
        }
        return false;
    }
    // Slash-menu branch (ADR-0007): menu owns keys when open.
    if is_menu_open(app) {
        match code {
            KeyCode::Up => {
                let n = menu_row_count_app(app);
                if n > 0 {
                    let cur = app.menu_sel.unwrap_or(0) % n;
                    app.menu_sel = Some((cur + n - 1) % n);
                }
                return false;
            }
            KeyCode::Down => {
                let n = menu_row_count_app(app);
                if n > 0 {
                    let cur = app.menu_sel.unwrap_or(0) % n;
                    app.menu_sel = Some((cur + 1) % n);
                }
                return false;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let matches = menu_matches(&app.input);
                if !matches.is_empty() {
                    let idx = app.menu_sel.unwrap_or(0).min(matches.len() - 1);
                    app.input = format!("/{}", matches[idx].name);
                    app.cursor = app.input.chars().count();
                    clamp_menu_sel(app);
                } else {
                    // Theme-arg mode (ADR-0010): complete `/theme <partial>`
                    // to the highlighted theme name.
                    let names = theme_arg_matches(&app.input);
                    if !names.is_empty() {
                        let idx = app.menu_sel.unwrap_or(0).min(names.len() - 1);
                        app.input = format!("/theme {}", names[idx]);
                        app.cursor = app.input.chars().count();
                        clamp_menu_sel(app);
                    } else {
                        // Skill-arg mode: complete `/skill:<partial>` to the
                        // highlighted skill name as invokable text.
                        let names = skill_arg_matches_for(&app.input, &app.skill_names);
                        if !names.is_empty() {
                            let idx = app.menu_sel.unwrap_or(0).min(names.len() - 1);
                            app.input = format!("/skill:{}", names[idx]);
                            app.cursor = app.input.chars().count();
                            clamp_menu_sel(app);
                        }
                    }
                }
                return false;
            }
            KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => {
                insert_newline(app);
                return false;
            }
            KeyCode::Enter => return submit(app, agent, tx, think_tx),
            KeyCode::Esc => {
                app.menu_sel = None;
                if app.busy {
                    abort_turn(app);
                }
                return false;
            }
            _ => {}
        }
    }
    // Esc (Task 2 / ADR-0006, scrollback edition): busy interrupts, idle
    // is a no-op. No selection exists to deselect; no Esc sequence quits.
    if code == KeyCode::Esc {
        if app.busy {
            abort_turn(app);
        }
        return false;
    }
    match code {
        KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => {
            insert_newline(app);
        }
        KeyCode::Enter => return submit(app, agent, tx, think_tx),
        // Tab is unbound in the scrollback model (ADR-0005): no block
        // selection exists. Kept as a no-op so the key stays free.
        KeyCode::BackTab | KeyCode::Tab if mods.contains(KeyModifiers::SHIFT) => {
            let next = app.mode.next();
            app.mode = next;
            if let Ok(mut mode) = agent.mode_handle().lock() {
                *mode = next;
            }
            app.enqueue_notice(format!("permission mode: {}", next.name()));
        }
        KeyCode::Tab | KeyCode::BackTab => {}
        KeyCode::Backspace => {
            if app.cursor > 0 {
                app.cursor -= 1;
                remove_char_at(&mut app.input, app.cursor);
            }
            clamp_menu_sel(app);
        }
        KeyCode::Delete => {
            remove_char_at(&mut app.input, app.cursor);
            clamp_menu_sel(app);
        }
        KeyCode::Left => {
            app.cursor = app.cursor.saturating_sub(1);
        }
        KeyCode::Right => {
            app.cursor = (app.cursor + 1).min(app.input.chars().count());
        }
        KeyCode::Home => {
            app.cursor = line_edge_cursor(
                &app.input,
                app.cursor,
                (app.term_width as usize).saturating_sub(4),
                false,
            );
        }
        KeyCode::End => {
            app.cursor = line_edge_cursor(
                &app.input,
                app.cursor,
                (app.term_width as usize).saturating_sub(4),
                true,
            );
        }
        KeyCode::Up => {
            move_cursor_at_edge(app, true);
        }
        KeyCode::Down => {
            move_cursor_at_edge(app, false);
        }
        KeyCode::PageUp | KeyCode::PageDown => {}
        KeyCode::Char(c) => {
            insert_char_at(&mut app.input, &mut app.cursor, c);
            clamp_menu_sel(app);
        }
        _ => {}
    }
    false
}

fn handle_ctrl(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('o') => {
            insert_newline(app);
            false
        }
        // Task 4 / ADR-0006: Ctrl+C is strict clear-only — clears the
        // input line, never interrupts, never quits (quit is Ctrl+D-on-empty
        // or /quit). Ignored in the modal via the CONTROL early-return in
        // `handle_key` above.
        KeyCode::Char('c') => {
            app.input.clear();
            app.cursor = 0;
            clamp_menu_sel(app);
            false
        }
        KeyCode::Char('u') => {
            app.input.clear();
            app.cursor = 0;
            clamp_menu_sel(app);
            false
        }
        KeyCode::Char('w') => {
            delete_word_before(&mut app.input, &mut app.cursor);
            clamp_menu_sel(app);
            false
        }
        KeyCode::Char('d') => {
            // Task 3 / ADR-0006 (D3): Ctrl+D quits only when the input
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
            // empty input so there is nothing to push.
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

/// `/theme` dispatch: no arg lists available themes + current; with a
/// name loads `<name>.toml`, swaps the live theme, and persists it so the
/// next launch becomes the default. Unknown names and load failures
/// queue an error notice — never a crash.
fn handle_theme_command(app: &mut App, arg: &str) {
    if arg.is_empty() {
        let list = match Theme::list_themes() {
            Ok(names) => names,
            Err(e) => {
                app.enqueue_notice(format!("theme: cannot list themes: {e:#}"));
                return;
            }
        };
        if list.is_empty() {
            app.enqueue_notice(format!(
                "theme: no themes installed (current: {}).",
                app.theme.name
            ));
        } else {
            app.enqueue_notice(format!(
                "themes (current: {}):\n  {}",
                app.theme.name,
                list.join("\n  ")
            ));
        }
        return;
    }
    match Theme::load_named(arg) {
        Ok(theme) => {
            let name = theme.name.clone();
            app.theme = theme;
            if let Err(e) = Theme::save_active(&name) {
                app.enqueue_notice(format!(
                    "theme: switched to \"{name}\" but could not persist: {e:#}"
                ));
            } else {
                app.enqueue_notice(format!("theme: switched to \"{name}\"."));
            }
        }
        Err(e) => {
            app.enqueue_notice(format!(
                "theme: unknown theme \"{arg}\" ({e:#}). Try /theme."
            ));
        }
    }
}

/// Expand leading `/skill:name` mentions (up to `MAX_STACKED`) into full
/// `SKILL.md` bodies with `$ARGUMENTS` substitution. Returns `None` when the
/// input is not a skill invoke. `Err` names the first unknown skill.
/// The display short form is the original text (transcript shows `/skill:x`).
fn expand_skill_invokes(
    text: &str,
    agent: &Arc<impl AgentLoop + Send + Sync + 'static>,
) -> Result<Option<(String, String)>, String> {
    let (names, task) = crate::skills::split_leading_mentions(text);
    if names.is_empty() {
        return Ok(None);
    }
    let args = crate::skills::split_args(&task);
    let mut bodies = Vec::with_capacity(names.len());
    for name in &names {
        match agent.get_skill_body(name) {
            Some(body) => bodies.push(crate::skills::substitute_args(&body, &task, &args)),
            None => return Err(format!("no skill '{name}' (see /skills)")),
        }
    }
    let mut expanded = bodies.join("\n\n");
    if !task.trim().is_empty() {
        expanded.push_str("\n\n");
        expanded.push_str(task.trim());
    }
    Ok(Some((text.to_string(), expanded)))
}

fn submit(
    app: &mut App,
    agent: &Arc<impl AgentLoop + Send + Sync + 'static>,
    tx: &mpsc::Sender<TurnResult>,
    think_tx: &mpsc::Sender<ThinkMsg>,
) -> bool {
    let mut text = app.input.trim().to_string();
    if text.is_empty() || app.busy {
        return false;
    }
    // Skill-arg accept (mirrors theme-arg accept below): `/skill:<partial>`
    // with suggestions open resolves the highlighted skill to invokable
    // text before expansion, so Enter picks a suggestion. Runs first — an
    // unresolved partial would otherwise fail expansion as unknown.
    if menu_matches(&text).is_empty()
        && skill_arg_partial(&text).is_some()
        && !text.contains(char::is_whitespace)
    {
        let names = skill_arg_matches_for(&text, &app.skill_names);
        if !names.is_empty() {
            let idx = app.menu_sel.unwrap_or(0).min(names.len() - 1);
            text = format!("/skill:{}", names[idx]);
        }
    }
    // Skill invoke (`/skill:name ...`, possibly stacked): expand to full
    // bodies before any menu/parse logic. Transcript keeps the short form
    // (mirrors `/init` display handling below).
    let mut skill_display: Option<String> = None;
    match expand_skill_invokes(&text, agent) {
        Ok(Some((display, expanded))) => {
            skill_display = Some(display);
            text = expanded;
        }
        Ok(None) => {}
        Err(e) => {
            app.input.clear();
            app.cursor = 0;
            app.menu_sel = None;
            app.enqueue_notice(format!("skill: {e}"));
            return false;
        }
    }
    // Bare `/skill:` with no name (not a leading mention) → usage.
    if skill_display.is_none() && text.starts_with("/skill:") {
        app.input.clear();
        app.cursor = 0;
        app.menu_sel = None;
        app.enqueue_notice("usage: /skill:<name> [task]".to_string());
        return false;
    }
    let skill_turn = skill_display.is_some();
    // Theme-arg accept (ADR-0010, mirrors the command prefix-run below):
    // `/theme <partial>` with suggestions open resolves the highlighted
    // theme name first, so Enter picks a suggestion. Bare `/theme` and
    // unmatched args fall through to normal dispatch.
    if menu_matches(&text).is_empty() {
        let names = theme_arg_matches(&text);
        if !names.is_empty() {
            let idx = app.menu_sel.unwrap_or(0).min(names.len() - 1);
            text = format!("/theme {}", names[idx]);
        }
    }
    // Prefix-run (ADR-0007): a menu-eligible prefix resolves to the
    // highlighted match before dispatch, so `/c` + Enter runs `/clear`.
    if !menu_matches(&text).is_empty() {
        let matches = menu_matches(&text);
        let idx = app.menu_sel.unwrap_or(0).min(matches.len() - 1);
        text = format!("/{}", matches[idx].name);
    }
    app.input.clear();
    app.cursor = 0;
    app.menu_sel = None;
    // Skill turns record the short `/skill:x` form, not the expansion.
    app.history.push(skill_display.clone().unwrap_or_else(|| text.clone()));
    app.hist_idx = None;

    if let Some((cmd, arg)) = parse_command(&text) {
        match cmd.name {
            "quit" => return true,
            "clear" => {
                app.request_clear_screen = true;
                app.enqueue_notice("cleared.".to_string());
                return false;
            }
            "help" => {
                let mut msg = String::from("commands:");
                for c in COMMANDS {
                    let usage = if c.takes_arg { " <arg>" } else { "" };
                    msg.push_str(&format!("\n  /{}{usage} — {}", c.name, c.desc));
                }
                app.enqueue_notice(msg);
                return false;
            }
            "theme" => {
                handle_theme_command(app, &arg);
                return false;
            }
            "init" => {
                // ADR-0009: rewrite to the fixed init instruction and fall
                // through to the normal turn path below.
                text = INIT_PROMPT.to_string();
            }
            "resume" => {
                if arg.is_empty() {
                    match crate::sessions::open() {
                        Err(e) => app.enqueue_notice(format!("sessions: {e}")),
                        Ok(conn) => {
                            let root = picker_project_root();
                            match crate::sessions::list_for_project(&conn, &root) {
                                Err(e) => app.enqueue_notice(format!("sessions: {e}")),
                                Ok(items) => {
                                    if items.is_empty() {
                                        app.enqueue_notice("no sessions for this project (a: all)".to_string());
                                    }
                                    app.session_picker = Some(SessionPicker { items, sel: 0, show_all: false });
                                }
                            }
                        }
                    }
                    return false;
                }
                match crate::sessions::open() {
                    Err(e) => {
                        app.enqueue_notice(format!("resume failed: {e}"));
                    }
                    Ok(conn) => match crate::sessions::get_session(&conn, &arg) {
                        Ok(Some(s)) => {
                            app.session_id = s.id.clone();
                            app.session_title = s.title.clone();
                            match agent.import_sync(&s.messages_json) {
                                Ok(()) => { app.replay_json(&s.messages_json); app.enqueue_notice(format!("resumed {}", s.title)) },
                                Err(e) => app.enqueue_notice(format!("resumed {} (history import failed: {e})", s.title)),
                            }
                        }
                        Ok(None) => {
                            app.enqueue_notice(format!("no session {arg}"));
                        }
                        Err(e) => {
                            app.enqueue_notice(format!("resume failed: {e}"));
                        }
                    },
                }
                return false;
            }
            "rename" => {
                if arg.is_empty() {
                    app.enqueue_notice("usage: /rename <name>".to_string());
                    return false;
                }
                match crate::sessions::open() {
                    Err(e) => {
                        app.enqueue_notice(format!("rename failed: {e}"));
                    }
                    Ok(conn) => match crate::sessions::update_title(&conn, &app.session_id, &arg) {
                        Ok(()) => {
                            app.session_title = arg.clone();
                            app.enqueue_notice(format!("renamed to {arg}"));
                        }
                        Err(e) => {
                            app.enqueue_notice(format!("rename failed: {e}"));
                        }
                    },
                }
                return false;
            }
            "fork" => {
                let msgs = match agent.export_sync() { Ok(m) => m, Err(e) => { app.enqueue_notice(format!("fork failed: {e}")); return false; } };
                let conn = match crate::sessions::open() { Ok(c) => c, Err(e) => { app.enqueue_notice(format!("fork failed: {e}")); return false; } };
                match crate::sessions::create_session(&conn, &picker_project_root(), &agent.model_name()) {
                    Err(e) => app.enqueue_notice(format!("fork failed: {e}")),
                    Ok(ns) => { let title = format!("{} (fork)", app.session_title); let _ = crate::sessions::save_messages(&conn, &ns.id, &msgs); let _ = crate::sessions::update_title(&conn, &ns.id, &title); app.session_id = ns.id.clone(); app.session_title = title.clone(); app.enqueue_notice(format!("forked as {title}")); }
                }
                return false;
            }
            "skills" => {
                if arg.trim() == "reload" {
                    if app.busy {
                        app.enqueue_notice("skills: busy — reload after the turn finishes".to_string());
                        return false;
                    }
                    match agent.reload_skills_sync() {
                        Ok(msg) => {
                            app.skill_names = agent.list_skills().iter().map(|s| s.name.clone()).collect();
                            app.enqueue_notice(format!("skills: {msg}"));
                        }
                        Err(e) => app.enqueue_notice(format!("skills reload failed: {e}")),
                    }
                    return false;
                }
                if !arg.trim().is_empty() {
                    app.enqueue_notice("usage: /skills [reload]".to_string());
                    return false;
                }
                let list = agent.list_skills();
                if list.is_empty() {
                    app.enqueue_notice("no skills installed (project .agents/skills, ~/.config/rem/skills, ~/.agents/skills)".to_string());
                } else {
                    let mut msg = String::from("skills:");
                    for s in list {
                        let note = s.note.map(|n| format!(" [{n}]")).unwrap_or_default();
                        msg.push_str(&format!("\n  /skill:{} — {}{} — {}", s.name, s.description, note, s.path));
                    }
                    app.enqueue_notice(msg);
                }
                return false;
            }
            _ => {}
        }
    }
    if text.starts_with('/') && !skill_turn {
        app.enqueue_notice(format!("unknown command \"{text}\". Try /help."));
        return false;
    }

    // `/init` and `/skill:x` show the short command in the transcript while
    // the agent receives the full expanded instruction.
    let display_owned;
    let display = if let Some(short) = skill_display.as_deref() {
        display_owned = short.to_string();
        display_owned.as_str()
    } else if text.as_str() == INIT_PROMPT {
        "/init"
    } else {
        text.as_str()
    };
    app.enqueue_user(display);
    app.busy = true;
    app.busy_since = Instant::now();
    app.streamed_tools = 0;

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
        let _ = tx.send(TurnResult {
            result,
            events,
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
    clamp_menu_sel(app);
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
    fn set_available_height(&mut self, height: u16) {
        self.max_visible_input_lines = height
            .saturating_sub(PANE_ROWS - 1)
            .clamp(1, MAX_INPUT_LINES as u16) as usize;
    }

    fn visible_input_lines(&self, term_width: u16) -> usize {
        input_layout(&self.input, (term_width as usize).saturating_sub(4))
            .rows
            .len()
            .min(self.max_visible_input_lines)
            .max(1)
    }

    fn pane_height(&self, term_width: u16) -> u16 {
        let input_extra = self.visible_input_lines(term_width).saturating_sub(1) as u16;
        let menu_extra = if is_menu_open(self) {
            // App-aware count: command/theme rows plus `/skill:` suggestions
            // from the cache. The input-only count misses skills and leaves
            // the pane ungrown (~2 squeezed rows).
            menu_row_count_app(self)
                .min(MENU_PREFERRED_ROWS)
                .min(MENU_MAX_ROWS) as u16
        } else {
            0
        };
        PANE_ROWS
            .saturating_add(input_extra)
            .saturating_add(menu_extra)
    }

    fn input_band_height(&self, term_width: u16, menu_open: bool) -> u16 {
        let lines = self.visible_input_lines(term_width) as u16;
        lines + u16::from(!(menu_open && lines == 1)) + 1
    }

    fn sync_input_scroll(&mut self, max_width: usize) {
        let layout = input_layout(&self.input, max_width);
        let visible = layout.rows.len().min(self.max_visible_input_lines).max(1);
        let caret_row = layout.positions[self.cursor.min(layout.positions.len() - 1)].0;
        let max_scroll = layout.rows.len().saturating_sub(visible);
        self.input_scroll = self.input_scroll.min(max_scroll);
        if caret_row < self.input_scroll {
            self.input_scroll = caret_row;
        } else if caret_row >= self.input_scroll + visible {
            self.input_scroll = caret_row + 1 - visible;
        }
    }
}

struct InputLayout {
    rows: Vec<String>,
    positions: Vec<(usize, usize)>,
}

fn input_layout(input: &str, max_width: usize) -> InputLayout {
    let max_width = max_width.max(1);
    let chars: Vec<char> = input.chars().collect();
    let mut rows = vec![String::new()];
    let mut positions = vec![(0, 0); chars.len() + 1];
    let mut row = 0;
    let mut column = 0;

    for (index, ch) in chars.iter().copied().enumerate() {
        if ch == '\n' {
            positions[index] = (row, column);
            rows.push(String::new());
            row += 1;
            column = 0;
            positions[index + 1] = (row, column);
            continue;
        }

        let width = UnicodeWidthStr::width(ch.to_string().as_str());
        if column > 0 && column + width > max_width {
            rows.push(String::new());
            row += 1;
            column = 0;
        }
        positions[index] = (row, column);
        rows[row].push(ch);
        column += width;
        positions[index + 1] = (row, column);
    }

    InputLayout { rows, positions }
}

fn move_cursor_vertical(input: &str, cursor: usize, max_width: usize, down: bool) -> Option<usize> {
    let layout = input_layout(input, max_width);
    let (row, column) = layout.positions[cursor.min(layout.positions.len() - 1)];
    let target_row = if down {
        row.checked_add(1)
            .filter(|next| *next < layout.rows.len())?
    } else {
        row.checked_sub(1)?
    };
    layout
        .positions
        .iter()
        .enumerate()
        .filter(|(_, (candidate_row, _))| *candidate_row == target_row)
        .min_by_key(|(_, (_, candidate_column))| candidate_column.abs_diff(column))
        .map(|(index, _)| index)
}

fn line_edge_cursor(input: &str, cursor: usize, max_width: usize, end: bool) -> usize {
    let layout = input_layout(input, max_width);
    let row = layout.positions[cursor.min(layout.positions.len() - 1)].0;
    let mut positions = layout
        .positions
        .iter()
        .enumerate()
        .filter(|(_, (candidate_row, _))| *candidate_row == row)
        .map(|(index, _)| index);
    let first = positions.next().unwrap_or(0);
    if end {
        positions.next_back().unwrap_or(first)
    } else {
        first
    }
}

fn move_cursor_at_edge(app: &mut App, older: bool) {
    if app.input.is_empty() {
        recall_history(app, older);
        return;
    }
    let max_width = (app.term_width as usize).saturating_sub(4);
    let layout = input_layout(&app.input, max_width);
    let row = layout.positions[app.cursor.min(layout.positions.len() - 1)].0;
    let last_row = layout.rows.len().saturating_sub(1);
    if (older && row == 0) || (!older && row == last_row) {
        recall_history(app, older);
    } else if let Some(cursor) = move_cursor_vertical(&app.input, app.cursor, max_width, !older) {
        app.cursor = cursor;
    }
}

fn insert_newline(app: &mut App) {
    insert_char_at(&mut app.input, &mut app.cursor, '\n');
    clamp_menu_sel(app);
}

fn render_overflow_line(
    f: &mut ratatui::Frame,
    area: Rect,
    show_up: bool,
    show_down: bool,
    theme: &Theme,
) {
    if !show_up && !show_down {
        return;
    }
    let left = if show_up { "  ↑ more above" } else { "" };
    let right = if show_down { "↓ more below  " } else { "" };
    let used = UnicodeWidthStr::width(left) + UnicodeWidthStr::width(right);
    let spaces = (area.width as usize).saturating_sub(used);
    let line = Line::from(vec![
        Span::styled(left, Style::default().fg(theme.accent)),
        Span::raw(" ".repeat(spaces)),
        Span::styled(right, Style::default().fg(theme.accent)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn cursor_position(app: &App, area: Rect) -> Position {
    let max_width = (area.width as usize).saturating_sub(4).max(1);
    let layout = input_layout(&app.input, max_width);
    let visible = layout.rows.len().min(app.max_visible_input_lines).max(1);
    let cursor = app.cursor.min(layout.positions.len() - 1);
    let (row, column) = layout.positions[cursor];
    let top_padding = area.height.saturating_sub(visible as u16).saturating_div(2);
    let visible_row = row.saturating_sub(app.input_scroll);
    Position::new(
        area.x
            .saturating_add(3)
            .saturating_add(column.min(max_width) as u16)
            .min(area.right().saturating_sub(1)),
        area.y
            .saturating_add(top_padding)
            .saturating_add(visible_row as u16)
            .min(area.bottom().saturating_sub(1)),
    )
}

fn render_input(f: &mut ratatui::Frame, app: &App, area: Rect) {
    f.render_widget(
        Block::default().style(Style::default().bg(app.theme.pane_bg)),
        area,
    );
    if area.height == 0 || area.width < 8 {
        return;
    }
    let max_width = (area.width as usize).saturating_sub(4).max(1);
    let layout = input_layout(&app.input, max_width);
    let visible = layout.rows.len().min(app.max_visible_input_lines).max(1);
    let top_padding = area.height.saturating_sub(visible as u16) / 2;
    let bottom_padding = area.height.saturating_sub(visible as u16 + top_padding);
    let show_up = app.input_scroll > 0;
    let show_down = app.input_scroll + visible < layout.rows.len();

    if top_padding > 0 {
        render_overflow_line(
            f,
            Rect::new(area.x, area.y, area.width, 1),
            show_up,
            show_down && bottom_padding == 0,
            &app.theme,
        );
    }
    let first = app.input_scroll.min(layout.rows.len().saturating_sub(1));
    for (offset, text) in layout.rows.iter().skip(first).take(visible).enumerate() {
        let prompt = first + offset == 0;
        let prefix = if prompt { " " } else { "   " };
        let mut body = vec![Span::raw(prefix)];
        if prompt {
            body.push(Span::styled(
                "❯ ",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let body = if app.input.is_empty() {
            let hint = match app.busy {
                true => "…  (esc to interrupt)",
                false => "Message rem…  (/ for commands)",
            };
            body.push(Span::styled(
                hint,
                Style::default()
                    .fg(app.theme.placeholder)
                    .add_modifier(Modifier::ITALIC),
            ));
            body
        } else {
            body.push(Span::raw(text.clone()));
            body
        };
        let y = area.y + top_padding + offset as u16;
        f.render_widget(
            Paragraph::new(Line::from(body)),
            Rect::new(area.x, y, area.width, 1),
        );
    }
    if bottom_padding > 0 {
        render_overflow_line(
            f,
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
            show_up && top_padding == 0,
            show_down,
            &app.theme,
        );
    }
}

/// The transcript lives in scrollback above; the viewport holds only the
/// bottom pane, whose height follows the composer's visible line count.
fn render_pane(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    let required_height = app.pane_height(area.width);
    if area.width < 20 || area.height < required_height {
        f.render_widget(
            Paragraph::new(Line::from("terminal too small — resize to continue")),
            area,
        );
        return;
    }
    let menu_open = is_menu_open(app);
    let menu_h = menu_height(app);
    let status_h = menu_status_height(app);
    let input_h = app.input_band_height(area.width, menu_open);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(menu_gap_height(app)), // gap (terminal bg)
            Constraint::Length(status_h),             // status (busy/approval/idle)
            Constraint::Length(menu_h),               // slash-menu popup (0 when closed)
            Constraint::Length(input_h),              // shaded input band
            Constraint::Length(1),                    // footer
        ])
        .split(area);

    render_gap(f, chunks[0]);
    render_status(f, app, chunks[1]);
    render_menu(f, app, chunks[2]);
    app.sync_input_scroll((area.width as usize).saturating_sub(4));
    render_input(f, app, chunks[3]);
    render_footer(f, app, chunks[4]);
    // Approval modal last: bottom-anchored sheet over the pane.
    if app.pending_approvals.front().is_some() {
        let queued = app.pending_approvals.len();
        // Clone the head request (oneshot sender is not Clone, so rebuild
        // a display-only copy without touching the queue).
        let head = app.pending_approvals.front().expect("checked above");
        let display = ApprovalDisplay {
            tool_name: head.tool_name.clone(),
            args_preview: head.args_preview.clone(),
            reason: head.reason.clone(),
        };
        render_approval_modal(f, f.area(), &display, queued, &app.theme);
    }
    if app.session_picker.is_some() {
        render_session_picker(f, app, f.area(), &app.theme);
    }
    // Keep the caret on the composer's text row in the shaded band.
    // `cursor_position` is the multiline-aware helper (dev): it accounts for
    // the resized composer and input scroll, which the sessions-side
    // `cursor_x` single-line path did not.
    f.set_cursor_position(cursor_position(app, chunks[3]));
}

/// Bottom-anchored approval sheet: shaded band with the request + key hints.
/// The request rows already printed into scrollback when the modal took
/// over; this sheet is the live decision surface. `handle_key` blocks input
/// routing until resolved.
/// Display-only snapshot of an approval head for the modal sheet.
/// (The live `ApprovalRequest` owns a oneshot sender and cannot be cloned.)
struct ApprovalDisplay {
    tool_name: String,
    args_preview: String,
    reason: String,
}

fn render_approval_modal(
    f: &mut ratatui::Frame,
    area: Rect,
    req: &ApprovalDisplay,
    queued: usize,
    theme: &Theme,
) {
    // Bottom sheet: 5 shaded rows over gap + status + input band; footer
    // stays visible underneath with the model/turn line.
    let h = 5u16.min(area.height.saturating_sub(1));
    let sheet = Rect::new(area.x, area.y, area.width, h);
    f.render_widget(
        Block::default().style(Style::default().bg(theme.pane_bg)),
        sheet,
    );
    if sheet.height < 5 || sheet.width < 20 {
        return;
    }
    let inner = Rect::new(sheet.x + 2, sheet.y, sheet.width.saturating_sub(2), h);
    let arg_line = match req.args_preview.chars().count() > inner.width as usize {
        true => format!(
            "{}…",
            req.args_preview
                .chars()
                .take(inner.width as usize - 1)
                .collect::<String>()
        ),
        false => req.args_preview.clone(),
    };
    let mut head: Vec<Span<'static>> = vec![
        Span::styled("◌ ", Style::default().fg(theme.accent)),
        Span::styled(
            "permission — approval needed".to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    if queued > 1 {
        head.push(Span::styled(
            format!(" (1 of {queued})"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let lines = vec![
        Line::from(head),
        Line::from(vec![Span::styled(
            format!("{} ({arg_line})", req.tool_name),
            Style::default().fg(Color::Yellow),
        )]),
        Line::from(vec![Span::styled(
            req.reason.clone(),
            Style::default().fg(Color::DarkGray),
        )]),
        Line::from(vec![
            Span::styled("[y] approve  ", Style::default().fg(Color::Green)),
            Span::styled("[a] always  ", Style::default().fg(Color::Green)),
            Span::styled("[n] deny  ", Style::default().fg(Color::Red)),
            Span::styled("[x] abort turn", Style::default().fg(Color::Red)),
        ]),
        Line::from(vec![Span::styled(
            "deny returns feedback so the model replans in the same run.",
            Style::default().fg(Color::DarkGray),
        )]),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn picker_project_root() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn picker_ago(updated_at: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(updated_at);
    let d = (now - updated_at).max(0);
    if d < 60 {
        "just now".to_string()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86400)
    }
}

fn render_session_picker(
    f: &mut ratatui::Frame,
    app: &App,
    area: Rect,
    theme: &Theme,
) {
    let Some(picker) = app.session_picker.as_ref() else { return; };
    let w = (area.width * 60 / 100).clamp(50, area.width.max(1)).min(area.width);
    let h = ((picker.items.len() as u16 + 4).max(12)).min(area.height.max(1));
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let sheet = Rect::new(x, y, w, h);
    f.render_widget(
        Block::default().style(Style::default().bg(theme.pane_bg)),
        sheet,
    );
    if sheet.width < 20 || sheet.height < 6 {
        return;
    }
    let inner = Rect::new(
        sheet.x + 2,
        sheet.y + 1,
        sheet.width.saturating_sub(4),
        sheet.height.saturating_sub(3),
    );
    let scope = if picker.show_all { "all" } else { "this project" };
    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![Span::styled(
        format!("sessions ({scope})"),
        Style::default().add_modifier(Modifier::BOLD),
    )])];
    if picker.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no sessions)".to_string(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    for (i, s) in picker.items.iter().enumerate().take(inner.height.saturating_sub(2) as usize) {
        let short_proj = s.project_root.rsplit('/').next().unwrap_or(s.project_root.as_str()).to_string();
        let id8: String = s.id.chars().take(8).collect();
        let row = format!("{} — {} — {} · {}", s.title, short_proj, picker_ago(s.updated_at), id8);
        let style = if i == picker.sel {
            Style::default().bg(theme.menu_sel_bg)
        } else {
            Style::default().bg(theme.pane_bg)
        };
        lines.push(Line::from(Span::styled(row, style)));
    }
    lines.push(Line::from(Span::styled(
        "enter resume · a all/mine · esc close".to_string(),
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
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
            Span::styled("◌ ", Style::default().fg(app.theme.accent)),
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
        let verb = BUSY_VERBS[(elapsed.as_secs() / BUSY_VERB_SECS) as usize % BUSY_VERBS.len()];
        let line = Line::from(vec![
            Span::styled(format!("{spinner} "), Style::default().fg(app.theme.accent)),
            Span::styled(verb, Style::default().fg(Color::DarkGray)),
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

/// Slash-menu popup directly above the composer (ADR-0007, ADR-0010):
/// themed shaded band, one row per match — selected row highlighted (theme
/// `menu_sel_bg` + bold accent name + `▸` marker), rest dim — capped at
/// `MENU_MAX_ROWS` with a trailing dim `+N more` overflow row. Zero-height
/// chunk when closed. Command mode renders `▸ /name  desc`; theme-arg mode
/// (`/theme <partial>`) renders `▸ <name>` rows instead.
fn render_menu(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let cmds = menu_matches(&app.input);
    let theme_names: Vec<String>;
    let skill_names: Vec<String>;
    let (names, from_themes, from_skills): (&[String], bool, bool) = if cmds.is_empty() {
        theme_names = theme_arg_matches(&app.input);
        if !theme_names.is_empty() || theme_arg_partial(&app.input).is_some() {
            (&theme_names, true, false)
        } else {
            skill_names = skill_arg_matches_for(&app.input, &app.skill_names);
            (&skill_names, false, true)
        }
    } else {
        (&[], false, false)
    };
    if cmds.is_empty() && names.is_empty() {
        return;
    }
    // Single total across modes: mutually exclusive, one mode's count.
    // Skill rows render like theme rows (`▸ <name>`); Tab/Enter insert the
    // full `/skill:<name>` invokable form.
    let total = if from_themes || from_skills { names.len() } else { cmds.len() };
    let theme = &app.theme;
    f.render_widget(
        Block::default().style(Style::default().bg(theme.pane_bg)),
        area,
    );
    let sel = app.menu_sel.map_or(0, |i| i.min(total.saturating_sub(1)));
    let has_overflow = total > area.height as usize;
    let command_rows = if has_overflow {
        area.height.saturating_sub(1) as usize
    } else {
        total.min(MENU_MAX_ROWS)
    };
    let start = if command_rows == 0 {
        0
    } else {
        sel.saturating_sub(command_rows - 1)
            .min(total.saturating_sub(command_rows))
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Theme-arg mode: `▸ <name>` rows (no leading slash, no desc).
    for (i, name) in names.iter().enumerate().skip(start).take(command_rows) {
        let selected = i == sel;
        let row_style = match selected {
            true => Style::default().bg(theme.menu_sel_bg),
            false => Style::default().bg(theme.pane_bg),
        };
        let (marker, name_style) = match selected {
            true => (
                Span::styled(
                    "▸ ",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                        .bg(theme.menu_sel_bg),
                ),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
                    .bg(theme.menu_sel_bg),
            ),
            false => (
                Span::styled("  ", Style::default().bg(theme.pane_bg)),
                Style::default().fg(Color::DarkGray).bg(theme.pane_bg),
            ),
        };
        // Current theme gets a trailing `●` so the list doubles as
        // the "which is active" answer (same info as bare `/theme`).
        let is_current = *name == app.theme.name;
        let label = match (selected, is_current) {
            (_, true) => format!("{name}  ●"),
            _ => name.clone(),
        };
        lines.push(Line::from(vec![marker, Span::styled(label, name_style)]));
        if let Some(line) = lines.last_mut() {
            line.style = row_style;
        }
    }
    if from_themes {
        if has_overflow {
            lines.push(Line::from(vec![Span::styled(
                format!("  +{} more", total - command_rows),
                Style::default().fg(Color::DarkGray).bg(theme.pane_bg),
            )]));
        }
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    for (i, cmd) in cmds.iter().enumerate().skip(start).take(command_rows) {
        let selected = i == sel;
        let row_style = match selected {
            true => Style::default().bg(theme.menu_sel_bg),
            false => Style::default().bg(theme.pane_bg),
        };
        let (marker, name_style) = match selected {
            true => (
                Span::styled(
                    "▸ ",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                        .bg(theme.menu_sel_bg),
                ),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
                    .bg(theme.menu_sel_bg),
            ),
            false => (
                Span::styled("  ", Style::default().bg(theme.pane_bg)),
                Style::default().fg(Color::DarkGray).bg(theme.pane_bg),
            ),
        };
        lines.push(Line::from(vec![
            marker,
            Span::styled(format!("/{}", cmd.name), name_style),
            Span::styled(
                format!("  {}", cmd.desc),
                Style::default().fg(Color::DarkGray).bg(match selected {
                    true => theme.menu_sel_bg,
                    false => theme.pane_bg,
                }),
            ),
        ]));
        if let Some(line) = lines.last_mut() {
            line.style = row_style;
        }
    }
    if has_overflow {
        lines.push(Line::from(vec![Span::styled(
            format!("  +{} more", total - command_rows),
            Style::default().fg(Color::DarkGray).bg(theme.pane_bg),
        )]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Aster-style footer: one left-aligned line — orange `▶▶▶ edit`, faint
/// model, turn count, and key hints. Busy state lives in the status row
/// above the input band, so the footer stays quiet during a turn.
fn render_footer(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let faint = Style::default().fg(Color::DarkGray);
    let spans = vec![
        Span::raw("  "),
        Span::styled(
            format!("▶ {}", app.mode.name()),
            Style::default().fg(app.theme.accent),
        ),
        Span::styled(format!("  ·  {}", app.model), faint),
        Span::styled(format!("  ·  {}", app.effort), faint),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_stays_bottom_anchored_across_terminal_resizes() {
        use ratatui::{Terminal, TerminalOptions, Viewport, backend::TestBackend};

        let mut backend = TestBackend::new(80, 24);
        backend.set_cursor_position(Position::new(0, 18)).unwrap();
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(PANE_ROWS),
            },
        )
        .unwrap();
        let mut app = App::new("test-model".to_string(), "medium".to_string());
        let mut area = Rect::default();

        terminal
            .draw(|f| {
                area = f.area();
                render_pane(f, &mut app);
            })
            .unwrap();
        assert_eq!(area, Rect::new(0, 18, 80, PANE_ROWS));
        resize_pane_viewport(&mut terminal, || TestBackend::new(80, 24), 80, 24, 10).unwrap();
        assert_eq!(terminal.get_frame().area(), Rect::new(0, 14, 80, 10));
        resize_pane_viewport(
            &mut terminal,
            || TestBackend::new(80, 24),
            80,
            24,
            PANE_ROWS,
        )
        .unwrap();

        resize_pane_viewport(
            &mut terminal,
            || TestBackend::new(40, 24),
            40,
            24,
            PANE_ROWS,
        )
        .unwrap();
        terminal
            .draw(|f| {
                area = f.area();
                render_pane(f, &mut app);
            })
            .unwrap();
        assert_eq!(area, Rect::new(0, 18, 40, PANE_ROWS));

        resize_pane_viewport(
            &mut terminal,
            || TestBackend::new(120, 32),
            120,
            32,
            PANE_ROWS,
        )
        .unwrap();
        terminal
            .draw(|f| {
                area = f.area();
                render_pane(f, &mut app);
            })
            .unwrap();
        assert_eq!(area, Rect::new(0, 26, 120, PANE_ROWS));

        resize_pane_viewport(&mut terminal, || TestBackend::new(20, 4), 20, 4, PANE_ROWS).unwrap();
        terminal
            .draw(|f| {
                area = f.area();
                render_pane(f, &mut app);
            })
            .unwrap();
        assert_eq!(area, Rect::new(0, 0, 20, 4));

        resize_pane_viewport(
            &mut terminal,
            || TestBackend::new(80, 24),
            80,
            24,
            PANE_ROWS,
        )
        .unwrap();
        terminal
            .draw(|f| {
                area = f.area();
                render_pane(f, &mut app);
            })
            .unwrap();
        assert_eq!(area, Rect::new(0, 18, 80, PANE_ROWS));
        let mut inserted = false;
        terminal
            .insert_before(1, |buffer| {
                Paragraph::new("transcript").render(Rect::new(0, 0, buffer.area.width, 1), buffer);
                inserted = true;
            })
            .unwrap();
        assert!(inserted, "dynamic inline viewport must preserve scrollback");
    }

    fn cell_text(buf: &ratatui::buffer::Buffer, y: u16, w: u16) -> String {
        (0..w)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    }

    fn pane_buffer(app: &mut App, w: u16) -> ratatui::buffer::Buffer {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(w, app.pane_height(w));
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_pane(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        let agent = Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(app, &agent, &tx, &think_tx, code, modifiers));
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
    fn input_layout_wraps_text_and_tracks_caret_rows() {
        let layout = input_layout("abcdef\n界ghij", 4);
        assert_eq!(layout.rows, ["abcd", "ef", "界gh", "ij"]);
        assert_eq!(layout.positions[7], (2, 0));
        assert_eq!(layout.positions[10], (3, 0));
        assert_eq!(layout.positions[12], (3, 2));
    }

    #[test]
    fn vertical_cursor_moves_across_wrapped_and_explicit_lines() {
        let input = "abcdefghijklmnopq\nlast";
        let start = input.chars().count();
        let up = move_cursor_vertical(input, start, 16, false).unwrap();
        assert_eq!(up, 17);
        assert_eq!(move_cursor_vertical(input, up, 16, true), Some(19));

        let input = "abc\ndefgh";
        assert_eq!(move_cursor_vertical(input, 7, 16, false), Some(3));
        assert_eq!(move_cursor_vertical(input, 3, 16, true), Some(7));
    }

    #[test]
    fn shift_enter_inserts_newline_and_vertical_edges_recall_history() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.term_width = 20;
        app.input = "hello".to_string();
        app.cursor = 5;
        press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(app.input, "hello\n");
        assert_eq!(app.cursor, 6);
        app.history.push(app.input.clone());

        app.input = "first\nsecond".to_string();
        app.cursor = 8;
        press(&mut app, KeyCode::Up, KeyModifiers::empty());
        assert_eq!(app.cursor, 2);
        press(&mut app, KeyCode::Up, KeyModifiers::empty());
        assert_eq!(app.input, "hello\n");
        assert_eq!(app.cursor, 6);
    }

    #[test]
    fn ctrl_o_inserts_newline_when_shift_enter_is_unavailable() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.input = "firstsecond".to_string();
        app.cursor = 5;

        press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);

        assert_eq!(app.input, "first\nsecond");
        assert_eq!(app.cursor, 6);
    }

    #[tokio::test]
    async fn enter_submits_all_composer_lines() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.input = "first line\nsecond line".to_string();
        app.cursor = app.input.chars().count();
        let agent = Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();

        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));

        assert!(app.busy);
        assert_eq!(
            app.history.last().map(String::as_str),
            Some("first line\nsecond line")
        );
        let rendered = app
            .print_queue
            .iter()
            .flatten()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("first line"), "{rendered}");
        assert!(rendered.contains("second line"), "{rendered}");
    }

    #[test]
    fn pane_renders_gap_status_band_footer() {
        let mut app = App::new("test-model".to_string(), "medium".to_string());
        app.term_width = 80;
        let buf = pane_buffer(&mut app, 80);
        // Gap row: terminal background.
        assert_eq!(buf[(0, 0)].bg, ratatui::style::Color::Reset);
        // Status row idle: blank line, terminal background.
        assert_eq!(cell_text(&buf, 1, 80).trim(), "");
        // Input band rows: shaded.
        for y in 2..5 {
            assert_eq!(buf[(0, y)].bg, app.theme.pane_bg, "band row {y}");
        }
        // Prompt + placeholder on the band's middle row.
        let mid = cell_text(&buf, 3, 80);
        assert!(mid.contains('❯'), "prompt missing: {mid}");
        assert!(mid.contains("Message rem…"), "placeholder missing: {mid}");
        // Footer: mode/model/effort line.
        let footer = cell_text(&buf, 5, 80);
        assert!(footer.contains("▶ manual"), "footer missing: {footer}");
        assert!(footer.contains("test-model"), "model missing: {footer}");
        assert!(footer.contains("medium"), "effort missing: {footer}");
        // Caret accounts for the 1-column inset + 2-column prompt.
        app.input = "hello".to_string();
        app.cursor = 5;
        let position = cursor_position(&app, Rect::new(0, 2, 80, 3));
        assert_eq!(position, Position::new(8, 3));
    }

    #[test]
    fn composer_grows_and_shows_overflow_cues() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.term_width = 40;
        app.input = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight".to_string();
        app.cursor = "one\ntwo\nthree\nfour\nfive\n".chars().count() + 1;
        let buffer = pane_buffer(&mut app, 40);
        let rendered = (0..buffer.area.height)
            .map(|y| cell_text(&buffer, y, 40))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(app.pane_height(40), 10);
        assert!(rendered.contains("↑ more above"), "{rendered}");
        assert!(rendered.contains("↓ more below"), "{rendered}");
        assert!(rendered.contains("four"), "{rendered}");
        assert!(!rendered.contains("one"), "{rendered}");
    }

    #[test]
    fn short_terminal_reduces_visible_rows_without_hiding_the_composer() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.term_width = 40;
        app.set_available_height(8);
        app.input = "one\ntwo\nthree\nfour\nfive\nsix".to_string();
        app.cursor = app.input.chars().count();

        let buffer = pane_buffer(&mut app, 40);
        let rendered = (0..buffer.area.height)
            .map(|y| cell_text(&buffer, y, 40))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(app.pane_height(40), 8);
        assert!(rendered.contains("↑ more above"), "{rendered}");
        assert!(rendered.contains("six"), "{rendered}");
        assert!(!rendered.contains("one"), "{rendered}");
    }

    #[test]
    fn busy_status_row_shows_spinner_and_hint() {
        let mut app = App::new("test-model".to_string(), "medium".to_string());
        app.term_width = 80;
        app.busy = true;
        app.busy_since = Instant::now();
        let buf = pane_buffer(&mut app, 80);
        let status = cell_text(&buf, 1, 80);
        assert!(status.contains("working"), "status missing: {status}");
        assert!(
            status.contains("esc to interrupt"),
            "hint missing: {status}"
        );
        // Backdate the task clock: the verb rotates every BUSY_VERB_SECS.
        app.busy_since = Instant::now() - Duration::from_secs(BUSY_VERB_SECS);
        let buf = pane_buffer(&mut app, 80);
        let status = cell_text(&buf, 1, 80);
        assert!(
            status.contains(BUSY_VERBS[1]),
            "verb did not rotate: {status}"
        );
        let mid = cell_text(&buf, 3, 80);
        assert!(mid.contains("esc to interrupt"), "busy hint missing: {mid}");
    }

    #[test]
    fn approval_sheet_renders_over_pane() {
        let mut app = App::new("test-model".to_string(), "medium".to_string());
        app.term_width = 80;
        let (req, _rx) = approval_req("bash", "cargo test");
        app.pending_approvals.push_back(req);
        let buf = pane_buffer(&mut app, 80);
        let text: String = (0..5)
            .map(|y| cell_text(&buf, y, 80))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("approval needed"), "got: {text}");
        assert!(text.contains("cargo test"), "got: {text}");
        assert!(text.contains("[y] approve"), "got: {text}");
        assert!(text.contains("[n] deny"), "got: {text}");
        // Sheet rows carry the shaded background.
        assert_eq!(buf[(0, 0)].bg, app.theme.pane_bg);
    }

    #[test]
    fn finish_turn_streams_tools_reply_and_duration_trailer() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.term_width = 80;
        // One tool already streamed live: only the tail prints.
        app.streamed_tools = 1;
        let ev = |name: &str| crate::agent::ToolEvent {
            name: name.to_string(),
            args: serde_json::json!({}),
            ok: true,
            summary: "s".to_string(),
            output: "out".to_string(),
        };
        let started = Instant::now() - Duration::from_millis(2200);
        let before = app.print_queue.len();
        app.finish_turn(&[ev("read"), ev("bash")], &Ok("done.".to_string()), started);
        // Queued groups: both tools (final record with output) + reply +
        // trailer. Live rows printed mid-turn stay as the in-progress record.
        assert_eq!(app.print_queue.len(), before + 4);
        let new_groups = &app.print_queue[before..];
        let flat: String = new_groups
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("bash"), "got: {flat}");
        assert!(flat.contains("read"), "got: {flat}");
        assert!(flat.contains("done."), "got: {flat}");
        assert!(flat.contains("Done (2.2s · 2 tools)"), "got: {flat}");
        assert_eq!(app.streamed_tools, 0);
    }

    #[test]
    fn finish_turn_error_prints_no_trailer() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.term_width = 80;
        let before = app.print_queue.len();
        app.finish_turn(&[], &Err("boom".to_string()), Instant::now());
        assert_eq!(app.print_queue.len(), before + 1);
    }

    #[test]
    fn stream_tool_counts_for_abort_trailer() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.term_width = 80;
        let ev = crate::agent::ToolEvent {
            name: "bash".to_string(),
            args: serde_json::json!({"command": "ls"}),
            ok: true,
            summary: "s".to_string(),
            output: "o".to_string(),
        };
        let before = app.print_queue.len();
        app.stream_tool(&ev);
        assert_eq!(app.streamed_tools, 1);
        assert_eq!(app.print_queue.len(), before + 1);
    }

    #[test]
    fn git_diff_streams_patch_row_with_counts() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.term_width = 80;
        let ev = crate::agent::ToolEvent {
            name: "git_diff".to_string(),
            args: serde_json::json!({"path": "f.rs"}),
            ok: true,
            summary: "diff".to_string(),
            output: " ctx\n+ add\n- del".to_string(),
        };
        app.stream_tool(&ev);
        let flat: String = app
            .print_queue
            .last()
            .unwrap()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("+1"), "got: {flat}");
        assert!(flat.contains("−1"), "got: {flat}");
    }

    fn approval_req(
        tool: &str,
        preview: &str,
    ) -> (
        ApprovalRequest,
        tokio::sync::oneshot::Receiver<ApprovalDecision>,
    ) {
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
    fn modal_keys_resolve_approval_with_audit_notice() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        let (req, rx) = approval_req("bash", "cargo test");
        app.pending_approvals.push_back(req);
        // Bare keys only: y approves.
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char('y'),
            KeyModifiers::empty()
        ));
        assert!(app.pending_approvals.is_empty());
        assert!(rx.blocking_recv().is_ok());
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("permission approved"), "got: {flat}");
        assert!(flat.contains("cargo test"), "got: {flat}");
    }

    #[test]
    fn modal_deny_and_abort_map_to_decisions() {
        for (key, expect) in [
            (KeyCode::Char('n'), ApprovalDecision::Deny),
            (KeyCode::Char('x'), ApprovalDecision::AbortTurn),
            (KeyCode::Char('a'), ApprovalDecision::ApproveAlways),
        ] {
            let mut app = App::new("model".to_string(), "medium".to_string());
            app.print_queue.clear();
            let (req, rx) = approval_req("write", "a.txt");
            app.pending_approvals.push_back(req);
            let agent = std::sync::Arc::new(StubAgent);
            let (tx, _rx) = mpsc::channel::<TurnResult>();
            let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
            assert!(!handle_key(
                &mut app,
                &agent,
                &tx,
                &think_tx,
                key,
                KeyModifiers::empty()
            ));
            assert_eq!(rx.blocking_recv().unwrap(), expect);
        }
    }

    #[test]
    fn esc_while_busy_aborts_and_queues_interrupted_rows() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.busy = true;
        app.turn_generation = 3;
        app.streamed_tools = 1;
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Esc,
            KeyModifiers::empty()
        ));
        assert!(!app.busy);
        assert!(app.current_turn.is_none());
        assert!(app.current_watcher.is_none());
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("interrupted."), "got: {flat}");
        assert!(flat.contains("Interrupted (1 tool)"), "got: {flat}");
        // D2: generation bumped, so a late TurnResult with seq 3 is stale.
        assert_eq!(app.turn_generation, 4);
    }

    #[test]
    fn abort_turn_without_streamed_tools_marks_interrupted_only() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.busy = true;
        abort_turn(&mut app);
        assert!(!app.busy);
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("interrupted."), "got: {flat}");
        assert!(!flat.contains("Interrupted ("), "got: {flat}");
    }

    #[test]
    fn abort_turn_is_noop_when_idle() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let before = app.print_queue.len();
        abort_turn(&mut app);
        assert_eq!(app.print_queue.len(), before);
        assert_eq!(app.turn_generation, 0);
    }

    #[test]
    fn ctrl_d_with_text_is_noop() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.input = "hi".to_string();
        app.cursor = 2;
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        ));
        assert_eq!(app.input, "hi");
    }

    #[test]
    fn ctrl_d_empty_idle_quits() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        ));
    }

    #[test]
    fn ctrl_d_empty_busy_aborts_then_quits() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.busy = true;
        app.turn_generation = 5;
        app.streamed_tools = 1;
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        ));
        assert!(!app.busy);
        assert_eq!(app.turn_generation, 6);
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("interrupted."), "got: {flat}");
        assert!(flat.contains("Interrupted (1 tool)"), "got: {flat}");
    }

    #[test]
    fn ctrl_d_empty_modal_resolves_abort_then_quits() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.busy = true;
        let (req, rx) = approval_req("bash", "cargo test");
        app.pending_approvals.push_back(req);
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // Modal CONTROL exemption lets Ctrl+D through; head resolves as
        // AbortTurn (D3), busy turn aborts, then quit.
        assert!(handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        ));
        assert_eq!(rx.blocking_recv().unwrap(), ApprovalDecision::AbortTurn);
        assert!(app.pending_approvals.is_empty());
        assert!(!app.busy);
    }

    #[test]
    fn ctrl_c_idle_clears_input_without_quit() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.input = "hello".to_string();
        app.cursor = 5;
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ));
        assert!(app.input.is_empty());
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn ctrl_c_while_busy_clears_only_never_interrupts() {
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // Busy + typed input: clears the line, the turn keeps running.
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.busy = true;
        app.turn_generation = 7;
        app.input = "partial".to_string();
        app.cursor = 7;
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ));
        assert!(app.input.is_empty());
        assert_eq!(app.cursor, 0);
        assert!(app.busy, "Ctrl+C must never interrupt a busy turn");
        assert_eq!(app.turn_generation, 7);
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!flat.contains("interrupted."), "got: {flat}");
        // Busy + empty input: no-op, still no interrupt.
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ));
        assert!(app.busy);
        assert_eq!(app.turn_generation, 7);
    }

    #[test]
    fn ctrl_c_in_modal_is_ignored() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.busy = true;
        app.input = "typed".to_string();
        app.cursor = 5;
        let (req, mut rx) = approval_req("bash", "cargo test");
        app.pending_approvals.push_back(req);
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // Modal CONTROL blanket-ignore: no clear, no resolve, no quit.
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ));
        assert_eq!(app.input, "typed");
        assert_eq!(app.cursor, 5);
        assert_eq!(app.pending_approvals.len(), 1);
        assert!(
            rx.try_recv().is_err(),
            "modal Ctrl+C must not resolve the approval"
        );
        assert!(app.busy);
    }

    #[test]
    fn slash_clear_requests_screen_wipe_and_notice() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.input = "/clear".to_string();
        app.cursor = 6;
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        assert!(app.request_clear_screen);
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("cleared."), "got: {flat}");
    }

    #[test]
    fn slash_unknown_queues_notice() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.input = "/nope".to_string();
        app.cursor = 5;
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("unknown command"), "got: {flat}");
    }

    // ---- ADR-0007 slash-menu regression tests ----

    #[test]
    fn slash_menu_filter_is_prefix_case_sensitive_no_whitespace() {
        assert!(menu_matches("").is_empty());
        assert!(menu_matches("hello /").is_empty());
        assert!(menu_matches(" /").is_empty());
        assert_eq!(menu_matches("/").len(), COMMANDS.len());
        let names = |input: &str| {
            menu_matches(input)
                .iter()
                .map(|c| c.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names("/c"), vec!["clear"]);
        assert_eq!(names("/qu"), vec!["quit"]);
        assert_eq!(names("/h"), vec!["help"]);
        assert!(menu_matches("/C").is_empty());
        assert!(menu_matches("/CLEAR").is_empty());
        assert!(menu_matches("/ ").is_empty());
        assert!(menu_matches("/foo bar").is_empty());
        assert!(menu_matches("/clear ").is_empty());
        assert!(menu_matches("/bogus").is_empty());
    }

    #[test]
    fn slash_menu_open_and_height_follow_matches() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        assert!(!is_menu_open(&app));
        assert_eq!(menu_height(&app), 0);
        app.input = "/".to_string();
        app.cursor = 1;
        assert!(is_menu_open(&app));
        // 9 registry commands, 3-row idle capacity: the menu caps at
        // capacity and spills into a `+N more` overflow row.
        assert_eq!(
            menu_height(&app),
            (COMMANDS.len() as u16).min(menu_row_capacity(&app))
        );
        assert_eq!(MENU_MAX_ROWS, 10);
        app.input = "/bogus".to_string();
        app.cursor = 6;
        assert!(!is_menu_open(&app));
        assert_eq!(menu_height(&app), 0);
    }

    #[test]
    fn slash_menu_clamp_reconciles_selection_after_edits() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.input = "/".to_string();
        clamp_menu_sel(&mut app);
        assert_eq!(app.menu_sel, Some(0));
        app.input = "/c".to_string();
        app.menu_sel = Some(2);
        clamp_menu_sel(&mut app);
        assert_eq!(app.menu_sel, Some(0));
        app.input = "hello".to_string();
        clamp_menu_sel(&mut app);
        assert_eq!(app.menu_sel, None);
        app.input = "/foo bar".to_string();
        app.menu_sel = Some(0);
        clamp_menu_sel(&mut app);
        assert_eq!(app.menu_sel, None);
    }

    #[test]
    fn slash_menu_up_down_wrap_and_never_touch_history() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/".to_string();
        app.cursor = 1;
        app.menu_sel = Some(0);
        app.history = vec!["old turn".to_string()];
        // 9 registry commands: quit, clear, help, theme, init, resume,
        // rename, fork, skills.
        for expect in [1, 2, 3, 4, 5, 6, 7, 8, 0] {
            assert!(!handle_key(
                &mut app,
                &agent,
                &tx,
                &think_tx,
                KeyCode::Down,
                KeyModifiers::empty()
            ));
            assert_eq!(app.menu_sel, Some(expect));
        }
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Up,
            KeyModifiers::empty()
        ));
        assert_eq!(app.menu_sel, Some(8));
        assert_eq!(app.hist_idx, None);
        assert_eq!(app.input, "/");
        assert_eq!(app.history, vec!["old turn".to_string()]);
    }

    fn print_flat(app: &App) -> String {
        app.print_queue.iter().flatten().map(|l| {
            l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
        }).collect::<Vec<_>>().join("\n")
    }

    #[tokio::test]
    async fn slash_skills_lists_without_turn() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/skills".to_string();
        app.cursor = 7;
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app.busy, "/skills must not start a turn");
        let flat = print_flat(&app);
        // StubAgent has no skills → empty notice.
        assert!(flat.contains("no skills"), "got: {flat}");
    }

    #[tokio::test]
    async fn slash_skills_bad_arg_usage_no_turn() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/skills frobnicate".to_string();
        app.cursor = 19;
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app.busy);
        assert!(print_flat(&app).contains("usage: /skills [reload]"));
    }

    #[tokio::test]
    async fn skill_colon_unknown_queues_notice_no_turn() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        let agent = std::sync::Arc::new(SkillStubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/skill:nope do x".to_string();
        app.cursor = 14;
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app.busy, "unknown skill must not start a turn");
        assert!(print_flat(&app).contains("no skill 'nope'"));
    }

    #[tokio::test]
    async fn skill_colon_single_expands_body_with_task() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        let agent = std::sync::Arc::new(SkillStubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/skill:demo fix login".to_string();
        app.cursor = 22;
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Enter, KeyModifiers::empty()));
        assert!(app.busy, "known skill must start a turn");
        // Transcript + history show the short form.
        assert_eq!(app.history.last().map(String::as_str), Some("/skill:demo fix login"));
        assert!(print_flat(&app).contains("/skill:demo fix login"));
    }

    #[tokio::test]
    async fn skill_colon_stacked_expands_both() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        let agent = std::sync::Arc::new(TwoSkillStubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/skill:demo /skill:plain task here".to_string();
        app.cursor = 34;
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Enter, KeyModifiers::empty()));
        assert!(app.busy, "stacked skills must start a turn");
        assert_eq!(app.history.last().map(String::as_str), Some("/skill:demo /skill:plain task here"));
    }

    #[tokio::test]
    async fn skill_colon_bare_shows_usage() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        let agent = std::sync::Arc::new(SkillStubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/skill:".to_string();
        app.cursor = 7;
        assert!(!handle_key(&mut app, &agent, &tx, &think_tx, KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app.busy);
        assert!(print_flat(&app).contains("usage: /skill:<name>"));
    }

    #[test]
    fn skill_menu_suggests_prefix() {
        let names = vec!["pdf".to_string(), "plan".to_string(), "deploy".to_string()];
        assert_eq!(skill_arg_partial("/skill:p"), Some("p".to_string()));
        assert_eq!(skill_arg_partial("/skill:"), Some(String::new()));
        assert!(skill_arg_partial("/skill:p task").is_none());
        assert!(skill_arg_partial("/skills").is_none());
        assert_eq!(match_skill_names(&names, "p"), vec!["pdf".to_string(), "plan".to_string()]);
        assert_eq!(
            skill_arg_matches_for("/skill:dep", &names),
            vec!["deploy".to_string()]
        );
        assert!(skill_arg_matches_for("/skill:x", &names).is_empty());
    }

    #[test]
    fn skill_menu_pane_grows_for_suggestions() {
        // Regression: pane_height used the command/theme-only count, so the
        // `/skill:` popup was squeezed to ~2 rows instead of up to 7.
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.skill_names = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
            "f".to_string(),
            "g".to_string(),
        ];
        app.input = "/skill:".to_string();
        app.cursor = 7;
        assert!(is_menu_open(&app));
        assert_eq!(menu_row_count_app(&app), 7);
        // Pane grows by min(7, PREFERRED) rows when the menu is open.
        let grown = app.pane_height(80);
        app.menu_sel = None;
        app.input = "hello".to_string();
        let idle = app.pane_height(80);
        assert_eq!(grown, idle + MENU_PREFERRED_ROWS.min(7) as u16);
    }

    #[test]
    fn skill_menu_opens_for_partial() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.skill_names = vec!["pdf".to_string(), "plan".to_string()];
        app.input = "/skill:p".to_string();
        app.cursor = 8;
        assert!(is_menu_open(&app));
        assert_eq!(menu_row_count_app(&app), 2);
        clamp_menu_sel(&mut app);
        assert_eq!(app.menu_sel, Some(0));
    }

    #[tokio::test]
    async fn slash_init_starts_turn_with_display_short() {
        // ADR-0009: `/init` rewrites to INIT_PROMPT and runs a normal turn;
        // the transcript shows `/init`, not the full instruction.
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/init".to_string();
        app.cursor = 5;
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        assert_eq!(app.history.last().map(String::as_str), Some("/init"));
        assert!(app.busy, "/init must start a turn");
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("/init"), "transcript must show /init: {flat}");
        assert!(
            !flat.contains("Generate an AGENTS.md"),
            "transcript must not leak full prompt: {flat}"
        );
    }

    #[test]
    fn slash_menu_tab_completes_highlighted_name() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/c".to_string();
        app.cursor = 2;
        app.menu_sel = Some(0);
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Tab,
            KeyModifiers::empty()
        ));
        assert_eq!(app.input, "/clear");
        assert_eq!(app.cursor, 6);
        assert!(is_menu_open(&app));
        assert_eq!(app.menu_sel, Some(0));
    }

    #[test]
    fn slash_menu_esc_idle_only_dismisses() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/".to_string();
        app.cursor = 1;
        app.menu_sel = Some(1);
        let before = app.print_queue.len();
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Esc,
            KeyModifiers::empty()
        ));
        assert_eq!(app.menu_sel, None);
        assert!(!app.busy);
        assert_eq!(app.print_queue.len(), before);
        assert!(is_menu_open(&app));
    }

    #[test]
    fn slash_menu_esc_busy_aborts_and_dismisses() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.busy = true;
        app.turn_generation = 3;
        app.input = "/".to_string();
        app.cursor = 1;
        app.menu_sel = Some(0);
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Esc,
            KeyModifiers::empty()
        ));
        assert_eq!(app.menu_sel, None);
        assert!(!app.busy);
        assert_eq!(app.turn_generation, 4);
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("interrupted."), "got: {flat}");
    }

    #[test]
    fn slash_menu_typing_space_closes_menu() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/".to_string();
        app.cursor = 1;
        app.menu_sel = Some(0);
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Char(' '),
            KeyModifiers::empty()
        ));
        assert_eq!(app.input, "/ ");
        assert_eq!(app.menu_sel, None);
        assert!(!is_menu_open(&app));
    }

    #[test]
    fn slash_submit_prefix_runs_highlighted_command() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/c".to_string();
        app.cursor = 2;
        app.menu_sel = Some(0);
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        assert_eq!(app.menu_sel, None);
        assert_eq!(app.history.last().map(String::as_str), Some("/clear"));
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("cleared."), "got: {flat}");
    }

    #[test]
    fn slash_submit_bare_slash_runs_top_match_quit() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/".to_string();
        app.cursor = 1;
        app.menu_sel = Some(0);
        assert!(handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
    }

    #[test]
    fn slash_submit_help_lists_every_registry_command() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/h".to_string();
        app.cursor = 2;
        app.menu_sel = Some(0);
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for name in ["/quit", "/clear", "/help", "/theme", "/init", "/skills"] {
            assert!(flat.contains(name), "help missing {name}: {flat}");
        }
        assert!(flat.contains("quit the app"), "help missing desc: {flat}");
    }

    #[test]
    fn slash_submit_unknown_points_at_help() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        app.input = "/bogus".to_string();
        app.cursor = 6;
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        assert!(!app.busy, "unknown slash must not start a turn");
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("unknown command"), "got: {flat}");
        assert!(flat.contains("Try /help."), "got: {flat}");
    }

    #[test]
    fn slash_menu_renders_rows_above_composer() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, PANE_ROWS);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("test-model".to_string(), "medium".to_string());
        app.input = "/".to_string();
        app.cursor = 1;
        app.menu_sel = Some(0);
        terminal.draw(|f| render_pane(f, &mut app)).unwrap();
        let text = terminal.backend().to_string();
        // Idle capacity fits 3 rows; 9 registry commands spill into
        // 2 visible rows + a `+7 more` overflow row.
        for name in ["/quit", "/clear"] {
            assert!(text.contains(name), "menu row missing {name}: {text}");
        }
        assert!(
            text.contains("clear transcript"),
            "menu desc missing: {text}"
        );
        assert!(text.contains('▸'), "selection marker missing: {text}");
        assert!(text.contains("+7 more"), "overflow row missing: {text}");
        let menu_row = text.lines().position(|l| l.contains("/quit")).unwrap();
        let input_row = text.lines().position(|l| l.contains("❯ /")).unwrap();
        assert!(menu_row < input_row, "menu must render above the composer");
        assert_eq!(input_row, 3, "composer should remain in its normal row");
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(4, input_row as u16),
            "caret should stay on the typed slash in the composer"
        );

        let mut app = App::new("test-model".to_string(), "medium".to_string());
        terminal.draw(|f| render_pane(f, &mut app)).unwrap();
        let text = terminal.backend().to_string();
        assert!(
            text.contains("Message rem…"),
            "composer placeholder missing: {text}"
        );
    }

    #[test]
    fn slash_menu_keeps_busy_status_and_composer_visible() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, PANE_ROWS);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("test-model".to_string(), "medium".to_string());
        app.busy = true;
        app.input = "/".to_string();
        app.cursor = 1;
        app.menu_sel = Some(2);

        terminal.draw(|f| render_pane(f, &mut app)).unwrap();

        let text = terminal.backend().to_string();
        assert!(text.contains("working"), "busy status missing: {text}");
        assert!(text.contains("/help"), "selected command missing: {text}");
        // Busy status takes a row, so only 1 menu row is visible and
        // 8 - 1 = 7 rows are hidden behind the overflow hint.
        assert!(text.contains("+7 more"), "overflow hint missing: {text}");
        assert!(
            text.lines().any(|line| line.contains("❯ /")),
            "typed input missing: {text}"
        );
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(4, 3),
            "caret should stay on the composer's normal row"
        );
    }

    #[test]
    fn slash_theme_lists_current_when_no_arg() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        handle_theme_command(&mut app, "");
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // No themes installed in CI env: names the current theme.
        // (With themes installed it lists them instead — either way it
        // must mention the current theme name.)
        assert!(
            flat.contains(&app.theme.name),
            "theme list must name current: {flat}"
        );
    }

    #[test]
    fn slash_theme_unknown_name_queues_error_notice() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        let before = app.theme.name.clone();
        handle_theme_command(&mut app, "definitely-not-a-theme-xyz");
        assert_eq!(app.theme.name, before, "failed switch must keep theme");
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("unknown theme"), "got: {flat}");
        assert!(!app.busy, "theme command must not start a turn");
    }

    #[test]
    fn slash_theme_dispatches_with_arg_through_submit() {
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.input = "/theme nope-xyz".to_string();
        app.cursor = app.input.chars().count();
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // Menu is closed (whitespace in input) so Enter dispatches the
        // full `/theme <arg>` line via parse_command.
        assert!(!is_menu_open(&app));
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        assert_eq!(
            app.history.last().map(String::as_str),
            Some("/theme nope-xyz")
        );
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("unknown theme"), "got: {flat}");
        assert!(!app.busy, "/theme must not start a turn");
    }

    // ---- ADR-0010 theme-arg suggestions ----

    #[test]
    fn theme_arg_partial_only_matches_single_arg_form() {
        assert_eq!(theme_arg_partial("/theme "), Some(String::new()));
        assert_eq!(theme_arg_partial("/theme d"), Some("d".to_string()));
        assert_eq!(theme_arg_partial("/theme dark"), Some("dark".to_string()));
        // Bare `/theme` belongs to the command menu, not arg mode.
        assert_eq!(theme_arg_partial("/theme"), None);
        assert_eq!(theme_arg_partial("/the"), None);
        // Other commands never trigger arg mode.
        assert_eq!(theme_arg_partial("/clear "), None);
        assert_eq!(theme_arg_partial("/ "), None);
        // Multi-token args dispatch directly, no suggestions.
        assert_eq!(theme_arg_partial("/theme a b"), None);
        assert_eq!(theme_arg_partial("/theme dark "), None);
        // Lookalike prefixes must not trigger (`/themedark`, `/themes`).
        assert_eq!(theme_arg_partial("/themedark"), None);
        assert_eq!(theme_arg_partial("/themes x"), None);
        assert_eq!(theme_arg_partial("theme "), None);
        assert_eq!(theme_arg_partial("/THEME "), None);
    }

    #[test]
    fn match_theme_names_filters_by_prefix_in_order() {
        let names = vec![
            "dark".to_string(),
            "dracula".to_string(),
            "gruvbox".to_string(),
        ];
        assert_eq!(
            match_theme_names(&names, ""),
            names,
            "empty partial lists everything"
        );
        assert_eq!(
            match_theme_names(&names, "d"),
            vec!["dark".to_string(), "dracula".to_string()]
        );
        assert_eq!(match_theme_names(&names, "dar"), vec!["dark".to_string()]);
        assert_eq!(match_theme_names(&names, "gr"), vec!["gruvbox".to_string()]);
        assert!(match_theme_names(&names, "xyz").is_empty());
        assert!(match_theme_names(&names, "DR").is_empty(), "case-sensitive");
    }

    #[test]
    fn theme_arg_menu_opens_for_partial_and_tab_completes() {
        // Uses the real ~/.config/rem/themes dir (10 themes installed).
        // If run on a machine with no themes, this degrades to the
        // closed-menu path — still asserts no crash and clean dispatch.
        let installed = crate::theme::Theme::list_themes().unwrap_or_default();
        let mut app = App::new("model".to_string(), "medium".to_string());
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        // Typing a space after the command closes the command menu…
        app.input = "/theme ".to_string();
        app.cursor = app.input.chars().count();
        assert!(menu_matches(&app.input).is_empty());
        clamp_menu_sel(&mut app);
        if installed.is_empty() {
            // …and with no themes installed the menu stays closed.
            assert!(!is_menu_open(&app));
            assert_eq!(app.menu_sel, None);
            return;
        }
        // …and opens the theme-arg menu listing every installed theme.
        let names = theme_arg_matches(&app.input);
        assert_eq!(names, installed);
        assert!(is_menu_open(&app));
        assert_eq!(app.menu_sel, Some(0));
        // Partial filters the list.
        app.input = "/theme gruvbox".to_string();
        app.cursor = app.input.chars().count();
        clamp_menu_sel(&mut app);
        let filtered = theme_arg_matches(&app.input);
        assert!(!filtered.is_empty(), "expected gruvbox-* themes installed");
        assert!(filtered.iter().all(|n| n.starts_with("gruvbox")));
        // Up/Down wrap within the theme list, never touching history.
        let n = filtered.len();
        let before_hist = app.history.len();
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Down,
            KeyModifiers::empty()
        ));
        assert_eq!(app.menu_sel, Some(1 % n));
        assert_eq!(app.history.len(), before_hist);
        // Tab completes the highlighted theme name into the input.
        app.menu_sel = Some(0);
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Tab,
            KeyModifiers::empty()
        ));
        assert_eq!(app.input, format!("/theme {}", filtered[0]));
        // Enter switches to the completed theme and clears the line.
        // Snapshot the real config file first: Enter persists the choice
        // via save_active, and tests must not leave side effects.
        let config_path = crate::config::Config::path().expect("config path");
        let config_before = std::fs::read(&config_path).ok();
        app.menu_sel = Some(0);
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        assert_eq!(app.theme.name, filtered[0]);
        assert_eq!(app.input, "");
        assert!(!app.busy, "/theme must not start a turn");
        // The switch persisted, then restore the file byte-for-byte.
        let saved = std::fs::read_to_string(&config_path).expect("config saved");
        assert!(
            saved.contains(&format!("theme = \"{}\"", filtered[0])),
            "switch must persist: {saved}"
        );
        match config_before {
            Some(bytes) => std::fs::write(&config_path, bytes).expect("restore config"),
            None => {
                std::fs::remove_file(&config_path).ok();
            }
        };
    }

    #[test]
    fn theme_arg_enter_without_suggestions_dispatches_normally() {
        // `/theme nope-xyz` with nothing installed: no suggestions, so
        // Enter falls through to the unknown-theme error (existing path).
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.input = "/theme nope-xyz".to_string();
        app.cursor = app.input.chars().count();
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!is_menu_open(&app));
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        assert_eq!(
            app.history.last().map(String::as_str),
            Some("/theme nope-xyz")
        );
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("unknown theme"), "got: {flat}");
    }

    #[test]
    fn theme_arg_menu_renders_name_rows_above_composer() {
        use ratatui::{Terminal, backend::TestBackend};
        // Needs real theme files; skip gracefully when none installed.
        if crate::theme::Theme::list_themes()
            .unwrap_or_default()
            .is_empty()
        {
            return;
        }
        let backend = TestBackend::new(80, PANE_ROWS);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.input = "/theme ".to_string();
        app.cursor = app.input.chars().count();
        clamp_menu_sel(&mut app);
        assert!(is_menu_open(&app));
        terminal.draw(|f| render_pane(f, &mut app)).unwrap();
        let text = terminal.backend().to_string();
        let installed = crate::theme::Theme::list_themes().unwrap();
        assert!(
            text.contains(&installed[0]),
            "theme row missing {}: {text}",
            installed[0]
        );
        assert!(text.contains('▸'), "selection marker missing: {text}");
        assert!(
            text.lines().any(|l| l.contains("❯ /theme ")),
            "typed input missing: {text}"
        );
        // Theme rows carry no slash-command prefix or desc.
        assert!(
            !text.contains("/gruvbox"),
            "theme rows need no slash: {text}"
        );
        // Partial narrows the painted rows.
        app.input = "/theme gruvbox".to_string();
        app.cursor = app.input.chars().count();
        clamp_menu_sel(&mut app);
        terminal.draw(|f| render_pane(f, &mut app)).unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("gruvbox"), "filtered row missing: {text}");
        assert!(!text.contains("nord"), "unmatched row leaked: {text}");
    }

    #[test]
    fn slash_help_lists_theme_command() {
        // Covered by slash_submit_help_lists_every_registry_command,
        // but pin the theme usage line explicitly.
        let mut app = App::new("model".to_string(), "medium".to_string());
        app.print_queue.clear();
        app.input = "/help".to_string();
        app.cursor = 5;
        let agent = std::sync::Arc::new(StubAgent);
        let (tx, _rx) = mpsc::channel::<TurnResult>();
        let (think_tx, _think_rx) = mpsc::channel::<ThinkMsg>();
        assert!(!handle_key(
            &mut app,
            &agent,
            &tx,
            &think_tx,
            KeyCode::Enter,
            KeyModifiers::empty()
        ));
        let flat: String = app
            .print_queue
            .iter()
            .flatten()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("/theme <arg>"), "got: {flat}");
    }

    struct StubAgent;

    #[async_trait::async_trait]
    impl AgentLoop for StubAgent {
        async fn chat(&self, _prompt: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }

    /// Stub with one skill (`demo`: body echoes `$ARGUMENTS`).
    struct SkillStubAgent;

    #[async_trait::async_trait]
    impl AgentLoop for SkillStubAgent {
        async fn chat(&self, _prompt: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn list_skills(&self) -> Vec<crate::agent::SkillSummary> {
            vec![crate::agent::SkillSummary {
                name: "demo".to_string(),
                description: "Demo skill.".to_string(),
                path: "/x/demo/SKILL.md".to_string(),
                note: None,
            }]
        }
        fn get_skill_body(&self, name: &str) -> Option<String> {
            match name {
                "demo" => Some("Do the demo: $ARGUMENTS".to_string()),
                "plain" => Some("Just do it.".to_string()),
                _ => None,
            }
        }
    }

    /// Stub with two invokable skills for stacked-mention tests.
    struct TwoSkillStubAgent;

    #[async_trait::async_trait]
    impl AgentLoop for TwoSkillStubAgent {
        async fn chat(&self, _prompt: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn get_skill_body(&self, name: &str) -> Option<String> {
            match name {
                "demo" => Some("Do the demo: $ARGUMENTS".to_string()),
                "plain" => Some("Just do it.".to_string()),
                _ => None,
            }
        }
    }
}
