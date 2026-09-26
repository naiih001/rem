//! Scrollback transcript rows, Aster-style.
//!
//! Every builder returns finished `Vec<Line<'static>>`, already wrapped to
//! `width`. The event loop pushes them above the bottom-anchored viewport with
//! `insert_before` and never touches them again — scrolling, selection, and
//! copy belong to the terminal. Anatomy verified against Aster's
//! `history.rs`: `hang`/`bullet`/`branch`, user band + rail, tool label +
//! sub-rows, patch counts + tinted bands, HEAD/TAIL = 4 elision.

use std::ops::Range;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

/// Hanging-indent gutter: `• ` on the first wrapped row, spaces after.
const GUTTER: usize = 2;
/// Elision window for long tool output: first + last lines stay visible.
const ELIDE_HEAD: usize = 4;
const ELIDE_TAIL: usize = 4;

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn faint() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn col_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn body_width(width: usize) -> usize {
    width.saturating_sub(GUTTER + 2).max(8)
}

/// Split `text` into display rows of at most `max` columns, as byte ranges.
/// Breaks after spaces; a word wider than a row is cut mid-word. Ranges tile
/// the input so every byte belongs to exactly one row.
fn rows(text: &str, max: usize) -> Vec<Range<usize>> {
    let max = max.max(1);
    let mut out = Vec::new();
    let mut start = 0;
    let mut col = 0;
    let mut pos = 0;
    for chunk in text.split_inclusive(' ') {
        let chunk_start = pos;
        pos += chunk.len();
        let word = chunk.trim_end_matches(' ');
        let word_w = col_width(word);
        if col > 0 && col + word_w > max {
            out.push(start..chunk_start);
            start = chunk_start;
            col = 0;
        }
        if word_w > max {
            let mut w = 0;
            for (off, ch) in word.char_indices() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if w + cw > max && chunk_start + off > start {
                    out.push(start..chunk_start + off);
                    start = chunk_start + off;
                    w = 0;
                }
                w += cw;
            }
            col = w + (chunk.len() - word.len());
        } else {
            col += col_width(chunk);
        }
    }
    out.push(start..text.len());
    out
}

/// `text` as display rows, trailing spaces dropped.
fn wrapped(text: &str, max: usize) -> Vec<String> {
    rows(text, max)
        .into_iter()
        .map(|r| text[r].trim_end().to_string())
        .collect()
}

/// Re-flow a styled line to `max` columns, carrying each span's style across
/// the break.
pub(crate) fn wrap_line(line: Line<'static>, max: usize) -> Vec<Line<'static>> {
    let mut text = String::new();
    let mut runs = Vec::with_capacity(line.spans.len());
    for span in &line.spans {
        let start = text.len();
        text.push_str(&span.content);
        runs.push((start..text.len(), span.style));
    }
    let ranges = rows(&text, max);
    if ranges.len() <= 1 {
        return vec![line];
    }
    ranges
        .into_iter()
        .map(|row| {
            let spans: Vec<Span<'static>> = runs
                .iter()
                .filter_map(|(run, style)| {
                    let from = run.start.max(row.start);
                    let to = run.end.min(row.end);
                    (from < to).then(|| Span::styled(text[from..to].to_string(), *style))
                })
                .collect();
            Line::from(spans).style(line.style)
        })
        .collect()
}

/// Pad `line` with spaces so its background reaches the full width.
fn pad_to(mut line: Line<'static>, max: usize, style: Style) -> Line<'static> {
    let used = line
        .spans
        .iter()
        .map(|s| col_width(&s.content))
        .sum::<usize>();
    if used < max {
        line.spans.push(Span::styled(" ".repeat(max - used), style));
    }
    line
}

/// Hanging indent: first wrapped row gets `bullet`, continuations get
/// `GUTTER` spaces. Every input line is re-wrapped to the body width.
fn hang(lines: Vec<Line<'static>>, bullet: Span<'static>, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut first = true;
    for line in lines {
        for wrapped in wrap_line(line, body_width(width)) {
            let lead = match first {
                true => bullet.clone(),
                false => Span::raw(" ".repeat(GUTTER)),
            };
            let mut spans = vec![lead];
            spans.extend(wrapped.spans);
            out.push(Line::from(spans).style(wrapped.style));
            first = false;
        }
    }
    out
}

fn bullet() -> Span<'static> {
    Span::styled("• ", Style::default().fg(Color::DarkGray))
}

fn branch(first: bool) -> Span<'static> {
    match first {
        true => Span::styled("└ ", faint()),
        false => Span::raw("  "),
    }
}

/// Blank separator above every group, so groups read as chapters.
fn prepend_blank(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return lines;
    }
    lines.insert(0, Line::from(""));
    lines
}

/// A message the user sent: accent rail on a filled band, the only chapter
/// mark in the transcript. Multi-line prompts continue under `❯ `.
pub(crate) fn user_row(text: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let fill = Style::default().bg(theme.rail_bg);
    let body = body_width(width);
    let mut out = Vec::new();
    let mut first = true;
    for raw in text.lines() {
        // Keep one row per prompt line even when empty (no wrap of "").
        let chunks = match raw.is_empty() {
            true => vec![String::new()],
            false => wrapped(raw, body),
        };
        for chunk in chunks {
            let lead = match first {
                true => "❯ ",
                false => "  ",
            };
            let line = Line::from(vec![
                Span::styled("▌", Style::default().fg(theme.accent).bg(theme.rail_bg)),
                Span::styled(lead, Style::default().fg(theme.accent).bg(theme.rail_bg)),
                Span::styled(chunk, fill),
            ]);
            out.push(pad_to(line, width.max(1), fill));
            first = false;
        }
    }
    if out.is_empty() {
        return out;
    }
    prepend_blank(out)
}

/// Model reply: rendered Markdown under a bullet.
pub(crate) fn reply_rows(text: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let lines = crate::markdown::render(text, body_width(width), theme);
    match lines.is_empty() {
        true => Vec::new(),
        false => prepend_blank(hang(lines, bullet(), width)),
    }
}

/// Short harness status line (`cleared.`, audit notes, `interrupted.`).
pub(crate) fn notice_row(text: &str, width: usize) -> Vec<Line<'static>> {
    prepend_blank(hang(
        vec![Line::from(Span::styled(text.to_string(), dim()))],
        Span::styled("· ", dim()),
        width,
    ))
}

/// Fatal turn error.
pub(crate) fn error_row(text: &str, width: usize) -> Vec<Line<'static>> {
    prepend_blank(hang(
        vec![Line::from(Span::styled(
            text.to_string(),
            Style::default().fg(Color::Red),
        ))],
        bullet(),
        width,
    ))
}

/// Turn trailer (`Done …` / `Interrupted …`): italic dim bullet row.
pub(crate) fn trailer_row(text: &str, width: usize) -> Vec<Line<'static>> {
    prepend_blank(hang(
        vec![Line::from(Span::styled(
            text.to_string(),
            dim().add_modifier(Modifier::ITALIC),
        ))],
        bullet(),
        width,
    ))
}

/// One finished tool call: bold label, `└ summary`, then elided output.
/// Failed calls render the label and body in red.
pub(crate) fn tool_row(
    name: &str,
    preview: &str,
    ok: bool,
    summary: &str,
    output: &str,
    width: usize,
) -> Vec<Line<'static>> {
    let label = match preview.is_empty() {
        true => name.to_string(),
        false => format!("{name} {preview}"),
    };
    let head_style = match ok {
        true => Style::default().add_modifier(Modifier::BOLD),
        false => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    };
    let mut lines = vec![Line::from(Span::styled(label, head_style))];

    let sub_style = match ok {
        true => dim(),
        false => Style::default().fg(Color::Red),
    };
    if !summary.trim().is_empty() {
        lines.push(Line::from(vec![
            branch(true),
            Span::styled(summary.to_string(), sub_style),
        ]));
    }
    let body: Vec<&str> = output.lines().collect();
    for (i, text) in elide(&body).into_iter().enumerate() {
        let (text, style) = match text {
            Elided::Text(s) => (s.to_string(), sub_style),
            Elided::Gap(n) => (format!("… +{n} lines"), faint()),
        };
        // The summary already took the `└`; continuation rows indent.
        let lead = match i == 0 && summary.trim().is_empty() {
            true => branch(true),
            false => Span::raw("  "),
        };
        lines.push(Line::from(vec![lead, Span::styled(text, style)]));
    }
    prepend_blank(hang(lines, bullet(), width))
}

/// A diff body (`git_diff` output): `verb path` header with `+N −M` counts
/// pushed right, then full-row tinted bands with a darker mark glyph.
pub(crate) fn patch_row(verb: &str, path: &str, body: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    // `+++`/`---` file headers are not changed lines.
    let added = body
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .count();
    let removed = body
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .count();

    let inner = body_width(width);
    let left = match path.is_empty() {
        true => verb.to_string(),
        false => format!("{verb} {path}"),
    };
    let right = format!("+{added} −{removed}");
    let gap = inner.saturating_sub(col_width(&left) + col_width(&right) + 1);
    let header = Line::from(vec![
        Span::styled(
            format!("{verb} "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(path.to_string(), Style::default().fg(Color::Blue)),
        Span::raw(" ".repeat(gap + 1)),
        Span::styled(format!("+{added}"), Style::default().fg(theme.add_fg)),
        Span::raw(" "),
        Span::styled(format!("−{removed}"), Style::default().fg(theme.del_fg)),
    ]);

    let mut lines = vec![header];
    lines.extend(diff_lines(body, inner, theme));
    prepend_blank(hang(lines, bullet(), width))
}

/// Tint a unified-ish patch body row by row. Context lines stay faint on the
/// terminal background; added/removed lines get full-width bands.
fn diff_lines(body: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    body.lines()
        .map(|raw| {
            let (fg, bg, mark) = match raw.chars().next() {
                Some('+') if !raw.starts_with("+++") => (theme.add_fg, theme.add_bg, Some(theme.add_mark)),
                Some('-') if !raw.starts_with("---") => (theme.del_fg, theme.del_bg, Some(theme.del_mark)),
                _ => (Color::DarkGray, Color::Reset, None),
            };
            let style = Style::default().fg(fg).bg(bg);
            let text = wrapped(raw, width).first().cloned().unwrap_or_default();
            let line = match (mark, text.is_empty()) {
                (Some(mark_fg), false) => {
                    let (head, rest) = text.split_at(1);
                    Line::from(vec![
                        Span::styled(head.to_string(), Style::default().fg(mark_fg).bg(bg)),
                        Span::styled(rest.to_string(), style),
                    ])
                }
                _ => Line::from(Span::styled(text, style)),
            };
            pad_to(line, width, style)
        })
        .collect()
}

/// Approval request rows, printed into scrollback when the modal takes over
/// the keys. The pane underneath keeps running the status spinner.
pub(crate) fn approval_rows(
    tool_name: &str,
    args_preview: &str,
    reason: &str,
    queued: usize,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
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
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{tool_name} ({args_preview})"),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(reason.to_string(), dim()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "y approve · a always · n deny · x abort",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];
    prepend_blank(hang(lines, Span::raw(""), width))
}

enum Elided<'a> {
    Text(&'a str),
    Gap(usize),
}

fn elide<'a>(body: &[&'a str]) -> Vec<Elided<'a>> {
    if body.len() <= ELIDE_HEAD + ELIDE_TAIL + 1 {
        return body.iter().copied().map(Elided::Text).collect();
    }
    let mut out: Vec<Elided<'a>> = body[..ELIDE_HEAD]
        .iter()
        .copied()
        .map(Elided::Text)
        .collect();
    out.push(Elided::Gap(body.len() - ELIDE_HEAD - ELIDE_TAIL));
    out.extend(
        body[body.len() - ELIDE_TAIL..]
            .iter()
            .copied()
            .map(Elided::Text),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wrap_line_breaks_long_styled_lines() {
        let line = Line::from(vec![Span::styled(
            "aaa bbb ccc",
            Style::default().fg(Color::Red),
        )]);
        let out = wrap_line(line, 5);
        assert_eq!(out.len(), 3);
        // Style survives the break.
        for l in &out {
            for s in &l.spans {
                assert_eq!(s.style.fg, Some(Color::Red));
            }
        }
        // Breaks land after spaces; only `wrapped()` trims the tail.
        assert_eq!(out[0].spans[0].content.as_ref(), "aaa ");
        assert_eq!(out[2].spans[0].content.as_ref(), "ccc");
    }

    #[test]
    fn hang_indents_continuations_under_bullet() {
        let lines = vec![Line::from("0123456789")];
        let out = hang(lines, bullet(), 10);
        // body width 10-4=6: "012345" + "6789".
        assert_eq!(out.len(), 2);
        let text: Vec<String> = plain(&out);
        assert!(text[0].starts_with("• "), "got: {}", text[0]);
        assert!(text[1].starts_with("  "), "got: {}", text[1]);
    }

    #[test]
    fn user_row_renders_rail_prompt_and_full_width_band() {
        let theme = Theme::default();
        let out = user_row("hello", 20, &theme);
        assert_eq!(out.len(), 2); // blank + band
        let text: Vec<String> = plain(&out);
        assert!(text[1].contains('▌'), "got: {}", text[1]);
        assert!(text[1].contains("❯ hello"), "got: {}", text[1]);
        // Band reaches the full width (bg fill).
        assert_eq!(col_width(&text[1]), 20, "got: {}", text[1]);
        // Rail carries the accent.
        let rail = &out[1].spans[0];
        assert_eq!(rail.style.fg, Some(theme.accent));
        assert_eq!(rail.style.bg, Some(theme.rail_bg));
    }

    #[test]
    fn tool_row_shows_label_summary_and_elides_long_output() {
        let output = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = tool_row("bash", "cargo test", true, "ok", &output, 80);
        let text: Vec<String> = plain(&out);
        assert!(text[1].contains("bash cargo test"), "got: {:?}", text);
        assert!(text[2].contains("└ ok"), "got: {:?}", text);
        // 4 head + gap + 4 tail, plus blank + label + summary = 12.
        assert_eq!(out.len(), 12, "got: {text:?}");
        assert!(
            text.iter().any(|l| l.contains("… +12 lines")),
            "got: {text:?}"
        );
        assert!(text.iter().any(|l| l.contains("line 19")), "got: {text:?}");
        // No gap for short output.
        let short = tool_row("read", "a.rs", true, "s", "l1\nl2", 80);
        let short_text: Vec<String> = plain(&short);
        assert!(
            !short_text.iter().any(|l| l.contains('+')),
            "got: {short_text:?}"
        );
    }

    #[test]
    fn failed_tool_row_renders_red() {
        let out = tool_row("bash", "rm x", false, "failed: nope", "err", 80);
        assert_eq!(out[1].spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn patch_row_counts_and_tints_bands() {
        let theme = Theme::default();
        let body = "--- a/f\n+++ b/f\n ctx\n+ add\n- del";
        let out = patch_row("Diff", "f", body, 40, &theme);
        let text: Vec<String> = plain(&out);
        assert!(text[1].contains("+1"), "got: {:?}", text);
        assert!(text[1].contains("−1"), "got: {:?}", text);
        // Added line carries the add background; context does not.
        let add_line = out
            .iter()
            .find(|l| plain(std::slice::from_ref(l))[0].contains("+ add"));
        let add_line = add_line.expect("added line present");
        assert_eq!(add_line.spans.last().unwrap().style.bg, Some(theme.add_bg));
        let ctx_line = out
            .iter()
            .find(|l| plain(std::slice::from_ref(l))[0].contains(" ctx"))
            .expect("context line present");
        assert_eq!(ctx_line.spans.last().unwrap().style.bg, Some(Color::Reset));
    }

    #[test]
    fn trailer_and_notice_carry_bullet_glyphs() {
        let t: Vec<String> = plain(&trailer_row("Done (1.2s · 2 tools)", 80));
        assert!(t[1].starts_with("• "), "got: {t:?}");
        let n: Vec<String> = plain(&notice_row("cleared.", 80));
        assert!(n[1].starts_with("· "), "got: {n:?}");
    }
}
