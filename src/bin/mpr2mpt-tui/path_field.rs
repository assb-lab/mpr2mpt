use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

pub(super) struct PathField {
    pub(super) value: String,
    cursor: usize,
}

impl PathField {
    pub(super) fn new(value: String) -> Self {
        Self {
            cursor: value.len(),
            value,
        }
    }

    pub(super) fn insert(&mut self, text: &str) {
        self.value.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.align_caret();
    }

    fn align_caret(&mut self) {
        // An edit can join neighboring graphemes, so move to the end of the joined grapheme.
        let text = Span::raw(self.value.as_str());
        self.cursor = Self::boundaries(&text)
            .find(|&position| position >= self.cursor)
            .unwrap_or(self.value.len());
    }

    fn boundaries<'a>(text: &'a Span<'a>) -> impl Iterator<Item = usize> + 'a {
        let ends = text
            .styled_graphemes(Style::default())
            .scan(0, |offset, grapheme| {
                *offset += grapheme.symbol.len();
                Some(*offset)
            });
        std::iter::once(0).chain(ends)
    }

    fn adjacent_boundaries(&self) -> (Option<usize>, Option<usize>) {
        let text = Span::raw(self.value.as_str());
        let mut boundaries = Self::boundaries(&text);
        let before = boundaries
            .by_ref()
            .take_while(|&position| position < self.cursor)
            .last();
        (before, boundaries.next())
    }

    pub(super) fn edit(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !c.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(c.encode_utf8(&mut [0; 4]));
            }
            KeyCode::Left => self.cursor = self.adjacent_boundaries().0.unwrap_or(self.cursor),
            KeyCode::Right => self.cursor = self.adjacent_boundaries().1.unwrap_or(self.cursor),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            KeyCode::Backspace => {
                if let Some(index) = self.adjacent_boundaries().0 {
                    self.value.drain(index..self.cursor);
                    self.cursor = index;
                    self.align_caret();
                }
            }
            KeyCode::Delete => {
                if let Some(index) = self.adjacent_boundaries().1 {
                    self.value.drain(self.cursor..index);
                    self.align_caret();
                }
            }
            _ => {}
        }
    }

    fn viewport(&self, width: u16) -> (&str, u16) {
        let prefix = Span::raw(&self.value[..self.cursor]);
        let widths = || {
            prefix.styled_graphemes(Style::default()).map(|grapheme| {
                let symbol = Span::raw(grapheme.symbol);
                (grapheme.symbol.len(), symbol.width())
            })
        };
        let mut caret_width: usize = widths().map(|(_, cells)| cells).sum();
        let mut start = 0;
        // Leave a cell for the caret and clip only at whole grapheme boundaries.
        for (bytes, cells) in widths() {
            if caret_width < usize::from(width) {
                break;
            }
            start += bytes;
            caret_width -= cells;
        }
        (&self.value[start..], caret_width as u16)
    }

    pub(super) fn draw(
        &self,
        frame: &mut Frame,
        area: Rect,
        title: &str,
        placeholder: &str,
        focused: bool,
    ) {
        let mut block = Block::bordered().title(title);
        if focused {
            block = block.border_style(Color::Cyan);
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.is_empty() {
            return;
        }
        let (visible, caret) = self.viewport(inner.width);
        let line = if self.value.is_empty() {
            Line::from(placeholder).dark_gray()
        } else {
            Line::from(visible)
        };
        frame.render_widget(Paragraph::new(line), inner);
        if focused {
            frame.set_cursor_position((inner.x + caret, inner.y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn keeps_the_caret_after_the_last_wide_grapheme_in_a_narrow_field() {
        let field = PathField::new("日本語東京京都大阪市".into());
        let mut terminal = Terminal::new(TestBackend::new(20, 3)).unwrap();
        terminal
            .draw(|frame| field.draw(frame, Rect::new(1, 0, 18, 3), "Input MPR", "", true))
            .unwrap();

        let backend = terminal.backend();
        assert_eq!(backend.buffer()[(14, 1)].symbol(), "市");
        assert_eq!(backend.cursor_position(), (16, 1).into());
    }

    #[test]
    fn moves_and_deletes_by_grapheme_in_decomposed_paths() {
        let mut field = PathField::new("Cafe\u{301}.MPR".into());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        field.edit(key(KeyCode::Home));
        for _ in 0..4 {
            field.edit(key(KeyCode::Right));
        }
        assert_eq!(field.cursor, "Cafe\u{301}".len());
        field.edit(key(KeyCode::Left));
        assert_eq!(field.cursor, "Caf".len());
        field.edit(key(KeyCode::Delete));
        assert_eq!(field.value, "Caf.MPR");

        field.insert("e\u{301}");
        field.edit(key(KeyCode::Backspace));
        assert_eq!(field.value, "Caf.MPR");
    }
}
