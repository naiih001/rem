use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders},
};

/// Small float box with thin border. Fits text. Sits low in area.
/// Same pane_bg fill. Border uses placeholder color. Width caps at max_w.
/// Viewport stays small, so chat list above is safe.
pub fn render_float(f: &mut Frame, area: Rect, height: u16, max_w: u16, theme: &Theme) -> Rect {
    let w = area.width.min(max_w).max(1);
    let h = height.min(area.height).max(1);
    // Low in area: above input line, not mid screen.
    // Small gap of 1 row from bottom edge.
    let y = area.y + area.height.saturating_sub(h).saturating_sub(1);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let rect = Rect::new(x, y, w, h);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.placeholder))
        .style(Style::default().bg(theme.pane_bg));
    f.render_widget(block, rect);
    // Inner text zone: inside border.
    Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    )
}

#[derive(Debug, Clone, Default)]
pub struct ListState {
    pub selected: usize,
    pub filter: String,
    pub filtering: bool,
}

impl ListState {
    pub fn move_selection(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        len: usize,
        page: usize,
    ) -> bool {
        if len == 0 {
            return false;
        }
        let step = page.max(1);
        match (code, modifiers.contains(KeyModifiers::CONTROL)) {
            (KeyCode::Char('j'), false) | (KeyCode::Down, false) => {
                self.selected = (self.selected + 1).min(len - 1)
            }
            (KeyCode::Char('k'), false) | (KeyCode::Up, false) => {
                self.selected = self.selected.saturating_sub(1)
            }
            (KeyCode::Char('d'), true) => self.selected = (self.selected + step).min(len - 1),
            (KeyCode::Char('u'), true) => self.selected = self.selected.saturating_sub(step),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vim_navigation_moves_by_item_and_half_page() {
        let mut state = ListState::default();
        assert!(state.move_selection(KeyCode::Char('j'), KeyModifiers::NONE, 10, 1));
        assert_eq!(state.selected, 1);
        state.move_selection(KeyCode::Char('d'), KeyModifiers::CONTROL, 10, 5);
        assert_eq!(state.selected, 6);
        state.move_selection(KeyCode::Char('u'), KeyModifiers::CONTROL, 10, 5);
        assert_eq!(state.selected, 1);
    }
    #[test]
    fn g_and_q_are_not_navigation_keys() {
        let mut state = ListState::default();
        assert!(!state.move_selection(KeyCode::Char('g'), KeyModifiers::NONE, 4, 1));
        assert!(!state.move_selection(KeyCode::Char('q'), KeyModifiers::NONE, 4, 1));
    }
}
