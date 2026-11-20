use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use openreadout_core::FormatReader;
use openreadout_echem::MprReader;
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};

const PREVIEW_BYTES: usize = 128 * 1024;
const PREVIEW_ROWS: usize = 24;

pub(super) struct MptPreview {
    headings: Vec<String>,
    data: Vec<Vec<String>>,
    has_more_rows: bool,
}

pub(super) struct InspectionView {
    pub(super) metadata: Vec<(String, String)>,
    pub(super) metadata_start: usize,
    pub(super) table: MptPreview,
    pub(super) source_meta: Option<(u64, Option<std::time::SystemTime>)>,
}

pub(super) struct TextPreview {
    lines: Vec<String>,
    columns: usize,
    truncated: bool,
}

impl TextPreview {
    pub(super) fn load(path: &Path) -> io::Result<Option<Self>> {
        let Some((bytes, truncated)) = read_preview(path)? else {
            return Ok(None);
        };
        let Some(text) = decode_text(&bytes, truncated).filter(|text| {
            // Allow occasional controls in text logs, but never accept NUL.
            !text.contains('\0')
                && text
                    .chars()
                    .filter(|&character| {
                        character.is_control()
                            && !matches!(character, '\n' | '\r' | '\t' | '\x0c' | '\x1b')
                    })
                    .nth(1 + text.len() / 100)
                    .is_none()
        }) else {
            return Ok(None);
        };
        let lines: Vec<_> = text
            .lines()
            .map(|line| super::safe_text(&line.replace('\t', "    ")))
            .collect();
        let columns = lines
            .iter()
            .map(|line| Span::raw(line).width())
            .max()
            .unwrap_or(0)
            .min(usize::from(u16::MAX) + 1);
        Ok(Some(Self {
            lines,
            columns,
            truncated,
        }))
    }

    pub(super) fn rows(&self) -> usize {
        self.lines.len()
    }

    pub(super) fn columns(&self) -> usize {
        self.columns
    }

    pub(super) fn draw(&self, frame: &mut Frame, area: Rect, column: usize, row: usize) {
        if area.is_empty() {
            return;
        }
        if self.lines.is_empty() {
            frame.render_widget(Paragraph::new("Empty file"), area);
            return;
        }
        let text_height = area.height - u16::from(area.height > 1);
        let first = row.min(self.rows() - 1);
        let last = (first + usize::from(text_height)).min(self.rows());
        let lines: Vec<_> = self.lines[first..last]
            .iter()
            .map(|line| Line::raw(line.as_str()))
            .collect();
        frame.render_widget(
            Paragraph::new(lines).scroll((0, column as u16)),
            Rect {
                height: text_height,
                ..area
            },
        );
        if text_height < area.height {
            let suffix = if self.truncated { "+" } else { "" };
            frame.render_widget(
                Paragraph::new(format!(
                    "Lines {}–{last}/{}{suffix}",
                    first + 1,
                    self.rows()
                ))
                .style(Style::default().fg(Color::Gray)),
                Rect {
                    y: area.y + text_height,
                    height: 1,
                    ..area
                },
            );
        }
    }
}

impl From<mpr2mpt::Inspection> for InspectionView {
    fn from(inspection: mpr2mpt::Inspection) -> Self {
        Self {
            metadata_start: 0,
            metadata: inspection
                .metadata
                .into_iter()
                .map(|(name, value)| (cell_text(&name), cell_text(&value)))
                .collect(),
            table: MptPreview {
                has_more_rows: inspection.total_rows > inspection.rows.len() as u64,
                headings: inspection.headings,
                data: inspection.rows,
            },
            source_meta: None,
        }
    }
}

pub(super) fn file_signature(path: &Path) -> Option<(u64, Option<std::time::SystemTime>)> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()))
}

impl MptPreview {
    pub(super) fn load(path: &Path) -> io::Result<Self> {
        let (mut bytes, byte_limit_reached) = read_preview(path)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Preview requires a regular file",
            )
        })?;
        if byte_limit_reached {
            let end = bytes
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |index| index + 1);
            bytes.truncate(end);
        }
        let text = decode_preview(&bytes, false);
        let mut lines = text.lines();
        if !matches!(
            lines.next().map(str::trim),
            Some("EC-Lab ASCII FILE" | "BT-Lab ASCII FILE")
        ) {
            return Err(invalid_header("Missing EC-Lab or BT-Lab ASCII file header"));
        }

        let header_lines = lines
            .next()
            .and_then(|line| line.split_once(':'))
            .filter(|(label, _)| label.trim() == "Nb header lines")
            .and_then(|(_, count)| count.trim().parse::<usize>().ok())
            .filter(|count| *count >= 3)
            .ok_or_else(|| invalid_header("Invalid MPT header line count"))?;
        let columns_line = lines
            .nth(header_lines - 3)
            .ok_or_else(|| invalid_header("MPT column headings are missing from the preview"))?
            .trim_end_matches(['\t', ' ']);
        if columns_line.is_empty() {
            return Err(invalid_header("MPT column headings are empty"));
        }
        let headings: Vec<_> = columns_line.split('\t').map(cell_text).collect();
        let mut rows = lines.filter(|line| !line.trim().is_empty());
        let data = rows
            .by_ref()
            .take(PREVIEW_ROWS)
            .map(|line| {
                line.split('\t')
                    .take(headings.len())
                    .map(cell_text)
                    .collect()
            })
            .collect();
        let has_more_rows = byte_limit_reached || rows.next().is_some();

        Ok(Self {
            headings,
            data,
            has_more_rows,
        })
    }

    pub(super) fn columns(&self) -> usize {
        self.headings.len()
    }

    pub(super) fn rows(&self) -> usize {
        self.data.len()
    }

    pub(super) fn draw(&self, frame: &mut Frame, area: Rect, column: usize, row: usize) {
        if area.is_empty() {
            return;
        }
        let table_height = area.height - u16::from(area.height > 2);
        let column = column.min(self.columns() - 1);
        let first_row = row.min(self.rows().saturating_sub(1));
        let last_row = (first_row + usize::from(table_height - 1)).min(self.rows());
        let data = &self.data[first_row..last_row];
        let widths = self.column_widths(column, data, area.width);
        let last_column = column + widths.len();
        let header = Row::new(
            self.headings[column..last_column]
                .iter()
                .zip(&widths)
                .map(|(heading, &width)| fit_cell(heading, width)),
        )
        .style(Style::default().fg(Color::Cyan));
        let rows = data.iter().map(|values| {
            Row::new((column..last_column).zip(&widths).map(|(index, &width)| {
                fit_cell(values.get(index).map_or("", String::as_str), width)
            }))
        });
        frame.render_widget(
            Table::new(rows, widths.iter().copied().map(Constraint::Length))
                .header(header)
                .column_spacing(1),
            Rect {
                height: table_height,
                ..area
            },
        );
        if table_height < area.height {
            let suffix = if self.has_more_rows { "+" } else { "" };
            let first_visible_row = if first_row < last_row {
                first_row + 1
            } else {
                0
            };
            let summary = if area.width < 32 {
                format!(
                    "C{}/{} R{first_visible_row}/{}{suffix}",
                    column + 1,
                    self.columns(),
                    self.rows(),
                )
            } else {
                format!(
                    "Cols {}–{last_column}/{}  Rows {first_visible_row}–{last_row}/{}{suffix}",
                    column + 1,
                    self.columns(),
                    self.rows(),
                )
            };
            frame.render_widget(
                Paragraph::new(summary).style(Style::default().fg(Color::Gray)),
                Rect {
                    y: area.y + table_height,
                    height: 1,
                    ..area
                },
            );
        }
    }

    fn column_widths(&self, column: usize, data: &[Vec<String>], width: u16) -> Vec<u16> {
        let mut widths = Vec::new();
        let mut remaining = width;
        for (index, heading) in self.headings.iter().enumerate().skip(column) {
            if remaining == 0 {
                break;
            }
            let longest = data
                .iter()
                .filter_map(|values| values.get(index))
                .map(|value| Span::raw(value).width())
                .fold(Span::raw(heading).width(), usize::max)
                .max(1);
            if !widths.is_empty() && longest > usize::from(remaining) {
                break;
            }
            let width = longest.min(usize::from(remaining)) as u16;
            widths.push(width);
            remaining = remaining.saturating_sub(width.saturating_add(1));
        }
        widths
    }
}

pub(super) fn has_mpr_signature(path: &Path) -> bool {
    let Ok(Some(mut file)) = open_preview_file(path) else {
        return false;
    };
    // Check the first module after the 52-byte MPR preamble.
    let mut header = [0; 58];
    file.read_exact(&mut header).is_ok()
        && header.ends_with(b"MODULE")
        && MprReader.sniff(&header, path).is_some()
}

fn open_preview_file(path: &Path) -> io::Result<Option<File>> {
    if !fs::metadata(path)?.is_file() {
        return Ok(None);
    }
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // A path can be replaced by a FIFO between metadata() and open().
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    Ok(file.metadata()?.is_file().then_some(file))
}

fn read_preview(path: &Path) -> io::Result<Option<(Vec<u8>, bool)>> {
    let Some(file) = open_preview_file(path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.take((PREVIEW_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > PREVIEW_BYTES;
    bytes.truncate(PREVIEW_BYTES);
    Ok(Some((bytes, truncated)))
}

fn decode_text(bytes: &[u8], truncated: bool) -> Option<Cow<'_, str>> {
    let (bytes, from_bytes): (_, fn([u8; 2]) -> u16) = match bytes {
        [0xff, 0xfe, rest @ ..] => (rest, u16::from_le_bytes),
        [0xfe, 0xff, rest @ ..] => (rest, u16::from_be_bytes),
        _ => return Some(decode_preview(bytes, truncated)),
    };
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !truncated && !remainder.is_empty() {
        return None;
    }
    let mut units: Vec<_> = pairs.iter().copied().map(from_bytes).collect();
    if truncated
        && units
            .last()
            .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
    {
        units.pop();
    }
    String::from_utf16(&units).ok().map(Cow::Owned)
}

fn decode_preview(bytes: &[u8], truncated: bool) -> Cow<'_, str> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    match str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(error) if truncated && error.error_len().is_none() => {
            let valid = &bytes[..error.valid_up_to()];
            Cow::Borrowed(str::from_utf8(valid).expect("valid UTF-8 prefix"))
        }
        Err(_) => Cow::Owned(bytes.iter().copied().map(char::from).collect::<String>()),
    }
}

fn fit_cell(value: &str, width: u16) -> Cell<'_> {
    let span = Span::raw(value);
    if span.width() <= usize::from(width) {
        return Cell::new(value);
    }

    let mut remaining = usize::from(width.saturating_sub(1));
    let mut clipped = String::new();
    for grapheme in span.styled_graphemes(Style::default()) {
        let grapheme_width = Span::raw(grapheme.symbol).width();
        if grapheme_width > remaining {
            break;
        }
        clipped.push_str(grapheme.symbol);
        remaining -= grapheme_width;
    }
    clipped.push('…');
    Cell::new(clipped)
}

fn invalid_header(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn cell_text(value: &str) -> String {
    super::safe_text(value.trim())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{ErrorKind, Write};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn preview_file(contents: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::Builder::new().suffix(".mpt").tempfile().unwrap();
        file.write_all(contents).unwrap();
        file
    }

    #[test]
    fn reads_declared_header_and_latin1_without_reformatting_values() {
        let file = preview_file(
            b"EC-Lab ASCII FILE\r\nNb header lines : 4\r\nmetadata\r\n              time/s\t<I>/\xb5A\t\r\n1,25\t-2E+04\t\r\n",
        );
        let preview = MptPreview::load(file.path()).unwrap();
        assert_eq!(preview.columns(), 2);
        assert_eq!(preview.rows(), 1);
        assert_eq!(preview.headings, ["time/s", "<I>/µA"]);
        assert_eq!(preview.data[0], ["1,25", "-2E+04"]);
    }

    #[test]
    fn reads_utf8_bom_and_sanitizes_control_characters() {
        let file = preview_file(
            "\u{feff}BT-Lab ASCII FILE\nNb header lines : 3\n電流\t名称\n2.34\t\x1b[2J\n"
                .as_bytes(),
        );
        let preview = MptPreview::load(file.path()).unwrap();
        assert_eq!(preview.headings, ["電流", "名称"]);
        assert_eq!(preview.data[0], ["2.34", "�[2J"]);
    }

    #[test]
    fn keeps_only_the_sampled_rows() {
        let mut content = b"EC-Lab ASCII FILE\nNb header lines : 3\nA\n".to_vec();
        content.extend(b"1\n".repeat(512));
        let file = preview_file(&content);
        let preview = MptPreview::load(file.path()).unwrap();
        assert_eq!(preview.rows(), 24);
        assert!(preview.has_more_rows);
    }

    #[test]
    fn drops_a_row_cut_by_the_preview_byte_limit() {
        let mut content = b"EC-Lab ASCII FILE\nNb header lines : 3\nA\n1\n".to_vec();
        content.extend(std::iter::repeat_n(b'0', 150_000));
        content.extend_from_slice(b"\n3\n");
        let file = preview_file(&content);
        let preview = MptPreview::load(file.path()).unwrap();
        assert_eq!(preview.rows(), 1);
        assert!(preview.has_more_rows);
    }

    #[test]
    fn text_keeps_a_partial_long_line_without_corrupting_its_utf8_prefix() {
        let mut content = "heading\nµ".as_bytes().to_vec();
        content.resize(PREVIEW_BYTES - 1, b'a');
        content.extend_from_slice("測".as_bytes());
        let file = preview_file(&content);
        let preview = TextPreview::load(file.path()).unwrap().unwrap();

        assert!(preview.truncated);
        assert_eq!(preview.rows(), 2);
        assert!(preview.lines[1].starts_with("µ"));
        assert!(preview.lines[1].ends_with('a'));
    }

    #[test]
    fn text_preview_reads_utf16_in_both_byte_orders() {
        let encodings = [
            ([0xff, 0xfe], u16::to_le_bytes as fn(u16) -> [u8; 2]),
            ([0xfe, 0xff], u16::to_be_bytes),
        ];
        for (bom, to_bytes) in encodings {
            let mut content = bom.to_vec();
            content.extend("日本語\n🔬\n".encode_utf16().flat_map(to_bytes));
            let file = preview_file(&content);
            let preview = TextPreview::load(file.path()).unwrap().unwrap();
            assert_eq!(preview.lines, ["日本語", "🔬"]);
        }
    }

    #[test]
    fn text_preview_sanitizes_incidental_controls_without_hiding_text() {
        for (bytes, expected) in [
            (&b"caf\xe9\x85line\n"[..], "café�line"),
            (&b"ready\x07\n"[..], "ready�"),
        ] {
            let file = preview_file(bytes);
            let preview = TextPreview::load(file.path()).unwrap().unwrap();
            assert_eq!(preview.lines, [expected]);
        }
    }

    #[test]
    fn rejects_bad_or_incomplete_headers() {
        let samples: &[&[u8]] = &[
            b"not an MPT file\nNb header lines : 3\nA\n1\n",
            b"EC-Lab ASCII FILE\nNb header lines : 2\nA\n",
            b"EC-Lab ASCII FILE\nNb header lines : 999\nA\n",
            b"EC-Lab ASCII FILE\nNb header lines : 3\n\t\n",
        ];
        for bytes in samples {
            let file = preview_file(bytes);
            let error = MptPreview::load(file.path()).err().unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidData);
        }
    }

    #[test]
    fn rejects_non_regular_paths() {
        let folder = tempfile::tempdir().unwrap();
        let error = MptPreview::load(folder.path()).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(fs::metadata(folder.path()).unwrap().is_dir());
    }

    #[test]
    fn draws_requested_rows_and_columns_in_a_narrow_area() {
        let file = preview_file(
            b"EC-Lab ASCII FILE\nNb header lines : 3\n              time/s\tEwe/V\tI/mA\n10\t20\t30\n11\t21\t31\n",
        );
        let preview = MptPreview::load(file.path()).unwrap();
        let backend = TestBackend::new(12, 3);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| preview.draw(frame, frame.area(), 1, 1))
            .unwrap();
        let buffer = term.backend().buffer();
        let rendered = |row| {
            (0..12)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        };
        assert!(rendered(0).contains("Ewe/V"));
        assert!(!rendered(0).contains("time/s"));
        assert!(rendered(1).contains("21"));
        assert!(!rendered(1).contains("20"));
        assert!(rendered(2).contains("C2/3 R2/2"));
    }

    #[test]
    fn gives_the_second_preview_line_to_the_requested_measurement() {
        let file = preview_file(b"EC-Lab ASCII FILE\nNb header lines : 3\nA\n1\n2\n");
        let preview = MptPreview::load(file.path()).unwrap();
        let mut term = Terminal::new(TestBackend::new(12, 2)).unwrap();
        term.draw(|frame| preview.draw(frame, frame.area(), 0, 1))
            .unwrap();
        let buffer = term.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "A");
        assert_eq!(buffer[(0, 1)].symbol(), "2");
    }

    #[test]
    fn hides_a_secondary_column_when_its_numeric_text_would_be_clipped() {
        let file = preview_file(b"EC-Lab ASCII FILE\nNb header lines : 3\nA\tB\n1\t3.14159\n");
        let preview = MptPreview::load(file.path()).unwrap();
        let mut term = Terminal::new(TestBackend::new(4, 2)).unwrap();
        term.draw(|frame| preview.draw(frame, frame.area(), 0, 0))
            .unwrap();
        let buffer = term.backend().buffer();
        assert_eq!(buffer[(0, 1)].symbol(), "1");
        assert_eq!(buffer[(2, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
    }

    #[test]
    fn marks_clipped_headings_and_numeric_text_with_an_ellipsis() {
        let file =
            preview_file("EC-Lab ASCII FILE\nNb header lines : 3\n状態\n3.14159\n".as_bytes());
        let preview = MptPreview::load(file.path()).unwrap();
        let mut narrow = Terminal::new(TestBackend::new(3, 2)).unwrap();
        narrow
            .draw(|frame| preview.draw(frame, frame.area(), 0, 0))
            .unwrap();
        let buffer = narrow.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "状");
        assert_eq!(buffer[(2, 0)].symbol(), "…");
        assert_eq!(buffer[(0, 1)].symbol(), "3");
        assert_eq!(buffer[(1, 1)].symbol(), ".");
        assert_eq!(buffer[(2, 1)].symbol(), "…");

        let mut wide = Terminal::new(TestBackend::new(10, 2)).unwrap();
        wide.draw(|frame| preview.draw(frame, frame.area(), 0, 0))
            .unwrap();
        let buffer = wide.backend().buffer();
        let value = (0..7)
            .map(|column| buffer[(column, 1)].symbol())
            .collect::<String>();
        assert_eq!(value, "3.14159");
    }
}
