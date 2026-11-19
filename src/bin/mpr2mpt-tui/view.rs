use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph, Wrap};

use super::browser::{Entry, Kind, terminal_text};
use super::jobs::Report;
use super::{App, Detail, PromptKind, safe_text};

const ACCENT: Color = Color::Cyan;

impl App {
    pub(super) fn draw(&self, frame: &mut Frame) {
        let area = frame.area().inner(Margin::new(1, 1));
        let prompt_height = if self.trash_target.is_some() {
            4
        } else if self.prompt.is_some() {
            3
        } else {
            0
        };
        let [heading, panes, status, prompt, help] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(2),
            Constraint::Length(prompt_height),
            Constraint::Length(if area.width >= 92 { 2 } else { 3 }),
        ])
        .areas(area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from("MPR → MPT").fg(ACCENT).bold(),
                Line::from(terminal_text(&self.browser.directory)),
            ]),
            heading,
        );
        let (files, details) = if panes.width >= 96 {
            let [parent, files, details] = Layout::horizontal([
                Constraint::Percentage(23),
                Constraint::Percentage(31),
                Constraint::Percentage(46),
            ])
            .areas(panes);
            let focus = self
                .browser
                .parents
                .iter()
                .position(|entry| entry.path == self.browser.directory);
            draw_entries(frame, parent, "Parent", &self.browser.parents, focus, None);
            (files, details)
        } else if panes.width >= 64 {
            let [files, details] =
                Layout::horizontal([Constraint::Percentage(43), Constraint::Percentage(57)])
                    .areas(panes);
            (files, details)
        } else {
            let [files, details] =
                Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(panes);
            (files, details)
        };
        draw_entries(
            frame,
            files,
            if self.browser.restricted {
                "Files · MPR/MPS/MPT"
            } else {
                "Files · All"
            },
            &self.browser.files,
            Some(self.browser.cursor),
            Some(&self.marked),
        );
        self.draw_details(frame, details);
        self.draw_status(frame, status);
        if let Some(edit) = &self.prompt {
            let (title, placeholder) = match &edit.kind {
                PromptKind::Path => ("Go to path", "Folder or measurement file"),
                PromptKind::Output => ("Single-file output", "Empty uses the source folder"),
                PromptKind::Filter { .. } => ("Filter files", "Filter by name"),
            };
            edit.field.draw(frame, prompt, title, placeholder, true);
        }
        if let Some(target) = &self.trash_target {
            let path = &target.path;
            let block = panel("Move to Trash?").border_style(Color::Yellow);
            let inner = block.inner(prompt);
            frame.render_widget(block, prompt);
            let name = path.file_name().map_or(path.as_path(), Path::new);
            let lines = vec![
                Line::from(terminal_text(name)).bold(),
                Line::from(terminal_text(path.parent().unwrap_or(path))),
            ];
            frame.render_widget(Paragraph::new(lines), inner);
        }
        let hint = if self.trash_target.is_some() {
            "y Confirm   n / Esc Cancel"
        } else if self.pending_jump {
            "g Top   d Downloads   h Home   t Trash"
        } else if self.prompt.is_some() {
            "Enter  Apply    Esc  Back    Ctrl+U  Clear"
        } else if area.width >= 92 {
            "j/k Move   h/l Folder   g… Jump   p Path   f Filter   m Mode   d Trash   q Quit\nSpace Mark   x Clear   c Convert   o Output   Tab Results   PgUp/Dn Rows   [ ] Cols   r Reload"
        } else {
            "j/k Move  h/l Folder  g… Jump  p Path\nf Filter  m Mode  d Trash  Space Mark  c Convert\nTab Results  PgUp/Dn Rows  [ ] Cols  q Quit"
        };
        frame.render_widget(
            Paragraph::new(hint).wrap(Wrap { trim: true }).dark_gray(),
            help,
        );
    }

    fn draw_details(&self, frame: &mut Frame, area: Rect) {
        if self.show_report
            && let Some(report) = &self.report
        {
            report.draw(frame, area);
            return;
        }
        let name = self
            .browser
            .selected()
            .map_or("Preview", |entry| entry.label.as_str());
        match &self.detail {
            Detail::Folder(entries) => draw_entries(frame, area, name, entries, None, None),
            detail => {
                let block = panel(name);
                let inner = block.inner(area);
                frame.render_widget(block, area);
                match detail {
                    Detail::Mpt(preview) => {
                        preview.draw(frame, inner, self.preview_column, self.preview_row)
                    }
                    Detail::Mpr { path, bytes } => {
                        let size = format!("{:.1} KiB", *bytes as f64 / 1024.0);
                        let planned = self
                            .output
                            .clone()
                            .unwrap_or_else(|| path.with_extension("mpt"));
                        let lines = vec![
                            Line::from("BioLogic MPR").fg(ACCENT).bold(),
                            Line::from(size),
                            Line::from("Output").fg(ACCENT),
                            Line::from(terminal_text(&planned)),
                        ];
                        frame
                            .render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
                    }
                    Detail::Error(error) => frame.render_widget(
                        Paragraph::new(safe_text(error))
                            .red()
                            .wrap(Wrap { trim: true }),
                        inner,
                    ),
                    Detail::Empty => {}
                    Detail::Folder(_) => unreachable!(),
                }
            }
        }
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let mut parts = vec![Span::styled(
            format!("{} marked", self.marked.len()),
            Style::new().fg(ACCENT),
        )];
        if let Some(report) = &self.report {
            let done = if report.failure.is_some() {
                report.results.len()
            } else {
                report.results.iter().flatten().count()
            };
            let written = report
                .results
                .iter()
                .flatten()
                .filter(|item| item.is_ok())
                .count();
            parts.push(Span::raw(format!(
                "  |  {done}/{}  ·  {written} written  ·  {} failed",
                report.results.len(),
                done - written
            )));
        }
        let filter = format!("Filter: {}", self.browser.filter);
        let secondary = if self.exit_when_done {
            Some("Exiting after the active conversion…")
        } else {
            self.message
                .as_deref()
                .or_else(|| (!self.browser.filter.is_empty()).then_some(filter.as_str()))
        };
        let mut lines = vec![Line::from(parts)];
        if let Some(message) = secondary {
            lines.push(Line::raw(message).yellow());
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
}

fn panel(title: &str) -> Block<'_> {
    Block::bordered()
        .title(Line::from(title).fg(ACCENT))
        .border_style(Color::DarkGray)
}

fn visible_start(cursor: usize, slots: usize, entries: usize) -> usize {
    cursor
        .saturating_sub(slots / 2)
        .min(entries.saturating_sub(slots))
}

fn draw_entries(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    entries: &[Entry],
    focus: Option<usize>,
    marked: Option<&BTreeSet<PathBuf>>,
) {
    let block = panel(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if entries.is_empty() {
        frame.render_widget(Paragraph::new("No matching entries").dark_gray(), inner);
        return;
    }
    let slots = usize::from(inner.height);
    let start = visible_start(focus.unwrap_or_default(), slots, entries.len());
    let items = entries
        .iter()
        .enumerate()
        .skip(start)
        .take(slots)
        .map(|(i, entry)| {
            let marked = marked.is_some_and(|marks| marks.contains(&entry.path));
            let leader = match (entry.kind, marked) {
                (_, true) => "✓ ",
                (Kind::Directory, false) => "▸ ",
                (Kind::Mpr, false) => "◇ ",
                (Kind::Mps, false) => "⚙ ",
                (Kind::Mpt, false) => "≡ ",
                (Kind::Other, false) => "· ",
            };
            let color = if marked { Color::LightGreen } else { ACCENT };
            let line = Line::from(vec![
                Span::styled(leader, Style::new().fg(color)),
                Span::raw(&entry.label),
            ]);
            let item = ListItem::new(line);
            if focus == Some(i) {
                item.style(
                    Style::new()
                        .fg(Color::White)
                        .bg(Color::Rgb(36, 65, 75))
                        .bold(),
                )
            } else {
                item
            }
        });
    frame.render_widget(List::new(items), inner);
}

impl Report {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let block = panel("Results");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [list, summary] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(4.min(inner.height / 2)),
        ])
        .areas(inner);
        let slots = usize::from(list.height);
        let start = visible_start(self.cursor, slots, self.inputs.len());
        let items = self
            .inputs
            .iter()
            .zip(&self.results)
            .enumerate()
            .skip(start)
            .take(slots)
            .map(|(index, (input, result))| {
                let (mark, color) = match result {
                    Some(Ok(_)) => ("✓ ", Color::LightGreen),
                    Some(Err(_)) => ("! ", Color::LightRed),
                    None if self.failure.is_some() => ("! ", Color::LightRed),
                    None => ("· ", Color::Gray),
                };
                let name = terminal_text(input.file_name().map_or(input.as_path(), Path::new));
                let text = Line::from(vec![
                    Span::styled(mark, Style::new().fg(color)),
                    Span::raw(name),
                ]);
                let item = ListItem::new(text);
                if index == self.cursor {
                    item.style(Style::new().bg(Color::Rgb(36, 65, 75)))
                } else {
                    item
                }
            });
        frame.render_widget(List::new(items), list);
        let text = match &self.results[self.cursor] {
            Some(Ok(report)) => format!(
                "Wrote {} · {} rows · {} columns",
                terminal_text(&report.output),
                report.rows,
                report.columns
            ),
            Some(Err(error)) => safe_text(&error.to_string()),
            None => self.failure.as_ref().map_or_else(
                || "Waiting for conversion…".into(),
                |reason| format!("Error: {reason}"),
            ),
        };
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), summary);
    }
}
