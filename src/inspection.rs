use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::Path;

use openreadout_core::{Dataset, FormatReader};
use openreadout_echem::MprReader;

use crate::ConversionError;

const FLAG_IDS: [u16; 6] = [1, 2, 3, 21, 31, 65];
const TIME_ID: u16 = 4;
const PREVIEW_ROWS: u64 = 100;

/// Read-only preview of the stored measurement and its available metadata.
#[derive(Debug)]
pub struct Inspection {
    pub metadata: Vec<(String, String)>,
    pub headings: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: u64,
}

/// Inspect the first 100 rows in the same order and format used by MPT export.
pub fn inspect(input: &Path) -> Result<Inspection, ConversionError> {
    let mut source = MprInput::open(input)?;
    let samples = source.read_rows(0, source.total_rows.min(PREVIEW_ROWS))?;
    let headings = source
        .columns
        .iter()
        .map(|column| column.name.iter().copied().map(char::from).collect())
        .collect();
    let rows = samples[0]
        .iter()
        .enumerate()
        .map(|(row, _)| {
            source
                .columns
                .iter()
                .map(|column| column.value(samples[column.index][row]).to_string())
                .collect()
        })
        .collect();
    Ok(Inspection {
        metadata: source.metadata,
        headings,
        rows,
        total_rows: source.total_rows,
    })
}

pub(super) struct MprInput<'a> {
    input: &'a Path,
    dataset: Box<dyn Dataset>,
    pub(super) total_rows: u64,
    pub(super) columns: Vec<Column>,
    metadata: Vec<(String, String)>,
}

impl<'a> MprInput<'a> {
    pub(super) fn open(input: &'a Path) -> Result<Self, ConversionError> {
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
        let dataset = MprReader.open(input).map_err(decode)?;
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
        let mut metadata = Vec::new();
        // OpenReadout 0.4 may read the technique and channel past short modules.
        if modules
            .iter()
            .find(|module| module["name"] == "VMP Set")
            .and_then(|module| module["size"].as_u64())
            .is_some_and(|size| size > 0)
            && let Some(technique) = eclab["technique"].as_str()
        {
            metadata.push(("Technique".into(), technique.into()));
        }
        if let Some(start) = eclab["acquisition_started"].as_str() {
            metadata.push(("Acquisition start (local)".into(), start.into()));
        }
        if modules
            .iter()
            .find(|module| module["name"] == "VMP LOG")
            .and_then(|module| module["size"].as_u64())
            .is_some_and(|size| size >= 10)
            && let Some(index) = eclab["channel_index"].as_u64()
        {
            metadata.push(("Channel index".into(), index.to_string()));
        }
        if let Some(version) = modules
            .iter()
            .find(|module| module["name"] == "VMP data")
            .and_then(|module| module["version"].as_u64())
        {
            metadata.push(("Data module version".into(), version.to_string()));
        }

        Ok(Self {
            input,
            dataset,
            total_rows: trace.sample_count,
            columns,
            metadata,
        })
    }

    pub(super) fn read_rows(
        &mut self,
        first: u64,
        count: u64,
    ) -> Result<Vec<Vec<f64>>, ConversionError> {
        let chunk = self
            .dataset
            .read_trace(0, 0, first, count)
            .map_err(|source| ConversionError::Decode {
                path: self.input.into(),
                source,
            })?;
        if chunk.first_sample != first
            || chunk.channels.len() != self.columns.len()
            || chunk
                .channels
                .iter()
                .any(|values| values.len() as u64 != count)
        {
            return Err(ConversionError::Layout {
                path: self.input.into(),
                reason: "inconsistent measurement row count".into(),
            });
        }
        Ok(chunk.channels)
    }
}

pub(super) struct Column {
    pub(super) index: usize,
    pub(super) name: Vec<u8>,
    format: NumberFormat,
}

impl Column {
    pub(super) fn value(&self, value: f64) -> impl fmt::Display + '_ {
        FormattedValue {
            format: &self.format,
            value,
        }
    }
}

enum NumberFormat {
    Float32,
    Float64,
    Integer,
}

struct FormattedValue<'a> {
    format: &'a NumberFormat,
    value: f64,
}

impl fmt::Display for FormattedValue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.value;
        match self.format {
            NumberFormat::Float32 => write!(f, "{:e}", value as f32),
            NumberFormat::Float64 => write!(f, "{value:e}"),
            NumberFormat::Integer => write!(f, "{value}"),
        }
    }
}
