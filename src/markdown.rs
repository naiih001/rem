use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::history::wrap_line;

const CODE_BG: Color = Color::Rgb(0x19, 0x19, 0x19);
const TABLE_SEPARATOR: &str = " │ ";

pub(crate) fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    Renderer::new(width).render(text)
}

struct Renderer {
    width: usize,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    style: Style,
    styles: Vec<Style>,
    quote_depth: usize,
    lists: Vec<ListFrame>,
    heading: Option<Style>,
    code_block: Option<String>,
    table: Option<Table>,
    link_destinations: Vec<String>,
}

struct ListFrame {
    next: u64,
    marker: String,
    first_pending: bool,
}

struct Table {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_row: bool,
    header_rows: usize,
}

impl Renderer {
    fn new(width: usize) -> Self {
        Self {
            width: width.max(1),
            lines: Vec::new(),
            current: Vec::new(),
            style: Style::default(),
            styles: Vec::new(),
            quote_depth: 0,
            lists: Vec::new(),
            heading: None,
            code_block: None,
            table: None,
            link_destinations: Vec::new(),
        }
    }

    fn render(mut self, text: &str) -> Vec<Line<'static>> {
        for event in Parser::new_ext(text, Options::all()) {
            self.event(event);
        }
        self.flush_line();
        self.lines
    }

    fn event(&mut self, event: Event<'_>) {
        if let Some(code) = &mut self.code_block {
            match event {
                Event::End(TagEnd::CodeBlock) => self.finish_code_block(),
                Event::Text(text) | Event::Code(text) => code.push_str(&text),
                Event::SoftBreak | Event::HardBreak => code.push('\n'),
                _ => {}
            }
            return;
        }

        if self.table.is_some() {
            self.table_event(event);
            return;
        }

        match event {
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => self.flush_block(),
            Event::Start(Tag::Heading { level, .. }) => {
                self.heading = Some(heading_style(level));
                self.style = self.heading.unwrap();
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush_block();
                self.heading = None;
                self.style = Style::default();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.flush_block();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            Event::Start(Tag::CodeBlock(_)) => {
                self.flush_line();
                self.code_block = Some(String::new());
            }
            Event::Start(Tag::List(start)) => self.lists.push(ListFrame {
                next: start.unwrap_or(1),
                marker: String::new(),
                first_pending: false,
            }),
            Event::End(TagEnd::List(_)) => {
                self.flush_block();
                self.lists.pop();
            }
            Event::Start(Tag::Item) => {
                if let Some(list) = self.lists.last_mut() {
                    if list.next == 1 && list.marker.is_empty() {
                        list.marker = "•".to_string();
                    } else if list.marker == "•" || list.marker.is_empty() {
                        list.marker = "•".to_string();
                    } else {
                        list.marker = format!("{}.", list.next);
                        list.next = list.next.saturating_add(1);
                    }
                    list.first_pending = true;
                }
            }
            Event::End(TagEnd::Item) => self.flush_block(),
            Event::Start(Tag::Emphasis) => self.push_style(Modifier::ITALIC),
            Event::End(TagEnd::Emphasis) => self.pop_style(),
            Event::Start(Tag::Strong) => self.push_style(Modifier::BOLD),
            Event::End(TagEnd::Strong) => self.pop_style(),
            Event::Start(Tag::Strikethrough) => self.push_style(Modifier::CROSSED_OUT),
            Event::End(TagEnd::Strikethrough) => self.pop_style(),
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.link_destinations.push(dest_url.to_string());
                self.push_style(Modifier::UNDERLINED);
                self.style = self.style.fg(Color::Cyan);
            }
            Event::End(TagEnd::Link) => {
                self.pop_style();
                if let Some(url) = self.link_destinations.pop()
                    && !url.is_empty()
                {
                    self.push_span(format!(" ({url})"), Style::default().fg(Color::DarkGray));
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                self.link_destinations.push(dest_url.to_string());
            }
            Event::End(TagEnd::Image) => {
                if let Some(url) = self.link_destinations.pop()
                    && !url.is_empty()
                {
                    self.push_span(format!(" ({url})"), Style::default().fg(Color::DarkGray));
                }
            }
            Event::Start(Tag::Table(alignments)) => {
                self.flush_block();
                self.table = Some(Table {
                    alignments,
                    rows: Vec::new(),
                    row: Vec::new(),
                    cell: String::new(),
                    in_row: false,
                    header_rows: 0,
                });
            }
            Event::Rule => {
                self.flush_line();
                self.emit_line(vec![Span::styled(
                    "─".repeat(self.width),
                    Style::default().fg(Color::DarkGray),
                )]);
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                self.push_span(text.to_string(), self.style);
            }
            Event::Code(text) => self.push_span(
                text.to_string(),
                Style::default().fg(Color::Yellow).bg(CODE_BG),
            ),
            Event::SoftBreak => self.push_span(" ".to_string(), self.style),
            Event::HardBreak => self.flush_line(),
            Event::FootnoteReference(label) => self.push_span(
                format!("[^{label}]"),
                Style::default().fg(Color::DarkGray),
            ),
            Event::TaskListMarker(checked) => {
                self.push_span(
                    if checked { "[x] " } else { "[ ] " }.to_string(),
                    Style::default().fg(Color::DarkGray),
                );
            }
            _ => {}
        }
    }

    fn table_event(&mut self, event: Event<'_>) {
        let table = self.table.as_mut().expect("table event requires table state");
        match event {
            Event::Start(Tag::TableHead) => {
                table.in_row = true;
                table.row.clear();
            }
            Event::End(TagEnd::TableHead) => {
                table.finish_row();
                table.header_rows = table.rows.len();
            }
            Event::Start(Tag::TableRow) => {
                table.in_row = true;
                table.row.clear();
            }
            Event::End(TagEnd::TableRow) => table.finish_row(),
            Event::Start(Tag::TableCell) => table.cell.clear(),
            Event::End(TagEnd::TableCell) => table.row.push(std::mem::take(&mut table.cell)),
            Event::Text(value) | Event::Code(value) | Event::Html(value) | Event::InlineHtml(value) => {
                table.cell.push_str(&value);
            }
            Event::SoftBreak | Event::HardBreak => table.cell.push(' '),
            Event::End(TagEnd::Table) => self.finish_table(),
            _ => {}
        }
    }

    fn finish_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let columns = table
            .rows
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(table.alignments.len())
            .max(1);
        let mut widths = vec![1usize; columns];
        for row in &table.rows {
            for (index, cell) in row.iter().enumerate().take(columns) {
                widths[index] = widths[index].max(UnicodeWidthStr::width(cell.as_str()));
            }
        }
        fit_columns(&mut widths, self.width);
        self.flush_line();
        for (row_index, row) in table.rows.iter().enumerate() {
            let wrapped_cells: Vec<Vec<String>> = (0..columns)
                .map(|index| {
                    wrap_cell(row.get(index).map_or("", String::as_str), widths[index])
                })
                .collect();
            let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1);
            for line_index in 0..row_height {
                let mut spans = Vec::new();
                for (column, cell_lines) in wrapped_cells.iter().enumerate() {
                    let value = cell_lines.get(line_index).map_or("", String::as_str);
                    let alignment = table.alignments.get(column).copied().unwrap_or(Alignment::None);
                    let value = align_cell(value, widths[column], alignment);
                    let style = if row_index < table.header_rows {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    spans.push(Span::styled(value, style));
                    if column + 1 < columns {
                        spans.push(Span::styled(
                            TABLE_SEPARATOR,
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
                self.emit_line(spans);
            }
            if row_index + 1 == table.header_rows {
                let separator = widths
                    .iter()
                    .map(|width| "─".repeat(*width))
                    .collect::<Vec<_>>()
                    .join("─┼─");
                self.emit_line(vec![Span::styled(
                    separator,
                    Style::default().fg(Color::DarkGray),
                )]);
            }
        }
    }

    fn finish_code_block(&mut self) {
        let code = self.code_block.take().unwrap_or_default();
        for raw in code.trim_end_matches('\n').split('\n') {
            let mut line = Line::from(vec![
                Span::styled("  ", Style::default().bg(CODE_BG)),
                Span::styled(raw.to_string(), Style::default().fg(Color::Gray).bg(CODE_BG)),
            ]);
            line.style = Style::default().bg(CODE_BG);
            for wrapped in wrap_line(line, self.width) {
                let mut rendered = wrapped;
                rendered.style = Style::default().bg(CODE_BG);
                self.lines.push(rendered);
            }
        }
        self.flush_block();
    }

    fn push_span(&mut self, text: String, style: Style) {
        if !text.is_empty() {
            self.current.push(Span::styled(text, style));
        }
    }

    fn push_style(&mut self, modifier: Modifier) {
        self.styles.push(self.style);
        self.style = self.style.add_modifier(modifier);
    }

    fn pop_style(&mut self) {
        if let Some(style) = self.styles.pop() {
            self.style = style;
        }
    }

    fn flush_block(&mut self) {
        self.flush_line();
        if !self.lines.is_empty() && !self.lines.last().map_or(true, |line| line.spans.is_empty()) {
            self.lines.push(Line::from(""));
        }
    }

    fn flush_line(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let current = std::mem::take(&mut self.current);
        self.emit_line(current);
    }

    fn emit_line(&mut self, mut spans: Vec<Span<'static>>) {
        let list_count = self.lists.len();
        let mut prefix = String::new();
        for (index, list) in self.lists.iter_mut().enumerate() {
            if list.first_pending {
                prefix.push_str(&list.marker);
                prefix.push(' ');
                list.first_pending = false;
            } else {
                prefix.push_str(&" ".repeat(UnicodeWidthStr::width(list.marker.as_str()) + 1));
            }
            if index + 1 < list_count {
                prefix.push(' ');
            }
        }
        if self.quote_depth > 0 {
            prefix.push_str(&"│ ".repeat(self.quote_depth));
        }
        if !prefix.is_empty() {
            spans.insert(0, Span::styled(prefix, Style::default().fg(Color::DarkGray)));
        }
        self.lines.push(Line::from(spans));
    }
}

impl Table {
    fn finish_row(&mut self) {
        if self.in_row {
            self.rows.push(std::mem::take(&mut self.row));
            self.in_row = false;
        }
    }
}

fn heading_style(level: HeadingLevel) -> Style {
    let modifier = match level {
        HeadingLevel::H1 | HeadingLevel::H2 => Modifier::BOLD,
        _ => Modifier::BOLD,
    };
    let color = match level {
        HeadingLevel::H1 => Color::Cyan,
        HeadingLevel::H2 => Color::Blue,
        _ => Color::Reset,
    };
    Style::default().fg(color).add_modifier(modifier)
}

fn fit_columns(widths: &mut [usize], total_width: usize) {
    if widths.is_empty() {
        return;
    }
    let separators = TABLE_SEPARATOR.chars().count() * widths.len().saturating_sub(1);
    let available = total_width.saturating_sub(separators).max(widths.len());
    while widths.iter().sum::<usize>() > available {
        let Some((index, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > 1)
            .max_by_key(|(_, width)| **width)
        else {
            break;
        };
        widths[index] -= 1;
    }
}

fn wrap_cell(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut columns = 0;
    for word in text.split_inclusive(' ') {
        let word_width = UnicodeWidthStr::width(word);
        if columns > 0 && columns + word_width > width {
            lines.push(line.trim_end().to_string());
            line.clear();
            columns = 0;
        }
        if word_width <= width {
            line.push_str(word);
            columns += word_width;
        } else {
            for ch in word.chars() {
                let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if columns + char_width > width && !line.is_empty() {
                    lines.push(line.trim_end().to_string());
                    line.clear();
                    columns = 0;
                }
                line.push(ch);
                columns += char_width;
            }
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line.trim_end().to_string());
    }
    lines
}

fn align_cell(text: &str, width: usize, alignment: Alignment) -> String {
    let padding = width.saturating_sub(UnicodeWidthStr::width(text));
    let (left, right) = match alignment {
        Alignment::Right => (padding, 0),
        Alignment::Center => (padding / 2, padding - padding / 2),
        _ => (0, padding),
    };
    format!("{}{text}{}", " ".repeat(left), " ".repeat(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn renders_blocks_lists_inline_styles_and_links() {
        let lines = render(
            "# Heading\n\nA **bold** and *italic* `code` [link](https://example.com).\n\n- one\n- two\n\n> quoted",
            60,
        );
        let text = flatten(&lines).join("\n");
        assert!(text.contains("Heading"));
        assert!(text.contains("bold"));
        assert!(text.contains("italic"));
        assert!(text.contains("code"));
        assert!(text.contains("link (https://example.com)"));
        assert!(text.contains("• one"));
        assert!(text.contains("• two"));
        assert!(text.contains("│ quoted"));
        assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
            span.content.as_ref() == "bold" && span.style.add_modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn renders_fenced_code_with_indentation_and_background() {
        let lines = render("```rust\nfn main() {}\n```", 40);
        assert_eq!(flatten(&lines)[0], "  fn main() {}");
        assert_eq!(lines[0].style.bg, Some(CODE_BG));
    }

    #[test]
    fn wraps_table_cells_to_terminal_width() {
        let lines = render(
            "| Name | Description |\n| --- | --- |\n| rem | a longer description that must wrap |\n",
            20,
        );
        assert!(flatten(&lines).iter().any(|line| line.contains("Description")));
        assert!(lines.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum::<usize>()
                <= 20
        }));
    }
}
