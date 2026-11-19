use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use mpr2mpt::convert;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

#[derive(Parser)]
#[command(
    name = "mpr2mpt-tui",
    version,
    about = "Interactively convert a BioLogic EC-Lab MPR file"
)]
struct Args {
    /// Initial input MPR path
    input: Option<String>,
    /// Initial output MPT path
    #[arg(short, long, value_name = "PATH")]
    output: Option<String>,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "mpr2mpt-tui needs an interactive terminal",
        ));
    }
    let mut app = App::new(args)?;
    enable_raw_mode()?;
    let _session = TerminalSession;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match app.key(key) {
                Action::Quit => return Ok(()),
                Action::Convert => {
                    app.status = Some(("Converting…".into(), Color::Cyan));
                    terminal.draw(|frame| app.draw(frame))?;
                    app.convert();
                }
                Action::Continue => {}
            },
            Event::Paste(text) => {
                if text.chars().any(char::is_control) {
                    app.status = Some((
                        "Paste a single path without control characters.".into(),
                        Color::Red,
                    ));
                } else {
                    app.fields[app.focus].insert(&text);
                }
            }
            _ => {}
        }
    }
}

struct TerminalSession;

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            Show
        );
    }
}

struct App {
    fields: [PathField; 2],
    focus: usize,
    status: Option<(String, Color)>,
}

enum Action {
    Continue,
    Convert,
    Quit,
}

impl App {
    fn new(args: Args) -> io::Result<Self> {
        let paths = [
            args.input.unwrap_or_default(),
            args.output.unwrap_or_default(),
        ];
        if paths.iter().any(|path| path.chars().any(char::is_control)) {
            return Err(io::Error::other(
                "paths must not contain control characters",
            ));
        }
        Ok(Self {
            fields: paths.map(PathField::new),
            focus: 0,
            status: None,
        })
    }

    fn key(&mut self, key: KeyEvent) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Action::Quit,
            KeyCode::Char('c') if control => return Action::Quit,
            KeyCode::Enter if key.kind == KeyEventKind::Press => return Action::Convert,
            KeyCode::Tab | KeyCode::BackTab if key.kind == KeyEventKind::Press => {
                self.focus = 1 - self.focus;
            }
            _ => self.fields[self.focus].edit(key),
        }
        Action::Continue
    }

    fn convert(&mut self) {
        let [input, output] = &self.fields;
        if input.value.is_empty() {
            self.status = Some(("Enter the input MPR path.".into(), Color::Red));
            return;
        }
        let output = (!output.value.is_empty()).then(|| Path::new(&output.value));
        self.status = Some(match convert(Path::new(&input.value), output) {
            Ok(report) => (
                format!(
                    "Wrote {} ({} rows, {} columns)",
                    report.output.display(),
                    report.rows,
                    report.columns
                ),
                Color::Green,
            ),
            Err(error) => (format!("Error: {error}"), Color::Red),
        });
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area().inner(Margin::new(1, 1));
        let [heading, input, output, result, help] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(area);

        frame.render_widget(Line::from("MPR → MPT").fg(Color::Cyan).bold(), heading);
        self.fields[0].draw(
            frame,
            input,
            "Input MPR",
            "Enter or paste a path",
            self.focus == 0,
        );
        self.fields[1].draw(
            frame,
            output,
            "Output MPT",
            "Same folder and name as input",
            self.focus == 1,
        );
        if let Some((message, color)) = &self.status {
            frame.render_widget(
                Paragraph::new(message.as_str())
                    .fg(*color)
                    .wrap(Wrap { trim: false }),
                result,
            );
        }
        frame.render_widget(
            Paragraph::new("Tab  Switch path    Enter  Convert    Esc  Quit")
                .wrap(Wrap { trim: true })
                .dark_gray(),
            help,
        );
    }
}

struct PathField {
    value: String,
    cursor: usize,
}

impl PathField {
    fn new(value: String) -> Self {
        Self {
            cursor: value.len(),
            value,
        }
    }

    fn insert(&mut self, text: &str) {
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

    fn edit(&mut self, key: KeyEvent) {
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

    fn draw(&self, frame: &mut Frame, area: Rect, title: &str, placeholder: &str, focused: bool) {
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
