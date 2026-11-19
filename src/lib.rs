use std::collections::HashSet;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use openreadout_core::FormatReader;
use openreadout_echem::MprReader;
use tempfile::NamedTempFile;
use thiserror::Error;

const CHUNK_ROWS: u64 = 4096;
const FLAG_IDS: [u16; 6] = [1, 2, 3, 21, 31, 65];
const TIME_ID: u16 = 4;

/// The file published by a successful conversion.
#[derive(Debug)]
pub struct ConversionReport {
    pub output: PathBuf,
    pub rows: u64,
    pub columns: usize,
}

#[derive(Debug, Error)]
pub enum ConversionError {
    #[error("cannot read input {path}: {source}")]
    InputIo { path: PathBuf, source: io::Error },
    #[error("input is not a regular file: {0}")]
    NotAFile(PathBuf),
    #[error("cannot decode MPR {path}: {source}")]
    Decode {
        path: PathBuf,
        source: openreadout_core::Error,
    },
    #[error("cannot convert MPR {path}: {reason}")]
    Layout { path: PathBuf, reason: String },
    #[error("invalid output filename: {0}")]
    OutputName(PathBuf),
    #[error("output already exists: {0}")]
    OutputExists(PathBuf),
    #[error("cannot write MPT {path}: {source}")]
    OutputIo { path: PathBuf, source: io::Error },
}

/// Convert stored MPR columns to an EC-Lab text table and publish it without overwriting.
/// When `output` is absent, replace the input's extension with `.mpt`.
pub fn convert(input: &Path, output: Option<&Path>) -> Result<ConversionReport, ConversionError> {
    let output = output.map_or_else(|| input.with_extension("mpt"), Path::to_path_buf);
    if output.file_name().is_none() {
        return Err(ConversionError::OutputName(output));
    }
    let output_error = |source: io::Error| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            ConversionError::OutputExists(output.clone())
        } else {
            ConversionError::OutputIo {
                path: output.clone(),
                source,
            }
        }
    };
    match fs::symlink_metadata(&output) {
        Ok(_) => return Err(ConversionError::OutputExists(output)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(output_error(error)),
    }
    let metadata = fs::metadata(input).map_err(|source| ConversionError::InputIo {
        path: input.into(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ConversionError::NotAFile(input.into()));
    }

    let decode = |source| ConversionError::Decode {
        path: input.into(),
        source,
    };
    let layout = |reason: &str| ConversionError::Layout {
        path: input.into(),
        reason: reason.into(),
    };
    let mut dataset = MprReader.open(input).map_err(decode)?;
    let vendor = dataset.vendor_metadata().map_err(decode)?;
    let eclab = &vendor["eclab"];
    let modules = eclab["modules"]
        .as_array()
        .ok_or_else(|| layout("missing module information"))?;
    if modules
        .iter()
        .filter(|entry| entry["name"] == "VMP data")
        .count()
        > 1
    {
        return Err(layout("multiple VMP data modules are unsupported"));
    }
    let ids = eclab["column_ids"]
        .as_array()
        .and_then(|ids| {
            ids.iter()
                .map(|id| u16::try_from(id.as_u64()?).ok())
                .collect::<Option<Vec<_>>>()
        })
        .filter(|ids| !ids.is_empty())
        .ok_or_else(|| layout("missing data column IDs"))?;
    // OpenReadout 0.4 decodes packed flags from the first byte of each row.
    if ids.iter().any(|id| FLAG_IDS.contains(id)) && !FLAG_IDS.contains(&ids[0]) {
        return Err(layout(
            "packed flags after the first stored column are unsupported",
        ));
    }

    let info = dataset.info().map_err(decode)?;
    let [trace] = info.traces.as_slice() else {
        return Err(layout("expected one measurement table"));
    };
    if trace.channels.len() != ids.len() || trace.sweep_count != 1 {
        return Err(layout("inconsistent measurement table"));
    }
    // OpenReadout promotes the first time column to channel zero. Restore the stored order.
    let mut order: Vec<_> = (0..ids.len()).collect();
    if let Some(time_index) = ids.iter().position(|&id| id == TIME_ID) {
        order[..=time_index].rotate_left(1);
    }
    let mut labels = HashSet::new();
    let mut columns = Vec::with_capacity(order.len());
    for index in order {
        let channel = &trace.channels[index];
        let label = channel
            .extra
            .get("label")
            .and_then(|label| label.as_str())
            .filter(|label| !label.is_empty() && labels.insert(*label))
            .ok_or_else(|| layout("empty or duplicate MPT column label"))?;
        let name = label
            .chars()
            .map(|ch| {
                u8::try_from(u32::from(ch))
                    .ok()
                    .filter(|&byte| byte >= 0x20 && byte != 0x7f)
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| layout("MPT column label is not representable in Latin-1"))?;
        let format = match channel.dtype.as_str() {
            "float32" => NumberFormat::Float32,
            "float64" => NumberFormat::Float64,
            "uint8" | "uint16" | "uint32" => NumberFormat::Integer,
            _ => return Err(layout("unsupported MPT numeric type")),
        };
        columns.push(Column {
            index,
            name,
            format,
        });
    }

    let parent = output.parent().filter(|path| !path.as_os_str().is_empty());
    let mut temporary =
        NamedTempFile::new_in(parent.unwrap_or_else(|| Path::new("."))).map_err(output_error)?;
    let mut writer = BufWriter::new(temporary.as_file_mut());
    write_header(&mut writer, &columns).map_err(output_error)?;
    let mut first = 0;
    while first < trace.sample_count {
        let count = (trace.sample_count - first).min(CHUNK_ROWS);
        let chunk = dataset.read_trace(0, 0, first, count).map_err(decode)?;
        let rows = count as usize;
        if chunk.first_sample != first
            || chunk.channels.len() != columns.len()
            || chunk.channels.iter().any(|values| values.len() != rows)
        {
            return Err(layout("inconsistent measurement row count"));
        }
        write_rows(&mut writer, &columns, &chunk.channels).map_err(output_error)?;
        first += count;
    }
    writer.flush().map_err(output_error)?;
    drop(writer);
    temporary
        .persist_noclobber(&output)
        .map_err(|error| output_error(error.error))?;
    Ok(ConversionReport {
        output,
        rows: trace.sample_count,
        columns: columns.len(),
    })
}

struct Column {
    index: usize,
    name: Vec<u8>,
    format: NumberFormat,
}

enum NumberFormat {
    Float32,
    Float64,
    Integer,
}

fn write_header(writer: &mut impl Write, columns: &[Column]) -> io::Result<()> {
    writer.write_all(b"EC-Lab ASCII FILE\r\nNb header lines : 3\r\n")?;
    for (position, column) in columns.iter().enumerate() {
        if position > 0 {
            writer.write_all(b"\t")?;
        }
        writer.write_all(&column.name)?;
    }
    writer.write_all(b"\r\n")
}

fn write_rows(
    writer: &mut impl Write,
    columns: &[Column],
    channels: &[Vec<f64>],
) -> io::Result<()> {
    for (row, _) in channels[0].iter().enumerate() {
        for (position, column) in columns.iter().enumerate() {
            if position > 0 {
                writer.write_all(b"\t")?;
            }
            let value = channels[column.index][row];
            match column.format {
                NumberFormat::Float32 => write!(writer, "{:e}", value as f32)?,
                NumberFormat::Float64 => write!(writer, "{value:e}")?,
                NumberFormat::Integer => write!(writer, "{value}")?,
            }
        }
        writer.write_all(b"\r\n")?;
    }
    Ok(())
}
