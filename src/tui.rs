use std::io::{self, Stdout};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Terminal,
};
use unicode_width::UnicodeWidthStr;

use crate::agent::{AgentLoop, ToolEvent};

/// Swappable UI abstraction. `RatatuiBackend` is the Ratatui implementation;
/// the old Cursive backend was removed in favor of this Aster-styled UI.
pub trait TuiBackend {
    fn run(self, agent: impl AgentLoop + Send + Sync + 'static) -> anyhow::Result<()>;
}

pub struct RatatuiBackend;

impl RatatuiBackend {
    pub fn new() -> Self {
        Self
    }
}

impl TuiBackend for RatatuiBackend {
    fn run(self, agent: impl AgentLoop + Send + Sync + 'static) -> anyhow::Result<()> {
        run_app(agent)
    }
}

/// Outcome of one completed turn, sent from the worker task to the UI loop.
struct TurnResult {
    result: Result<String, String>,
    events: Vec<ToolEvent>,
}

fn run_app(agent: impl AgentLoop + Send + Sync + 'static) -> anyhow::Result<()> {
    let model = agent.model_name();
    let agent = Arc::new(agent);
    let (tx, rx) = mpsc::channel::<TurnResult>();

    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let mut app = App::new(model);
    let outcome = event_loop(&mut terminal, &mut app, agent, &rx, tx);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    outcome
}

struct App {
    model: String,
    cwd: String,
    transcript: Vec<Line<'static>>,
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
            transcript: Vec::new(),
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
        };
        app.push_system("rem — Aster-style TUI. /quit exits, /clear clears history view.");
        app
    }

    fn push_user(&mut self, prompt: &str) {
        if !self.transcript.is_empty() {
            self.transcript.push(Line::from(""));
        }
        self.transcript.push(Line::from(vec![
            Span::styled(
                "› ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(prompt.to_string()),
        ]));
    }

    fn push_turn(&mut self, events: &[ToolEvent], result: &Result<String, String>) {
        for ev in events {
            self.push_tool(ev);
        }
        if !events.is_empty() {
            self.transcript.push(Line::from(""));
        }
        match result {
            Ok(text) => self.push_reply(text),
            Err(e) => {
                self.transcript.push(Line::from(vec![Span::styled(
                    format!("  [error] {e}"),
                    Style::default().fg(Color::Red),
                )]));
                self.status = Status::Error;
            }
        }
        if !events.is_empty() && result.is_ok() {
            let n = events.len();
            self.transcript.push(Line::from(vec![Span::styled(
                format!("  Done ({n} tool{})", if n == 1 { "" } else { "s" }),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )]));
        }
        self.cap_transcript();
        self.dirty = true;
    }

    fn push_tool(&mut self, ev: &ToolEvent) {
        let name_style = match ev.ok {
            true => Style::default().add_modifier(Modifier::BOLD),
            false => Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        };
        let mut head = vec![
            Span::raw("  ".to_string()),
            Span::styled(ev.name.clone(), name_style),
            Span::styled(
                format!(" {}", ev.arg_preview()),
                Style::default().fg(Color::DarkGray),
            ),
        ];
        if !ev.ok {
            head.push(Span::styled(
                " ✗",
                Style::default().fg(Color::Red),
            ));
        }
        self.transcript.push(Line::from(head));
        self.transcript.push(Line::from(vec![
            Span::styled("  └ ", Style::default().fg(Color::DarkGray)),
            Span::styled(ev.summary.clone(), Style::default().fg(Color::DarkGray)),
        ]));
    }

    fn push_reply(&mut self, text: &str) {
        let mut any = false;
        for line in text.lines() {
            any = true;
            self.transcript
                .push(Line::from(vec![Span::styled(
                    line.to_string(),
                    reply_style(line),
                )]));
        }
        if !any {
            self.transcript.push(Line::from(vec![Span::styled(
                "  (empty reply)",
                Style::default().fg(Color::DarkGray),
            )]));
        }
    }

    fn push_system(&mut self, msg: &str) {
        self.transcript.push(Line::from(vec![Span::styled(
            format!("  {msg}"),
            Style::default().fg(Color::DarkGray),
        )]));
        self.dirty = true;
    }

    fn cap_transcript(&mut self) {
        const MAX: usize = 5000;
        if self.transcript.len() > MAX {
            let drop = self.transcript.len() - MAX;
            self.transcript.drain(..drop);
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

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    agent: Arc<impl AgentLoop + Send + Sync + 'static>,
    rx: &mpsc::Receiver<TurnResult>,
    tx: mpsc::Sender<TurnResult>,
) -> anyhow::Result<()> {
    loop {
        // Drain completed turns without blocking the UI.
        while let Ok(turn) = rx.try_recv() {
            app.busy = false;
            app.turns += 1;
            if turn.result.is_ok() {
                app.status = Status::Ready;
            }
            app.push_turn(&turn.events, &turn.result);
        }

        if app.dirty || app.busy {
            terminal
                .draw(|f| render(f, app))
                .context("draw frame")?;
            // Place the hardware cursor inside the input line.
            let area = terminal.size().unwrap_or_default();
            let x = app.cursor_x(area.width);
            let y = area.height.saturating_sub(2);
            terminal
                .set_cursor_position(ratatui::layout::Position::new(x, y))
                .ok();
            terminal.show_cursor().ok();
            app.dirty = false;
        }

        if event::poll(Duration::from_millis(50)).context("poll events")? {
            match event::read().context("read event")? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if handle_key(app, &agent, &tx, key.code, key.modifiers) {
                        return Ok(());
                    }
                    app.dirty = true;
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
    code: KeyCode,
    mods: KeyModifiers,
) -> bool {
    if mods.contains(KeyModifiers::CONTROL) {
        return handle_ctrl(app, code);
    }
    match code {
        KeyCode::Enter => return submit(app, agent, tx),
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
            app.scroll = app.scroll.saturating_add(app.view_h.max(1));
        }
        KeyCode::Esc => {
            app.input.clear();
            app.cursor = 0;
        }
        KeyCode::Char(c) => {
            insert_char_at(&mut app.input, &mut app.cursor, c);
        }
        _ => {}
    }
    false
}

fn handle_ctrl(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('c') | KeyCode::Char('d') => true,
        KeyCode::Char('u') => {
            app.input.clear();
            app.cursor = 0;
            false
        }
        KeyCode::Char('w') => {
            delete_word_before(&mut app.input, &mut app.cursor);
            false
        }
        _ => false,
    }
}

fn submit(
    app: &mut App,
    agent: &Arc<impl AgentLoop + Send + Sync + 'static>,
    tx: &mpsc::Sender<TurnResult>,
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
        app.transcript.clear();
        app.push_system("cleared.");
        app.pinned = true;
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

    let agent = Arc::clone(agent);
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = agent.chat(&text).await;
        let events = agent.last_tool_events();
        let _ = tx.send(TurnResult { result, events });
    });
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
    /// Terminal x-coordinate of the cursor within the input row.
    /// Shares [`visible_window`] with the renderer so the hardware caret
    /// always sits on the displayed caret column, even mid-line in overflow.
    fn cursor_x(&self, term_width: u16) -> u16 {
        let max_w = (term_width as usize).saturating_sub(3);
        let (_, caret) = visible_window(&self.input, self.cursor, max_w);
        2u16.saturating_add(caret as u16)
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    app.view_h = chunks[1].height;

    render_header(f, app, chunks[0]);
    // Ratatui 0.29 clamps nothing: lines with y < scroll.y are skipped and
    // the rest shift up, so pin-to-bottom must be an explicit offset from the
    // last wrapped line count. `rendered_total` trails by one frame at most.
    let body_h = chunks[1].height as usize;
    let bottom = app.rendered_total.saturating_sub(body_h) as u16;
    let scroll = match app.pinned {
        true => bottom,
        false => app.scroll.min(bottom),
    };
    let body = Paragraph::new(app.transcript.clone())
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    app.rendered_total = count_wrapped(&app.transcript, chunks[1].width as usize);
    f.render_widget(body, chunks[1]);
    render_input(f, app, chunks[2]);
    render_footer(f, app, chunks[3]);
}

/// Wrapped line count of the transcript at `width`, mirroring
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

fn render_input(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let mut spans = vec![Span::styled(
        "› ",
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )];
    if app.input.is_empty() {
        spans.push(Span::styled(
            "Message rem… (/ for commands)",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        let max_w = (area.width as usize).saturating_sub(3);
        let (visible, _) = visible_window(&app.input, app.cursor, max_w);
        spans.push(Span::raw(visible));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_footer(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let left = vec![
        Span::styled("▶▶▶ ", Style::default().fg(Color::Yellow)),
        Span::styled(
            format!("edit · {} · {} turn{}", app.model, app.turns, if app.turns == 1 { "" } else { "s" }),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    let right_text = match (&app.status, app.busy) {
        (_, true) => {
            let dots = 1 + (app.busy_since.elapsed().as_millis() / 400 % 3) as usize;
            format!("thinking{}", ".".repeat(dots))
        }
        (_, _) => match app.status {
            Status::Ready => "ready".to_string(),
            Status::Error => "error — see log".to_string(),
        },
    };
    let right_style = match (&app.status, app.busy) {
        (_, true) => Style::default().fg(Color::Yellow),
        (_, _) => match app.status {
            Status::Ready => Style::default().fg(Color::DarkGray),
            Status::Error => Style::default().fg(Color::Red),
        },
    };
    let width = area.width as usize;
    let left_w: usize = left
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let pad = width.saturating_sub(left_w + right_text.width() + 1);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right_text, right_style));
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
}
