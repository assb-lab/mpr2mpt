use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph, Wrap};

use super::browser::{Entry, Kind, terminal_text};
use super::footer;
use super::jobs::{PendingOverwrite, Report};
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
        let hints = footer::shortcuts(self, area.width);
        // A stacked MPR preview needs room for its metadata, column labels, and a data row.
        let minimum_panes = if area.width >= 64 {
            4
        } else if !self.show_report && matches!(self.detail, Detail::Inspection(_)) {
            8
        } else {
            6
        };
        let help_height = (hints.len() as u16).min(
            area.height
                .saturating_sub(4 + minimum_panes + prompt_height)
                .max(1),
        );
        let [heading, panes, prompt, status, help] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(minimum_panes),
            Constraint::Length(prompt_height),
            Constraint::Length(2),
            Constraint::Length(help_height),
        ])
        .areas(area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from("MPR → MPT").fg(ACCENT).bold(),
                Line::from(terminal_text(&self.browser.directory)),
            ]),
            heading,
        );
        if self.pending_overwrite.is_some() {
            self.draw_details(frame, panes);
        } else {
            self.draw_browser(frame, panes);
        }
        footer::draw_status(self, frame, status);
        if let Some(edit) = &self.prompt {
            let (title, placeholder) = match &edit.kind {
                PromptKind::Path => ("Go to path", "Folder or measurement file"),
                PromptKind::Output { .. } => ("Convert MPR as", "Empty uses the default .mpt name"),
                PromptKind::Filter { .. } => ("Filter files", ""),
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
        footer::draw_shortcuts(frame, help, hints);
    }

    fn draw_browser(&self, frame: &mut Frame, panes: Rect) {
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
            draw_entries(
                frame,
                parent,
                "Parent",
                &self.browser.parents,
                focus,
                None,
                self.plain_icons,
            );
            (files, details)
        } else if panes.width >= 64 {
            let [files, details] =
                Layout::horizontal([Constraint::Percentage(43), Constraint::Percentage(57)])
                    .areas(panes);
            (files, details)
        } else {
            let stack = if self.preview_focused
                || (!self.show_report && matches!(self.detail, Detail::Inspection(_)))
            {
                [
                    Constraint::Length((panes.height / 3).max(3)),
                    Constraint::Min(0),
                ]
            } else {
                [Constraint::Percentage(50), Constraint::Percentage(50)]
            };
            let [files, details] = Layout::vertical(stack).areas(panes);
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
            self.plain_icons,
        );
        self.draw_details(frame, details);
    }

    fn draw_details(&self, frame: &mut Frame, area: Rect) {
        if let Some(pending) = &self.pending_overwrite {
            pending.draw(frame, area);
            return;
        }
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
            Detail::Folder(entries) => {
                draw_entries(frame, area, name, entries, None, None, self.plain_icons)
            }
            detail => {
                let preview_title;
                let title = if matches!(detail, Detail::Inspection(_)) {
                    preview_title = format!("MPR preview · {name}");
                    preview_title.as_str()
                } else {
                    name
                };
                let block = if self.preview_focused {
                    panel(title).border_style(ACCENT)
                } else {
                    panel(title)
                };
                let inner = block.inner(area);
                frame.render_widget(block, area);
                match detail {
                    Detail::Mpt(preview) => {
                        preview.draw(frame, inner, self.preview_column, self.preview_row)
                    }
                    Detail::Text(preview) => {
                        preview.draw(frame, inner, self.preview_column, self.preview_row)
                    }
                    Detail::Inspection(preview) => {
                        let [metadata, samples] = if inner.width >= 76 {
                            Layout::horizontal([
                                Constraint::Percentage(32),
                                Constraint::Percentage(68),
                            ])
                            .areas(inner)
                        } else {
                            let height = (preview.metadata.len() as u16 + 1).min(inner.height / 2);
                            Layout::vertical([Constraint::Length(height), Constraint::Min(1)])
                                .areas(inner)
                        };
                        let details: Vec<_> = preview
                            .metadata
                            .iter()
                            .skip(preview.metadata_start)
                            .map(|(key, value)| {
                                Line::from(vec![
                                    Span::raw(key).fg(ACCENT),
                                    Span::raw("  "),
                                    Span::raw(value),
                                ])
                            })
                            .collect();
                        frame.render_widget(
                            Paragraph::new(details).wrap(Wrap { trim: false }),
                            metadata,
                        );
                        preview
                            .table
                            .draw(frame, samples, self.preview_column, self.preview_row);
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
    plain_icons: bool,
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
            let (icon, plain, color) = file_style(entry.kind, &entry.path);
            let line = Line::from(vec![
                if marked {
                    Span::raw("✓ ").fg(Color::LightGreen)
                } else {
                    Span::raw("  ")
                },
                Span::raw(if plain_icons { plain } else { icon }).fg(color),
                Span::raw(" "),
                Span::raw(&entry.label).fg(color),
            ]);
            let item = ListItem::new(line);
            if focus == Some(i) {
                item.style(Style::new().bg(Color::Rgb(36, 65, 75)).bold())
            } else {
                item
            }
        });
    frame.render_widget(List::new(items), inner);
}

fn file_style(kind: Kind, path: &Path) -> (&'static str, &'static str, Color) {
    match kind {
        Kind::Directory => return ("\u{f07b}", "▸", Color::LightBlue),
        Kind::Mpr => return ("\u{f0c3}", "◇", Color::LightYellow),
        Kind::Mps => return ("\u{f1de}", "⚙", Color::LightMagenta),
        Kind::Mpt => return ("\u{f0ce}", "≡", Color::LightGreen),
        Kind::Other => {}
    }
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    let (icon, plain) = match extension.as_deref() {
        Some("mpl") => return ("\u{f0f6}", "¶", Color::LightCyan),
        Some("mpls" | "mpsl") => return ("\u{f1de}", "⚙", Color::LightMagenta),
        Some("mpp") => return ("\u{f0c3}", "◇", Color::LightYellow),
        Some(
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" | "heic" | "tif" | "tiff" | "bmp"
            | "ico" | "avif",
        ) => ("\u{f1c5}", "▧"),
        Some("pdf") => ("\u{f1c1}", "¶"),
        Some("doc" | "docx" | "odt" | "rtf") => ("\u{f1c2}", "¶"),
        Some("csv" | "tsv" | "xls" | "xlsx" | "ods") => ("\u{f1c3}", "≡"),
        Some("ppt" | "pptx" | "odp") => ("\u{f1c4}", "▤"),
        Some("zip" | "gz" | "bz2" | "xz" | "tar" | "tgz" | "rar" | "7z" | "dmg" | "iso") => {
            ("\u{f1c6}", "□")
        }
        Some("wav" | "mp3" | "ogg" | "flac" | "aac" | "m4a") => ("\u{f1c7}", "♪"),
        Some("mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v") => ("\u{f1c8}", "▷"),
        Some(
            "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "html" | "css" | "sh" | "c" | "cpp" | "go"
            | "rb" | "ipynb",
        ) => ("\u{f121}", "⌘"),
        Some("json" | "toml" | "yaml" | "yml" | "ini" | "conf") => ("\u{f013}", "⚙"),
        Some("txt" | "md" | "rst" | "tex" | "log") => ("\u{f0f6}", "¶"),
        _ => ("\u{f15b}", "·"),
    };
    (icon, plain, Color::Reset)
}

impl PendingOverwrite {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let block = panel("Overwrite output?").border_style(Color::Yellow);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [intro, list, detail] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3.min(inner.height / 3)),
        ])
        .areas(inner);
        frame.render_widget(
            Paragraph::new(format!(
                "{} output destinations need attention",
                self.existing.len()
            ))
            .yellow(),
            intro,
        );
        let slots = usize::from(list.height);
        let start = visible_start(self.cursor, slots, self.existing.len());
        let items = self
            .existing
            .iter()
            .enumerate()
            .skip(start)
            .take(slots)
            .map(|(index, row)| {
                let name =
                    terminal_text(row.path.file_name().map_or(row.path.as_path(), Path::new));
                let status = if row.approval.is_ok() {
                    "Existing"
                } else {
                    "Blocked"
                };
                let item = ListItem::new(Line::from(vec![
                    Span::raw(format!("{status}  ")).yellow(),
                    Span::raw(name),
                ]));
                if index == self.cursor {
                    item.style(Style::new().bg(Color::Rgb(60, 55, 39)))
                } else {
                    item
                }
            });
        frame.render_widget(List::new(items), list);
        if let Some(row) = self.existing.get(self.cursor) {
            let mut lines = vec![Line::from(terminal_text(&row.path))];
            if let Err(error) = &row.approval {
                lines.push(Line::from(safe_text(&error.to_string())).red());
            }
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), detail);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_only_directories_and_biologic_files() {
        assert_eq!(
            file_style(Kind::Directory, Path::new("dir")).2,
            Color::LightBlue
        );
        let measurements = [
            file_style(Kind::Mpr, Path::new("sample.MPR")),
            file_style(Kind::Mps, Path::new("sample.MPS")),
            file_style(Kind::Mpt, Path::new("sample.MPT")),
        ];
        assert_ne!(measurements[0].2, measurements[1].2);
        assert_ne!(measurements[1].2, measurements[2].2);
        assert_eq!(measurements[2].2, Color::LightGreen);
        let image = file_style(Kind::Other, Path::new("figure.PNG"));
        let doc = file_style(Kind::Other, Path::new("reference.pdf"));
        assert_eq!(image.2, Color::Reset);
        assert_eq!(doc.2, Color::Reset);
        assert_ne!(image.0, doc.0);
        assert_ne!(
            file_style(Kind::Other, Path::new("sample.MPL")).2,
            Color::Reset
        );
    }
}
