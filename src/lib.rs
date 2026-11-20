use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use thiserror::Error;

pub mod batch;
mod inspection;
pub mod overwrite;

use inspection::{Column, MprInput};
pub use inspection::{Inspection, inspect};
use overwrite::OverwriteApproval;

const CHUNK_ROWS: u64 = 4096;

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
    #[error("cannot overwrite protected or non-regular output: {0}")]
    OverwriteProtected(PathBuf),
    #[error("output changed after overwrite confirmation: {0}")]
    OverwriteChanged(PathBuf),
    #[error("multiple batch jobs resolve to the same output: {0}")]
    DuplicateBatchOutput(PathBuf),
    #[error("cannot verify batch output {path}: {reason}")]
    BatchOutputVerification { path: PathBuf, reason: String },
    #[error("cannot write MPT {path}: {source}")]
    OutputIo { path: PathBuf, source: io::Error },
}

/// Convert stored MPR columns to an EC-Lab text table and publish it without overwriting.
/// When `output` is absent, replace the input's extension with `.mpt`.
pub fn convert(input: &Path, output: Option<&Path>) -> Result<ConversionReport, ConversionError> {
    convert_with_approval(input, output, None, &[])
}

pub(crate) fn has_invalid_output_name(output: &Path) -> bool {
    if output.file_name().is_none() {
        return true;
    }
    let bytes = output.as_os_str().as_encoded_bytes();
    bytes
        .strip_suffix(b".")
        .unwrap_or(bytes)
        .last()
        .is_some_and(|byte| std::path::is_separator(char::from(*byte)))
}

fn convert_with_approval(
    input: &Path,
    output: Option<&Path>,
    approval: Option<&OverwriteApproval>,
    protected: &[PathBuf],
) -> Result<ConversionReport, ConversionError> {
    let output = output.map_or_else(|| input.with_extension("mpt"), Path::to_path_buf);
    if has_invalid_output_name(&output) {
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
    if let Some(approval) = approval {
        approval.validate(&output, protected)?;
    } else {
        match fs::symlink_metadata(&output) {
            Ok(_) => return Err(ConversionError::OutputExists(output)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(output_error(error)),
        }
    }
    let mut source = MprInput::open(input)?;

    let parent = output.parent().filter(|path| !path.as_os_str().is_empty());
    let mut temporary =
        NamedTempFile::new_in(parent.unwrap_or_else(|| Path::new("."))).map_err(output_error)?;
    let mut writer = BufWriter::new(temporary.as_file_mut());
    write_header(&mut writer, &source.columns).map_err(output_error)?;
    let mut first = 0;
    while first < source.total_rows {
        let count = (source.total_rows - first).min(CHUNK_ROWS);
        let channels = source.read_rows(first, count)?;
        write_rows(&mut writer, &source.columns, &channels).map_err(output_error)?;
        first += count;
    }
    writer.flush().map_err(output_error)?;
    drop(writer);
    if let Some(approval) = approval {
        approval.publish_temp(temporary, &output, protected)?;
    } else {
        temporary
            .persist_noclobber(&output)
            .map_err(|error| output_error(error.error))?;
    }
    Ok(ConversionReport {
        output,
        rows: source.total_rows,
        columns: source.columns.len(),
    })
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
            write!(writer, "{}", column.value(channels[column.index][row]))?;
        }
        writer.write_all(b"\r\n")?;
    }
    Ok(())
}
