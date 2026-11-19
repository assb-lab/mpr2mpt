mod browser;
mod jobs;
mod path_field;
mod preview;
mod view;

use std::collections::BTreeSet;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use browser::{Browser, Entry, Kind};
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
use jobs::{BatchMessage, Report, RunningBatch};
use mpr2mpt::batch::BatchJob;
use path_field::PathField;
use preview::MptPreview;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

#[derive(Parser)]
#[command(
    name = "mpr2mpt-tui",
    version,
    about = "Browse and convert BioLogic measurements"
)]
struct Args {
    /// Initial file or directory
    input: Option<PathBuf>,
    /// Output MPT path for a single input
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,
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
        app.receive_results();
        if app.exit_when_done && app.running.is_none() {
            return Ok(());
        }
        terminal.draw(|frame| app.draw(frame))?;
        if app.running.is_some() && !event::poll(Duration::from_millis(75))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if app.key(key) {
                    return Ok(());
                }
            }
            Event::Paste(text) => app.paste(&text),
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

enum Detail {
    Empty,
    Mpr { path: PathBuf, bytes: u64 },
    Mpt(MptPreview),
    Folder(Vec<Entry>),
    Error(String),
}

enum PromptKind {
    Path,
    Output,
}

struct Prompt {
    kind: PromptKind,
    field: PathField,
}

struct App {
    browser: Browser,
    marked: BTreeSet<PathBuf>,
    detail: Detail,
    preview_column: usize,
    preview_row: usize,
    output: Option<PathBuf>,
    prompt: Option<Prompt>,
    report: Option<Report>,
    show_report: bool,
    running: Option<RunningBatch>,
    exit_when_done: bool,
    message: Option<String>,
}

impl App {
    fn new(args: Args) -> io::Result<Self> {
        let mut app = Self {
            browser: Browser::open(args.input.as_deref().unwrap_or_else(|| Path::new(".")))?,
            marked: BTreeSet::new(),
            detail: Detail::Empty,
            preview_column: 0,
            preview_row: 0,
            output: args.output.map(std::path::absolute).transpose()?,
            prompt: None,
            report: None,
            show_report: false,
            running: None,
            exit_when_done: false,
            message: None,
        };
        app.update_detail();
        Ok(app)
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.quit();
        }
        if self.prompt.is_some() {
            self.edit_prompt(key);
            return false;
        }

        self.message = None;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return self.quit(),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => self.parent(),
            KeyCode::Right | KeyCode::Char('l') => self.open_selection(false),
            KeyCode::Enter if key.kind == KeyEventKind::Press && self.show_report => {
                self.open_result();
            }
            KeyCode::Enter if key.kind == KeyEventKind::Press => self.open_selection(true),
            KeyCode::Down | KeyCode::Char('j') => self.walk(1),
            KeyCode::Up | KeyCode::Char('k') => self.walk(-1),
            KeyCode::Home => self.walk(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.walk(isize::MAX),
            KeyCode::Char(' ') if key.kind == KeyEventKind::Press => self.toggle_mark(),
            KeyCode::Char('x') => self.marked.clear(),
            KeyCode::Char('c') if key.kind == KeyEventKind::Press => self.start_conversion(),
            KeyCode::Char('g') => self.open_prompt(PromptKind::Path),
            KeyCode::Char('o') => self.open_prompt(PromptKind::Output),
            KeyCode::Char('r') => self.reload(),
            KeyCode::Tab if self.report.is_some() => self.show_report = !self.show_report,
            KeyCode::PageUp => self.move_details(-1, 0),
            KeyCode::PageDown => self.move_details(1, 0),
            KeyCode::Char('[') => self.move_details(0, -1),
            KeyCode::Char(']') => self.move_details(0, 1),
            _ => {}
        }
        false
    }

    fn quit(&mut self) -> bool {
        self.exit_when_done = self.running.is_some();
        !self.exit_when_done
    }

    fn walk(&mut self, distance: isize) {
        self.browser.walk(distance);
        self.update_detail();
    }

    fn parent(&mut self) {
        match self.browser.enter_parent() {
            Ok(Some(browser)) => self.set_browser(browser),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }

    fn open_selection(&mut self, convert_mpr: bool) {
        match self.browser.selected() {
            Some(entry) if entry.kind == Kind::Directory => self.open_path(entry.path.clone()),
            Some(entry) if convert_mpr && entry.kind == Kind::Mpr => self.start_conversion(),
            Some(entry) if entry.kind == Kind::Mpt => self.show_report = false,
            _ => {}
        }
    }

    fn set_browser(&mut self, browser: Browser) {
        self.browser = browser;
        self.show_report = false;
        self.update_detail();
    }

    fn open_path(&mut self, path: PathBuf) {
        match Browser::open(&path) {
            Ok(browser) => self.set_browser(browser),
            Err(error) => self.show_error(error),
        }
    }

    fn open_result(&mut self) {
        if let Some(report) = &self.report
            && let Some(Some(Ok(result))) = report.results.get(report.cursor)
        {
            match Browser::open_output(&result.output) {
                Ok(browser) => self.set_browser(browser),
                Err(error) => self.show_error(error),
            }
        }
    }

    fn reload(&mut self) {
        match self.browser.reload() {
            Ok(()) => self.update_detail(),
            Err(error) => self.show_error(error),
        }
    }

    fn update_detail(&mut self) {
        self.preview_column = 0;
        self.preview_row = 0;
        self.detail = match self.browser.selected() {
            Some(entry) => match entry.kind {
                Kind::Directory => match browser::entries(&entry.path) {
                    Ok(entries) => Detail::Folder(entries),
                    Err(error) => Detail::Error(error.to_string()),
                },
                Kind::Mpr => match entry.path.metadata() {
                    Ok(metadata) => Detail::Mpr {
                        path: entry.path.clone(),
                        bytes: metadata.len(),
                    },
                    Err(error) => Detail::Error(error.to_string()),
                },
                Kind::Mpt => match MptPreview::load(&entry.path) {
                    Ok(preview) => Detail::Mpt(preview),
                    Err(error) => Detail::Error(error.to_string()),
                },
            },
            None => Detail::Empty,
        };
    }

    fn toggle_mark(&mut self) {
        if let Some(entry) = self
            .browser
            .selected()
            .filter(|entry| entry.kind == Kind::Mpr)
            && !self.marked.remove(&entry.path)
        {
            self.marked.insert(entry.path.clone());
        }
    }

    fn open_prompt(&mut self, kind: PromptKind) {
        let value = match kind {
            PromptKind::Path => String::new(),
            PromptKind::Output => match self.output.as_deref() {
                Some(path) => match path
                    .to_str()
                    .filter(|text| !text.chars().any(char::is_control))
                {
                    Some(text) => text.to_owned(),
                    None => {
                        self.message =
                            Some("Type a new output path, or Esc to keep the current one.".into());
                        String::new()
                    }
                },
                None => String::new(),
            },
        };
        self.prompt = Some(Prompt {
            kind,
            field: PathField::new(value),
        });
    }

    fn paste(&mut self, text: &str) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        if text.chars().any(char::is_control) {
            self.message = Some("Paste one path without control characters.".into());
        } else {
            prompt.field.insert(text);
        }
    }

    fn edit_prompt(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter if key.kind == KeyEventKind::Press => {
                let Some(prompt) = self.prompt.take() else {
                    return;
                };
                let value = &prompt.field.value;
                match prompt.kind {
                    PromptKind::Path if !value.is_empty() => {
                        self.open_path(self.browser.directory.join(value));
                    }
                    PromptKind::Output => {
                        self.output =
                            (!value.is_empty()).then(|| self.browser.directory.join(value));
                    }
                    PromptKind::Path => {}
                }
            }
            _ => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.field.edit(key);
                }
            }
        }
    }

    fn start_conversion(&mut self) {
        if self.running.is_some() {
            return;
        }
        let mut checked: Vec<_> = self.marked.iter().cloned().collect();
        if checked.is_empty()
            && let Some(entry) = self
                .browser
                .selected()
                .filter(|entry| entry.kind == Kind::Mpr)
        {
            checked.push(entry.path.clone());
        }
        if checked.is_empty() {
            self.message = Some("Select an MPR to convert.".into());
            return;
        }
        if checked.len() > 1 && self.output.is_some() {
            self.message = Some("Clear the single-file output with o before a batch.".into());
            return;
        }
        let jobs: Vec<_> = checked
            .iter()
            .map(|input| BatchJob {
                input: input.clone(),
                output: self.output.clone(),
            })
            .collect();
        match RunningBatch::start(jobs) {
            Ok(worker) => {
                self.report = Some(Report::new(checked));
                self.running = Some(worker);
                self.show_report = true;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn receive_results(&mut self) {
        while let Some(running) = &self.running {
            match running.receiver.try_recv() {
                Ok(BatchMessage::Outcome(outcome)) => {
                    if let Some(report) = &mut self.report {
                        if outcome.result.is_ok() {
                            self.marked.remove(&report.inputs[outcome.index]);
                        }
                        report.results[outcome.index] = Some(outcome.result);
                    }
                }
                Ok(BatchMessage::Finished(error)) => {
                    drop(self.running.take());
                    if let Some(error) = error {
                        self.fail_batch(&error);
                    } else {
                        self.reload();
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    drop(self.running.take());
                    self.fail_batch("The conversion worker stopped before reporting all results.");
                    self.reload();
                }
            }
        }
    }

    fn fail_batch(&mut self, error: &str) {
        if let Some(report) = &mut self.report {
            report.failure = Some(safe_text(error));
        }
    }

    fn move_details(&mut self, rows: isize, columns: isize) {
        if self.show_report {
            if let Some(report) = &mut self.report {
                report.cursor = shift(report.cursor, rows, report.inputs.len());
            }
        } else if let Detail::Mpt(preview) = &self.detail {
            self.preview_row = shift(self.preview_row, rows, preview.rows());
            self.preview_column = shift(self.preview_column, columns, preview.columns());
        }
    }

    fn show_error(&mut self, error: impl std::fmt::Display) {
        self.message = Some(safe_text(&error.to_string()));
    }
}

fn safe_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { '�' } else { ch })
        .collect()
}

fn shift(cursor: usize, delta: isize, len: usize) -> usize {
    cursor
        .saturating_add_signed(delta)
        .min(len.saturating_sub(1))
}
