mod browser;
mod clipboard;
mod footer;
mod jobs;
mod path_field;
mod preview;
mod trash;
mod view;

use std::collections::{BTreeSet, HashMap};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use browser::{Browser, Entry, Kind};
use clap::Parser;
use clipboard::PathPart;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use jobs::{BatchMessage, PendingOverwrite, Report, RunningBatch};
use mpr2mpt::batch::BatchJob;
use mpr2mpt::overwrite::OverwriteApproval;
use path_field::PathField;
use preview::{InspectionView, MptPreview, TextPreview};
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
    /// Use standard text symbols instead of Nerd Font icons
    #[arg(long)]
    plain_icons: bool,
    /// Create a NUL-terminated directory file on Shift+Q for shell integration
    #[arg(long, value_name = "PATH")]
    cwd_file: Option<PathBuf>,
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
    let cwd_file = args.cwd_file.clone();
    let mut app = App::new(args)?;
    run_terminal(&mut app)?;
    if let Some(directory) = app.exit_directory {
        if let Some(destination) = cwd_file {
            write_exit_directory(&destination, &directory)?;
        } else {
            println!("{}", browser::terminal_text(&directory));
        }
    }
    Ok(())
}

fn write_exit_directory(destination: &Path, directory: &Path) -> io::Result<()> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut answer = tempfile::NamedTempFile::new_in(parent)?;
    answer.write_all(directory.as_os_str().as_encoded_bytes())?;
    answer.write_all(b"\0")?;
    answer.persist_noclobber(destination)?;
    Ok(())
}

fn run_terminal(app: &mut App) -> io::Result<()> {
    enable_raw_mode()?;
    let _session = TerminalSession;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    loop {
        app.receive_results();
        app.poll_launchers();
        if app.exit_when_done && app.running.is_none() {
            return Ok(());
        }
        terminal.draw(|frame| app.draw(frame))?;
        if (app.running.is_some() || !app.launchers.is_empty())
            && !event::poll(Duration::from_millis(75))?
        {
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
    Inspection(InspectionView),
    Mpt(MptPreview),
    Text(TextPreview),
    Folder(Vec<Entry>),
    Error(String),
}

enum PromptKind {
    Path,
    Output {
        input: PathBuf,
    },
    Filter {
        previous: String,
        focus: Option<PathBuf>,
    },
}

struct Prompt {
    kind: PromptKind,
    field: PathField,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prefix {
    Jump,
    Yank,
    CopyPath,
}

struct App {
    plain_icons: bool,
    browser: Browser,
    marked: BTreeSet<PathBuf>,
    detail: Detail,
    preview_focused: bool,
    preview_column: usize,
    preview_row: usize,
    output: Option<PathBuf>,
    output_target: Option<PathBuf>,
    prompt: Option<Prompt>,
    trash_target: Option<trash::Target>,
    prefix: Option<Prefix>,
    clipboard: Vec<PathBuf>,
    pending_overwrite: Option<PendingOverwrite>,
    report: Option<Report>,
    show_report: bool,
    running: Option<RunningBatch>,
    launchers: Vec<(Child, Instant)>,
    exit_when_done: bool,
    exit_directory: Option<PathBuf>,
    message: Option<String>,
}

const LAUNCHER_WATCH_TIMEOUT: Duration = Duration::from_secs(3);

impl App {
    fn new(args: Args) -> io::Result<Self> {
        let initial_file = args
            .input
            .as_deref()
            .filter(|path| path.is_file())
            .and_then(|path| path.canonicalize().ok());
        let browser = Browser::open(args.input.as_deref().unwrap_or_else(|| Path::new(".")))?;
        let mut output = args.output.map(std::path::absolute).transpose()?;
        let output_target = if output.is_some() && args.input.as_deref().is_some_and(Path::is_file)
        {
            match initial_file.or_else(|| browser.selected().map(|entry| entry.path.clone())) {
                Some(path) => Some(path),
                None => {
                    output = None;
                    None
                }
            }
        } else {
            None
        };
        let mut app = Self {
            plain_icons: args.plain_icons,
            browser,
            marked: BTreeSet::new(),
            detail: Detail::Empty,
            preview_focused: false,
            preview_column: 0,
            preview_row: 0,
            output,
            output_target,
            prompt: None,
            trash_target: None,
            prefix: None,
            clipboard: Vec::new(),
            pending_overwrite: None,
            report: None,
            show_report: false,
            running: None,
            launchers: Vec::new(),
            exit_when_done: false,
            exit_directory: None,
            message: None,
        };
        app.update_detail();
        Ok(app)
    }

    fn planned_output(&self, input: &Path) -> Option<PathBuf> {
        let output = self.output.as_ref()?;
        match &self.output_target {
            Some(target) if target != input => None,
            _ => Some(output.clone()),
        }
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.quit();
        }
        if self.prompt.is_some() {
            self.edit_prompt(key);
            return false;
        }
        if self.trash_target.is_some() {
            self.confirm_trash(key);
            return false;
        }
        if self.pending_overwrite.is_some() {
            self.confirm_overwrite(key);
            return false;
        }

        self.message = None;
        if self.prefix.is_some() && key.kind == KeyEventKind::Repeat {
            return false;
        }
        if let Some(prefix) = self.prefix.take()
            && self.complete_prefix(prefix, key)
        {
            return false;
        }
        match key.code {
            KeyCode::Esc if self.preview_focused => self.preview_focused = false,
            KeyCode::Esc if !self.show_report && matches!(self.detail, Detail::Inspection(_)) => {
                self.update_detail()
            }
            KeyCode::Esc if !self.browser.filter.is_empty() => self.set_filter(String::new()),
            KeyCode::Esc | KeyCode::Char('q') => return self.quit(),
            KeyCode::Char('Q') => {
                self.exit_directory = Some(self.browser.directory.clone());
                return self.quit();
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                if self.preview_focused {
                    self.preview_focused = false;
                } else {
                    self.parent();
                }
            }
            KeyCode::Right | KeyCode::Char('l') => self.open_selection(),
            KeyCode::Enter if key.kind == KeyEventKind::Press && self.show_report => {
                self.open_result();
            }
            KeyCode::Enter if key.kind == KeyEventKind::Press => self.open_selection(),
            KeyCode::Down | KeyCode::Char('j') => self.walk(1),
            KeyCode::Up | KeyCode::Char('k') => self.walk(-1),
            KeyCode::Char('J') => self.move_metadata(1),
            KeyCode::Char('K') => self.move_metadata(-1),
            KeyCode::Home => self.walk(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.walk(isize::MAX),
            KeyCode::Char(' ') if key.kind == KeyEventKind::Press => self.toggle_mark(),
            KeyCode::Char('x') => self.marked.clear(),
            KeyCode::Char('e') if key.kind == KeyEventKind::Press && key.modifiers.is_empty() => {
                self.start_conversion()
            }
            KeyCode::Char('E')
                if key.kind == KeyEventKind::Press
                    && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                self.prompt_named_conversion();
            }
            KeyCode::Char('c') if key.kind == KeyEventKind::Press && key.modifiers.is_empty() => {
                self.prefix = Some(Prefix::CopyPath)
            }
            KeyCode::Char('v') if key.kind == KeyEventKind::Press => self.toggle_inspection(),
            KeyCode::Char('g') if key.kind == KeyEventKind::Press => {
                self.prefix = Some(Prefix::Jump)
            }
            KeyCode::Char('y') if key.kind == KeyEventKind::Press => {
                self.prefix = Some(Prefix::Yank)
            }
            KeyCode::Char('p') if key.kind == KeyEventKind::Press => self.paste_files(),
            KeyCode::Char('.') if key.kind == KeyEventKind::Press => {
                self.browser
                    .set_visibility(self.browser.restricted, !self.browser.show_hidden);
                self.show_report = false;
                self.update_detail();
            }
            KeyCode::Char('f') if key.kind == KeyEventKind::Press => {
                self.open_prompt(PromptKind::Filter {
                    previous: self.browser.filter.clone(),
                    focus: self.browser.selected().map(|entry| entry.path.clone()),
                })
            }
            KeyCode::Char('m') if key.kind == KeyEventKind::Press => {
                self.browser
                    .set_visibility(!self.browser.restricted, self.browser.show_hidden);
                self.show_report = false;
                self.update_detail();
            }
            KeyCode::Char('d') if key.kind == KeyEventKind::Press => self.request_trash(),
            KeyCode::Char('o') if key.kind == KeyEventKind::Press && key.modifiers.is_empty() => {
                self.launch_selected();
            }
            KeyCode::Char('r') => self.reload(),
            KeyCode::Tab if self.report.is_some() => {
                self.preview_focused = false;
                self.show_report = !self.show_report;
            }
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
        if self.preview_focused {
            self.move_details(distance, 0);
            return;
        }
        self.show_report = false;
        self.browser.walk(distance);
        self.update_detail();
    }

    fn complete_prefix(&mut self, prefix: Prefix, key: KeyEvent) -> bool {
        if key.code == KeyCode::Esc {
            return true;
        }
        if !key.modifiers.is_empty() {
            return prefix == Prefix::CopyPath;
        }
        match prefix {
            Prefix::Jump => self.jump(key.code),
            Prefix::Yank if key.code == KeyCode::Char('y') => {
                self.yank();
                true
            }
            Prefix::Yank => false,
            Prefix::CopyPath => {
                self.copy_path(key.code);
                true
            }
        }
    }

    fn jump(&mut self, key: KeyCode) -> bool {
        let path = match key {
            KeyCode::Char('g') => {
                self.walk(isize::MIN);
                return true;
            }
            KeyCode::Char(' ') => {
                self.open_prompt(PromptKind::Path);
                return true;
            }
            KeyCode::Char('d') => trash::home().map(|path| path.join("Downloads")),
            KeyCode::Char('h') => trash::home(),
            KeyCode::Char('t') => trash::directory(),
            _ => return false,
        };
        match path {
            Ok(path) => self.open_path(path),
            Err(error) => self.show_error(error),
        }
        true
    }

    fn copy_path(&mut self, key: KeyCode) {
        let part = match key {
            KeyCode::Char('c') => PathPart::Absolute,
            KeyCode::Char('d') => PathPart::Parent,
            KeyCode::Char('f') => PathPart::Name,
            KeyCode::Char('n') => PathPart::Stem,
            _ => return,
        };
        let Some(entry) = self.browser.selected() else {
            self.message = Some("Select an item to copy its path.".into());
            return;
        };
        match part.text(&entry.path).and_then(clipboard::copy_text) {
            Ok(()) => self.message = Some("Copied to system clipboard.".into()),
            Err(error) => self.show_error(error),
        }
    }

    fn yank(&mut self) {
        let sources: Vec<_> = if self.marked.is_empty() {
            self.browser
                .selected()
                .filter(|entry| entry.path != self.browser.directory)
                .map(|entry| entry.path.clone())
                .into_iter()
                .collect()
        } else {
            self.marked.iter().cloned().collect()
        };
        if sources.is_empty() {
            self.message = Some("Select a file or child folder to copy.".into());
            return;
        }
        self.message = Some(format!("{} item(s) ready to paste.", sources.len()));
        self.clipboard = sources;
    }

    fn paste_files(&mut self) {
        if self.clipboard.is_empty() {
            self.message = Some("Use yy to copy an item first.".into());
            return;
        }
        let mut copied = 0;
        let mut first_failure = None;
        for source in &self.clipboard {
            match clipboard::copy_into(source, &self.browser.directory) {
                Ok(_) => copied += 1,
                Err(error) => {
                    first_failure.get_or_insert_with(|| {
                        format!(
                            "{}: {}",
                            browser::terminal_text(source),
                            safe_text(&error.to_string())
                        )
                    });
                }
            }
        }
        let failed = self.clipboard.len() - copied;
        self.message = Some(match first_failure {
            Some(reason) => format!("{copied} pasted, {failed} failed. {reason}"),
            None => format!("{copied} item(s) pasted."),
        });
        self.show_report = false;
        self.reload();
    }

    fn request_trash(&mut self) {
        if self.running.is_some() {
            self.message =
                Some("Wait for the active conversion before moving files to Trash.".into());
        } else if let Some(entry) = self.browser.selected() {
            if entry.path == self.browser.directory {
                self.message = Some("Select a file or child folder to move to Trash.".into());
                return;
            }
            match trash::Target::new(entry.path.clone()) {
                Ok(target) => self.trash_target = Some(target),
                Err(error) => self.show_error(error),
            }
        }
    }

    fn confirm_trash(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter
                if key.kind == KeyEventKind::Press && key.modifiers.is_empty() =>
            {
                let Some(target) = self.trash_target.take() else {
                    return;
                };
                match target.move_to_trash() {
                    Ok(()) => {
                        // Clear marks for a moved file or for anything inside a moved directory.
                        self.marked
                            .retain(|marked| !marked.starts_with(&target.path));
                        self.show_report = false;
                        self.message = Some("Moved to Trash.".into());
                        self.reload();
                    }
                    Err(error) => self.show_error(error),
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => self.trash_target = None,
            _ => {}
        }
    }

    fn parent(&mut self) {
        match self.browser.enter_parent() {
            Ok(Some(browser)) => self.set_browser(browser),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }

    fn open_selection(&mut self) {
        match self.browser.selected() {
            Some(entry) if entry.kind == Kind::Directory => {
                if entry.path != self.browser.directory {
                    self.open_path(entry.path.clone());
                }
            }
            Some(entry) => {
                if entry.kind == Kind::Mpr && !matches!(self.detail, Detail::Inspection(_)) {
                    self.open_inspection();
                }
                if matches!(
                    self.detail,
                    Detail::Inspection(_) | Detail::Mpt(_) | Detail::Text(_)
                ) {
                    self.preview_focused = true;
                    self.show_report = false;
                }
            }
            None => {}
        }
    }

    fn set_browser(&mut self, mut browser: Browser) {
        browser.set_visibility(
            self.browser.restricted,
            self.browser.show_hidden || browser.show_hidden,
        );
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
        if let Some(path) = self.selected_output() {
            match Browser::open_output(path) {
                Ok(browser) => self.set_browser(browser),
                Err(error) => self.show_error(error),
            }
        }
    }

    fn selected_output(&self) -> Option<&Path> {
        let report = self.report.as_ref()?;
        let result = report.results.get(report.cursor)?.as_ref()?.as_ref().ok()?;
        Some(result.output.as_path())
    }

    fn launch_selected(&mut self) {
        let path = if self.show_report {
            self.selected_output()
        } else {
            self.browser.selected().map(|entry| entry.path.as_path())
        };
        let Some(path) = path else {
            self.message = Some("Select an item to open.".into());
            return;
        };
        let program = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "linux") {
            "xdg-open"
        } else {
            self.message =
                Some("Opening in the default app is unavailable on this platform.".into());
            return;
        };
        match Command::new(program)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                self.launchers.push((child, Instant::now()));
                self.message = Some("Sent to the default app.".into());
            }
            Err(error) => self.show_error(format!("Cannot start {program}: {error}")),
        }
    }

    fn poll_launchers(&mut self) {
        let mut error = None;
        let mut remaining = Vec::with_capacity(self.launchers.len());
        for (mut child, started) in self.launchers.drain(..) {
            match child.try_wait() {
                Ok(None) if started.elapsed() < LAUNCHER_WATCH_TIMEOUT => {
                    remaining.push((child, started));
                }
                Ok(None) => {
                    // Detach a reaper thread so long-lived launchers do not become zombies.
                    let _ = std::thread::Builder::new()
                        .name("mpr2mpt-reap".into())
                        .spawn(move || {
                            let _ = child.wait();
                        });
                }
                Ok(Some(status)) if status.success() => {}
                Ok(Some(status)) => {
                    error = Some(format!("Default app launcher exited with {status}."))
                }
                Err(wait_error) => {
                    error = Some(format!(
                        "Cannot check the default app launcher: {wait_error}"
                    ))
                }
            }
        }
        self.launchers = remaining;
        if let Some(error) = error {
            self.show_error(error);
        }
    }

    fn reload(&mut self) {
        let selected_before = self.browser.selected().map(|entry| entry.path.clone());
        let was_focused = self.preview_focused;
        let previous_inspection = match std::mem::replace(&mut self.detail, Detail::Empty) {
            Detail::Inspection(view) => Some(view),
            other => {
                self.detail = other;
                None
            }
        };
        let (row, column) = (self.preview_row, self.preview_column);
        if let Err(error) = self.browser.reload() {
            if let Some(view) = previous_inspection {
                self.detail = Detail::Inspection(view);
            }
            self.show_error(error);
            return;
        }
        self.update_detail();
        let Some(path) = selected_before else {
            return;
        };
        let still_selected = self
            .browser
            .selected()
            .filter(|entry| entry.path == path);
        if let (Some(previous), Some(entry)) = (previous_inspection, still_selected)
            && entry.kind == Kind::Mpr
        {
            let current_sig = preview::file_signature(&entry.path);
            if previous.source_meta.is_some() && previous.source_meta == current_sig {
                self.detail = Detail::Inspection(previous);
                self.preview_focused = was_focused;
                self.restore_preview_position(row, column);
            } else if let Ok(inspection) = mpr2mpt::inspect(&entry.path) {
                let mut view = InspectionView::from(inspection);
                view.source_meta = current_sig;
                view.metadata_start = shift(previous.metadata_start, 0, view.metadata.len());
                self.detail = Detail::Inspection(view);
                self.preview_focused = was_focused;
                self.restore_preview_position(row, column);
            }
        } else if was_focused
            && still_selected.is_some()
            && matches!(self.detail, Detail::Mpt(_) | Detail::Text(_))
        {
            self.preview_focused = true;
            self.restore_preview_position(row, column);
        }
    }

    fn restore_preview_position(&mut self, row: usize, column: usize) {
        let (row_count, column_count) = match &self.detail {
            Detail::Mpt(preview) => (preview.rows(), preview.columns()),
            Detail::Inspection(preview) => (preview.table.rows(), preview.table.columns()),
            Detail::Text(preview) => (preview.rows(), preview.columns()),
            _ => return,
        };
        self.preview_row = shift(0, row as isize, row_count);
        self.preview_column = shift(0, column as isize, column_count);
    }

    fn update_detail(&mut self) {
        self.preview_focused = false;
        self.preview_column = 0;
        self.preview_row = 0;
        self.detail = match self.browser.selected() {
            Some(entry) => match entry.kind {
                Kind::Directory => match browser::entries(
                    &entry.path,
                    self.browser.restricted,
                    self.browser.show_hidden,
                ) {
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
                Kind::Mps | Kind::Other => match TextPreview::load(&entry.path) {
                    Ok(Some(preview)) => Detail::Text(preview),
                    Ok(None) => Detail::Empty,
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
        let value = match &kind {
            PromptKind::Path | PromptKind::Filter { .. } => String::new(),
            PromptKind::Output { input } => match self.planned_output(input) {
                Some(path) => match path
                    .to_str()
                    .filter(|text| !text.chars().any(char::is_control))
                {
                    Some(text) => text.to_owned(),
                    None => {
                        self.message = Some("Type an output path, or press Esc to cancel.".into());
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
        } else if !text.is_empty() {
            prompt.field.insert(text);
            self.refresh_filter();
        }
    }

    fn refresh_filter(&mut self) {
        if let Some(prompt) = &self.prompt
            && matches!(prompt.kind, PromptKind::Filter { .. })
        {
            self.set_filter(prompt.field.value.clone());
        }
    }

    fn set_filter(&mut self, value: String) {
        self.browser.set_filter(value);
        self.show_report = false;
        self.update_detail();
    }

    fn edit_prompt(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if let Some(Prompt {
                    kind: PromptKind::Filter { previous, focus },
                    ..
                }) = self.prompt.take()
                {
                    self.browser.set_filter(previous);
                    if let Some(path) = focus {
                        self.browser.focus_path(&path);
                    }
                    self.update_detail();
                }
            }
            KeyCode::Enter if key.kind == KeyEventKind::Press => {
                let Some(prompt) = self.prompt.take() else {
                    return;
                };
                let value = &prompt.field.value;
                match prompt.kind {
                    PromptKind::Path if !value.is_empty() => {
                        self.open_path(self.browser.directory.join(value));
                    }
                    PromptKind::Output { input } => {
                        let output = (!value.is_empty()).then(|| input.with_file_name(value));
                        self.queue_conversion(vec![BatchJob { input, output }]);
                    }
                    PromptKind::Filter { .. } => self.set_filter(prompt.field.value),
                    PromptKind::Path => {}
                }
            }
            _ => {
                if let Some(prompt) = &mut self.prompt
                    && prompt.field.edit(key)
                {
                    self.refresh_filter();
                }
            }
        }
    }

    fn prompt_named_conversion(&mut self) {
        if self.running.is_some() {
            self.message = Some("Wait for the active conversion before starting another.".into());
            return;
        }
        if let Some(entry) = self
            .browser
            .selected()
            .filter(|entry| entry.kind == Kind::Mpr)
        {
            self.open_prompt(PromptKind::Output {
                input: entry.path.clone(),
            });
        } else {
            self.message = Some("Select an MPR to convert.".into());
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
        if checked.len() > 1 && checked.iter().any(|input| self.planned_output(input).is_some()) {
            self.message =
                Some("The -o option requires one input. Restart without -o for a batch.".into());
            return;
        }
        let jobs: Vec<_> = checked
            .iter()
            .map(|input| BatchJob {
                input: input.clone(),
                output: self.planned_output(input),
            })
            .collect();
        self.queue_conversion(jobs);
    }

    fn queue_conversion(&mut self, jobs: Vec<BatchJob>) {
        let pending = PendingOverwrite::scan(jobs);
        if pending.existing.is_empty() {
            self.run_jobs(pending.jobs, HashMap::new());
        } else {
            self.pending_overwrite = Some(pending);
        }
    }

    fn confirm_overwrite(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y' | 's')
                if key.kind == KeyEventKind::Press && key.modifiers.is_empty() =>
            {
                let Some(pending) = self.pending_overwrite.take() else {
                    return;
                };
                let (jobs, approvals) = if key.code == KeyCode::Char('y') {
                    pending.approvals()
                } else {
                    (pending.jobs, HashMap::new())
                };
                self.run_jobs(jobs, approvals);
            }
            KeyCode::Esc | KeyCode::Char('n') => self.pending_overwrite = None,
            KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k') => {
                if let Some(pending) = &mut self.pending_overwrite {
                    let distance = if matches!(key.code, KeyCode::Up | KeyCode::Char('k')) {
                        -1
                    } else {
                        1
                    };
                    pending.cursor = shift(pending.cursor, distance, pending.existing.len());
                }
            }
            _ => {}
        }
    }

    fn run_jobs(&mut self, jobs: Vec<BatchJob>, approvals: HashMap<usize, OverwriteApproval>) {
        let inputs = jobs.iter().map(|job| job.input.clone()).collect();
        match RunningBatch::start(jobs, approvals) {
            Ok(worker) => {
                self.preview_focused = false;
                self.report = Some(Report::new(inputs));
                self.running = Some(worker);
                self.show_report = true;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn toggle_inspection(&mut self) {
        if matches!(self.detail, Detail::Inspection(_)) {
            if self.show_report {
                self.show_report = false;
            } else {
                self.update_detail();
            }
        } else {
            self.open_inspection();
        }
    }

    fn open_inspection(&mut self) {
        if self.running.is_some() {
            self.message = Some("Wait for the active conversion before opening a preview.".into());
            return;
        }
        let Some(entry) = self
            .browser
            .selected()
            .filter(|entry| entry.kind == Kind::Mpr)
        else {
            self.message = Some("Select an MPR to preview.".into());
            return;
        };
        match mpr2mpt::inspect(&entry.path) {
            Ok(inspection) => {
                let mut view = InspectionView::from(inspection);
                view.source_meta = preview::file_signature(&entry.path);
                self.detail = Detail::Inspection(view);
                self.show_report = false;
                self.preview_column = 0;
                self.preview_row = 0;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn move_metadata(&mut self, distance: isize) {
        if self.preview_focused
            && let Detail::Inspection(preview) = &mut self.detail
        {
            preview.metadata_start =
                shift(preview.metadata_start, distance, preview.metadata.len());
        }
    }

    fn receive_results(&mut self) {
        while let Some(running) = &self.running {
            match running.receiver.try_recv() {
                Ok(BatchMessage::Outcome(outcome)) => {
                    if let Some(report) = &mut self.report {
                        if let Ok(conversion) = &outcome.result {
                            self.marked.remove(&report.inputs[outcome.index]);
                            if self.output.as_deref() == Some(conversion.output.as_path()) {
                                self.output = None;
                                self.output_target = None;
                            }
                        }
                        report.results[outcome.index] = Some(outcome.result);
                    }
                }
                Ok(BatchMessage::Finished(error)) => {
                    drop(self.running.take());
                    if let Some(error) = error {
                        self.fail_batch(&error);
                    }
                    self.reload();
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
        } else {
            let (row_count, column_count) = match &self.detail {
                Detail::Mpt(preview) => (preview.rows(), preview.columns()),
                Detail::Inspection(preview) => (preview.table.rows(), preview.table.columns()),
                Detail::Text(preview) => (preview.rows(), preview.columns()),
                _ => return,
            };
            self.preview_row = shift(self.preview_row, rows, row_count);
            self.preview_column = shift(self.preview_column, columns, column_count);
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

#[cfg(test)]
mod navigation_tests {
    use super::*;
    use std::fs;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn app_at(path: &Path) -> App {
        App::new(Args {
            input: Some(path.into()),
            output: None,
            plain_icons: false,
            cwd_file: None,
        })
        .unwrap()
    }

    #[test]
    fn filters_live_and_restores_the_list_when_cancelled() {
        let scratch = tempfile::tempdir().unwrap();
        fs::write(scratch.path().join("Alpha.mpr"), []).unwrap();
        fs::write(scratch.path().join("beta.MPS"), []).unwrap();
        let mut app = app_at(scratch.path());
        assert_eq!(app.browser.files.len(), 3);
        app.key(key('f'));
        app.paste("BETA");
        assert_eq!(app.browser.files.len(), 2);
        assert_eq!(app.browser.selected().unwrap().label, "beta.MPS");
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.browser.files.len(), 3);
    }

    #[test]
    fn reopens_filter_blank_and_clears_it_on_empty_enter() {
        let scratch = tempfile::tempdir().unwrap();
        fs::write(scratch.path().join("alpha.mpr"), []).unwrap();
        fs::write(scratch.path().join("beta.mpr"), []).unwrap();
        let mut app = app_at(scratch.path());
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        app.key(key('f'));
        app.key(key('b'));
        assert_eq!(app.browser.files.len(), 2);
        assert_eq!(app.browser.selected().unwrap().label, "beta.mpr");
        app.key(enter);

        app.key(key('f'));
        assert!(app.prompt.as_ref().unwrap().field.value.is_empty());
        app.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.browser.files.len(), 2);

        app.key(key('l'));
        assert_eq!(app.browser.selected().unwrap().label, "alpha.mpr");
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.browser.selected().unwrap().label, "beta.mpr");
        assert_eq!(app.browser.files.len(), 2);

        app.key(key('f'));
        app.key(enter);
        assert!(app.prompt.is_none());
        assert!(app.browser.filter.is_empty());
        assert_eq!(app.browser.files.len(), 3);
    }

    #[test]
    fn moves_to_top_with_gg_and_dismisses_results_on_navigation() {
        let scratch = tempfile::tempdir().unwrap();
        fs::write(scratch.path().join("a.mpr"), []).unwrap();
        fs::write(scratch.path().join("b.mpr"), []).unwrap();
        let mut app = app_at(scratch.path());
        app.show_report = true;
        app.key(key('j'));
        assert!(!app.show_report);
        assert_eq!(app.browser.selected().unwrap().label, "b.mpr");
        app.key(key('g'));
        assert!(app.prompt.is_none());
        assert_eq!(app.browser.selected().unwrap().label, "b.mpr");
        app.key(key('g'));
        assert_eq!(app.browser.selected().unwrap().label, ".");
        app.key(key('g'));
        assert!(!app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.prefix.is_none());
    }

    #[test]
    fn current_directory_row_is_first_and_safe_inside_hidden_directories() {
        let scratch = tempfile::tempdir().unwrap();
        let directory = scratch.path().join(".measurements");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("sample.mpr"), []).unwrap();
        let mut app = app_at(&directory);

        app.key(key('g'));
        app.key(key('g'));
        let entry = app.browser.selected().unwrap();
        assert_eq!(entry.label, ".");
        assert_eq!(entry.path, directory.canonicalize().unwrap());
        assert_eq!(entry.kind, Kind::Directory);

        app.key(key('d'));
        assert!(app.trash_target.is_none());
        app.key(key('y'));
        app.key(key('y'));
        assert!(app.clipboard.is_empty());
        app.key(key('l'));
        assert_eq!(app.browser.selected().unwrap().label, ".");

        app.key(key('f'));
        app.paste("sample");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.browser.selected().unwrap().label, "sample.mpr");
        app.key(key('g'));
        app.key(key('g'));
        assert_eq!(app.browser.selected().unwrap().label, ".");
    }

    #[test]
    fn right_focuses_preview_and_left_returns_to_the_same_file() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("selected.mpt");
        fs::create_dir(scratch.path().join("folder")).unwrap();
        fs::write(
            &path,
            b"EC-Lab ASCII FILE\nNb header lines : 3\ntime/s\n1\n2\n3\n",
        )
        .unwrap();
        let mut app = app_at(&path);
        let selected = app.browser.selected().unwrap().path.clone();

        app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.key(key('j'));
        assert_eq!(app.preview_row, 1);
        fs::write(scratch.path().join("another.mpt"), []).unwrap();
        app.reload();
        assert_eq!(app.preview_row, 1);
        app.key(key('G'));
        assert_eq!(app.preview_row, 2);
        app.key(key('k'));
        assert_eq!(app.preview_row, 1);
        app.key(key('g'));
        app.key(key('g'));
        assert_eq!(app.preview_row, 0);
        assert_eq!(app.browser.selected().unwrap().path, selected);

        app.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.browser.selected().unwrap().path, selected);
        app.key(key('g'));
        app.key(key('g'));
        assert_eq!(app.browser.selected().unwrap().kind, Kind::Directory);
    }

    #[test]
    fn previews_named_text_inputs_including_mps_without_treating_them_as_mpr() {
        let scratch = tempfile::tempdir().unwrap();
        for suffix in ["mps", "TXT", "md", "csv", "tsv"] {
            let path = scratch.path().join("sample").with_extension(suffix);
            fs::write(&path, b"first line\nsecond line\n").unwrap();
            let mut app = app_at(&path);
            assert_eq!(
                app.browser.selected().unwrap().label,
                format!("sample.{suffix}")
            );
            assert_ne!(app.browser.selected().unwrap().kind, Kind::Mpr);
            app.key(key('l'));
            app.key(key('G'));
            assert_eq!(app.preview_row, 1);
        }
    }

    #[test]
    fn previews_text_without_requiring_a_known_extension() {
        let scratch = tempfile::tempdir().unwrap();
        for name in [
            "Cargo.toml",
            "config.yaml",
            "data.json",
            "main.rs",
            "Makefile",
        ] {
            let path = scratch.path().join(name);
            fs::write(&path, b"first line\nsecond line\n").unwrap();
            let mut app = app_at(&path);
            assert_eq!(app.browser.selected().unwrap().kind, Kind::Other);
            assert!(matches!(app.detail, Detail::Text(_)));
            app.key(key('l'));
            app.key(key('G'));
            assert_eq!(app.preview_row, 1);
        }

        let mut app = app_at(scratch.path());
        assert_eq!(app.browser.files.len(), 1);
        assert_eq!(app.browser.selected().unwrap().label, ".");
        app.key(key('m'));
        assert!(matches!(app.detail, Detail::Text(_)));
    }

    #[test]
    fn leaves_a_binary_file_with_a_text_extension_unfocused() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("binary.txt");
        fs::write(&path, b"looks like text\n\0\xff\0").unwrap();
        let mut app = app_at(&path);
        assert!(matches!(app.detail, Detail::Empty));
        app.key(key('l'));
        assert!(!app.preview_focused);
    }

    #[test]
    fn toggles_between_measurement_files_and_all_files() {
        let scratch = tempfile::tempdir().unwrap();
        fs::write(scratch.path().join("a.mpr"), []).unwrap();
        fs::write(scratch.path().join("note.txt"), []).unwrap();
        let mut app = app_at(scratch.path());
        assert_eq!(app.browser.files.len(), 2);
        app.key(key('m'));
        assert_eq!(app.browser.files.len(), 3);
        app.key(key('m'));
        assert_eq!(app.browser.files.len(), 2);
    }

    #[test]
    fn path_prompt_uses_g_then_space_without_marking() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("sample.mpr");
        fs::write(&path, []).unwrap();
        let mut app = app_at(scratch.path());
        app.key(key('g'));
        app.key(key(' '));
        assert!(matches!(app.prompt.unwrap().kind, PromptKind::Path));
        assert!(app.marked.is_empty());
    }

    #[test]
    fn dotfiles_are_hidden_until_toggled_and_setting_survives_navigation() {
        let scratch = tempfile::tempdir().unwrap();
        let child = scratch.path().join("folder");
        fs::create_dir(&child).unwrap();
        fs::write(scratch.path().join(".root.mpr"), []).unwrap();
        fs::write(child.join(".child.mpr"), []).unwrap();
        let mut app = app_at(scratch.path());
        assert_eq!(app.browser.files.len(), 2);
        assert!(matches!(&app.detail, Detail::Folder(entries) if entries.is_empty()));
        app.key(key('.'));
        assert_eq!(app.browser.files.len(), 3);
        assert!(matches!(&app.detail, Detail::Folder(entries) if entries.len() == 1));
        app.key(key('l'));
        assert_eq!(app.browser.selected().unwrap().label, ".child.mpr");
        app.key(key('.'));
        assert_eq!(app.browser.files.len(), 1);
        assert_eq!(app.browser.selected().unwrap().label, ".");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn yy_and_p_copy_across_folders_without_overwriting() {
        let scratch = tempfile::tempdir().unwrap();
        let child = scratch.path().join("folder");
        fs::create_dir(&child).unwrap();
        let source = scratch.path().join("sample.mpr");
        let destination = child.join("sample.mpr");
        fs::write(&source, b"source").unwrap();
        let mut app = app_at(&source);
        app.key(key('y'));
        app.key(key('y'));
        app.key(key('k'));
        app.key(key('l'));
        app.key(key('p'));
        assert_eq!(fs::read(&destination).unwrap(), b"source");
        fs::write(&destination, b"other").unwrap();
        app.key(key('p'));
        assert_eq!(fs::read(&destination).unwrap(), b"other");
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert!(app.prompt.is_none());
    }

    #[test]
    fn shift_q_requests_the_current_directory_and_q_exits_normally() {
        let scratch = tempfile::tempdir().unwrap();
        let mut app = app_at(scratch.path());
        assert!(app.key(key('q')));
        assert!(app.exit_directory.is_none());
        assert!(app.key(KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT)));
        assert_eq!(app.exit_directory.unwrap(), app.browser.directory);
    }

    #[test]
    fn e_requests_conversion_confirmation() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("sample.mpr");
        let output = source.with_extension("mpt");
        fs::write(&source, []).unwrap();
        fs::write(&output, b"existing").unwrap();
        let mut app = app_at(&source);

        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.pending_overwrite.is_none());
        app.key(key('c'));
        assert!(app.pending_overwrite.is_none());
        assert!(app.prefix.is_some());
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.key(key('e'));
        assert!(app.pending_overwrite.is_some());
        assert_eq!(fs::read(output).unwrap(), b"existing");
    }

    #[test]
    fn shifted_e_checks_only_the_cursor_mpr_and_its_named_output() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("first.mpr");
        fs::write(&source, []).unwrap();
        fs::write(scratch.path().join("second.mpr"), []).unwrap();
        let output = scratch.path().join("named result.mpt");
        fs::write(&output, b"existing").unwrap();
        let mut app = app_at(&source);
        app.marked.extend(
            app.browser
                .files
                .iter()
                .filter_map(|entry| (entry.kind == Kind::Mpr).then_some(entry.path.clone())),
        );

        app.key(KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT));
        assert!(app.prompt.is_some());
        app.paste("named result.mpt");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let pending = app.pending_overwrite.as_ref().unwrap();
        assert_eq!(pending.jobs.len(), 1);
        let job = &pending.jobs[0];
        assert_eq!(job.input, app.browser.selected().unwrap().path);
        assert_eq!(job.destination(), output.canonicalize().unwrap());
        assert_eq!(app.marked.len(), 2);
        assert!(app.output.is_none());
        assert_eq!(fs::read(output).unwrap(), b"existing");
    }

    #[test]
    fn cancelling_trash_keeps_the_highlighted_file() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("sample.mpr");
        fs::write(&path, b"unchanged").unwrap();
        let mut app = app_at(scratch.path());
        app.key(key('d'));
        assert_eq!(
            app.trash_target
                .as_ref()
                .map(|target| target.path.as_path()),
            Some(app.browser.selected().unwrap().path.as_path())
        );
        assert_eq!(fs::read(&path).unwrap(), b"unchanged");
        app.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::ALT));
        assert!(app.trash_target.is_some());
        app.key(key('n'));
        assert!(app.trash_target.is_none());
        assert_eq!(fs::read(&path).unwrap(), b"unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_trash_a_file_replaced_during_confirmation() {
        for confirm in [key('y'), KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)] {
            let scratch = tempfile::tempdir().unwrap();
            let path = scratch.path().join("sample.mpr");
            fs::write(&path, b"original").unwrap();
            let mut app = app_at(scratch.path());
            app.key(key('d'));
            fs::rename(&path, scratch.path().join("original.mpr")).unwrap();
            fs::write(&path, b"replacement").unwrap();
            app.key(confirm);
            assert!(app.trash_target.is_none());
            assert_eq!(fs::read(path).unwrap(), b"replacement");
            assert!(app.message.unwrap().contains("changed"));
        }
    }

    #[test]
    fn cli_output_applies_only_to_the_initial_file_target() {
        let scratch = tempfile::tempdir().unwrap();
        let first = scratch.path().join("alpha.mpr");
        let second = scratch.path().join("beta.mpr");
        let explicit = scratch.path().join("custom.mpt");
        fs::write(&first, []).unwrap();
        fs::write(&second, []).unwrap();
        let app = App::new(Args {
            input: Some(first.clone()),
            output: Some(explicit.clone()),
            plain_icons: false,
            cwd_file: None,
        })
        .unwrap();
        let first_canon = first.canonicalize().unwrap();
        let second_canon = second.canonicalize().unwrap();
        assert_eq!(app.planned_output(&first_canon), Some(explicit));
        assert_eq!(app.planned_output(&second_canon), None);
    }

    #[test]
    fn pending_overwrite_skips_duplicate_batch_outputs() {
        let scratch = tempfile::tempdir().unwrap();
        let first = scratch.path().join("first.mpr");
        let second = scratch.path().join("second.mpr");
        let shared = scratch.path().join("shared.mpt");
        fs::write(&first, []).unwrap();
        fs::write(&second, []).unwrap();
        fs::write(&shared, b"existing").unwrap();

        let pending = PendingOverwrite::scan(vec![
            BatchJob {
                input: first,
                output: Some(shared.clone()),
            },
            BatchJob {
                input: second,
                output: Some(shared),
            },
        ]);
        assert!(pending.existing.is_empty());
    }

    #[test]
    fn reload_preserves_inspection_without_moving_report_cursor() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("sample.mpr");
        fs::write(&path, []).unwrap();
        let mut app = app_at(&path);
        let mut view = InspectionView::from(mpr2mpt::Inspection {
            metadata: vec![("Technique".into(), "CV".into())],
            headings: vec!["time/s".into(), "Ewe/V".into()],
            rows: vec![
                vec!["0".into(), "1".into()],
                vec!["1".into(), "2".into()],
                vec!["2".into(), "3".into()],
            ],
            total_rows: 3,
        });
        view.source_meta = preview::file_signature(&path);
        app.detail = Detail::Inspection(view);
        app.preview_row = 2;
        app.preview_column = 1;
        app.report = Some(Report::new(vec![
            scratch.path().join("a.mpr"),
            scratch.path().join("b.mpr"),
            scratch.path().join("c.mpr"),
        ]));
        app.show_report = true;

        app.reload();

        assert!(matches!(app.detail, Detail::Inspection(_)));
        assert_eq!(app.preview_row, 2);
        assert_eq!(app.preview_column, 1);
        assert_eq!(app.report.as_ref().unwrap().cursor, 0);
    }
}
