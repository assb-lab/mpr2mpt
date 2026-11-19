use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{App, Detail, Prefix};

const BAR: Color = Color::Rgb(37, 53, 73);
const GUIDE: Color = Color::Rgb(23, 33, 49);
const KEY: Color = Color::Rgb(50, 69, 94);
const LABEL: Color = Color::Rgb(222, 232, 244);
const AQUA: Color = Color::Rgb(120, 225, 218);
const INK: Color = Color::Rgb(16, 32, 51);

const NORMAL: &[(&str, &str)] = &[
    ("j/k", "Move"),
    ("h/l", "Back/Open"),
    ("c", "Convert"),
    ("v", "Preview"),
    ("q/Q", "Quit/cd"),
    ("g…", "Jump"),
    ("yy/p", "Copy/Paste"),
    (".", "Hidden"),
    ("f", "Filter"),
    ("m", "Mode"),
    ("d", "Trash"),
    ("Space", "Mark"),
    ("x", "Clear"),
    ("o", "Output"),
    ("Tab", "Results"),
    ("PgUp/Dn", "Rows"),
    ("[ ]", "Cols"),
    ("r", "Reload"),
];

pub(super) fn shortcuts(app: &App, width: u16) -> Vec<Line<'static>> {
    let keys: &[(&str, &str)] = if app.pending_overwrite.is_some() {
        &[
            ("y", "Overwrite"),
            ("s", "No overwrite"),
            ("Esc", "Cancel"),
            ("j/k", "Outputs"),
        ]
    } else if app.trash_target.is_some() {
        &[("y", "Confirm"), ("n / Esc", "Cancel")]
    } else if app.prefix == Some(Prefix::Jump) {
        &[
            ("Space", "Path"),
            ("g", "Top"),
            ("d", "Downloads"),
            ("h", "Home"),
            ("t", "Trash"),
        ]
    } else if app.prefix == Some(Prefix::Copy) {
        &[("y", "Copy"), ("Esc", "Cancel")]
    } else if app.prompt.is_some() {
        &[("Enter", "Apply"), ("Esc", "Back"), ("Ctrl+U", "Clear")]
    } else if app.preview_focus.is_some() && matches!(app.detail, Detail::Inspection(_)) {
        &[
            ("h / Esc", "Files"),
            ("j/k", "Rows"),
            ("gg/G", "Top/End"),
            ("J/K", "Metadata"),
            ("[ ]", "Cols"),
            ("c", "Convert"),
        ]
    } else if app.preview_focus.is_some() {
        &[
            ("h / Esc", "Files"),
            ("j/k", "Rows"),
            ("gg/G", "Top/End"),
            ("[ ]", "Cols"),
        ]
    } else {
        NORMAL
    };
    key_lines(keys, width)
}

fn key_lines(keys: &[(&str, &str)], width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let mut rows = Vec::new();
    let mut spans = Vec::new();
    let mut used = 0;
    for (key, label) in keys {
        let cap = Span::styled(format!(" {key} "), Style::new().fg(AQUA).bg(KEY).bold());
        let action = Span::raw(format!(" {label} "));
        let cells = cap.width() + action.width();
        if used != 0 && used + cells > width {
            rows.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }
        used += cells;
        spans.extend([cap, action]);
    }
    if !spans.is_empty() {
        rows.push(Line::from(spans));
    }
    rows
}

pub(super) fn draw_shortcuts(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    frame.render_widget(
        Paragraph::new(lines).style(Style::new().fg(LABEL).bg(GUIDE)),
        area,
    );
}

pub(super) fn draw_status(app: &App, frame: &mut Frame, area: Rect) {
    let marked = Span::styled(
        format!(" {} marked ", app.marked.len()),
        Style::new().fg(INK).bg(AQUA).bold(),
    );
    let status = if let Some(report) = &app.report {
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
        report_status(marked, done, written, report.results.len(), area.width)
    } else {
        Line::from(marked)
    };
    let note = if app.exit_when_done {
        Some((
            "Exiting after the active conversion…".to_owned(),
            Color::LightYellow,
        ))
    } else if let Some(message) = &app.message {
        Some((message.clone(), Color::LightYellow))
    } else if !app.browser.filter.is_empty() {
        Some((format!("Filter: {}", app.browser.filter), AQUA))
    } else {
        None
    };
    let mut lines = vec![status];
    if let Some((message, color)) = note {
        lines.push(Line::from(format!(" {message}")).fg(color));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::new().fg(LABEL).bg(BAR)),
        area,
    );
}

fn report_status(
    marked: Span<'static>,
    done: usize,
    written: usize,
    total: usize,
    width: u16,
) -> Line<'static> {
    let width = usize::from(width);
    let failed = done - written;
    let success_style = Style::new().fg(Color::LightGreen).bg(GUIDE);
    let failure = Span::styled(
        format!(" {failed} failed "),
        Style::new()
            .fg(if failed == 0 { LABEL } else { Color::LightRed })
            .bg(GUIDE),
    );
    let full = Line::from(vec![
        marked.clone(),
        Span::raw(format!("  {done}/{total}  ")),
        Span::styled(format!(" {written} written "), success_style),
        failure.clone(),
    ]);
    if full.width() <= width {
        return full;
    }
    let mut used = failure.width();
    let mut compact = vec![failure];
    for segment in [
        Span::raw(format!(" {done}/{total} ")),
        Span::styled(format!(" {written} OK "), success_style),
        marked,
    ] {
        let cells = segment.width();
        if used + cells <= width {
            used += cells;
            compact.push(segment);
        }
    }
    Line::from(compact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn packs_whole_key_labels_and_paints_the_empty_width() {
        for width in [51, 111] {
            let lines = key_lines(NORMAL, width);
            assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
            assert!(lines.len() <= 5);
        }
        let mut terminal = Terminal::new(TestBackend::new(40, 1)).unwrap();
        terminal
            .draw(|frame| {
                draw_shortcuts(frame, frame.area(), key_lines(&[("q", "Quit")], 40));
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, KEY);
        assert_eq!(buffer[(39, 0)].bg, GUIDE);
    }

    #[test]
    fn keeps_the_failure_count_in_narrow_status() {
        let line = report_status(Span::raw(" 300 marked "), 12, 10, 12, 27);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("2 failed"));
        assert!(text.contains("12/12"));
        assert!(line.width() <= 27);
    }
}
