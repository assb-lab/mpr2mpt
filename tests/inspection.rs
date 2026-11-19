use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use mpr2mpt::{ConversionError, convert, inspect};

fn write_fixture(path: &Path, count: u32, include_metadata: bool) -> Vec<u8> {
    let mut bytes = b"BIO-LOGIC MODULAR FILE\x1a".to_vec();
    bytes.resize(52, 0);
    if include_metadata {
        append_module(&mut bytes, b"VMP Set   ", 0, &[0x06]);
        let mut log = vec![0; 593];
        log[9] = 2;
        log[585..].copy_from_slice(&45_000.5_f64.to_le_bytes());
        append_module(&mut bytes, b"VMP LOG   ", 0, &log);
    }
    let mut data = count.to_le_bytes().to_vec();
    data.extend(4_u16.to_le_bytes());
    for id in [1_u16, 6, 4, 169] {
        data.extend(id.to_le_bytes());
    }
    data.resize(1007, 0);
    for index in 0..count {
        data.push(1);
        data.extend((index as f32 + 0.3).to_le_bytes());
        data.extend((43_202.043_968_823_85_f64 + f64::from(index)).to_le_bytes());
        data.extend((-3.6e-30_f32).to_le_bytes());
    }
    append_module(&mut bytes, b"VMP data  ", 11, &data);
    fs::write(path, &bytes).unwrap();
    bytes
}

fn append_module(bytes: &mut Vec<u8>, label: &[u8; 10], version: u32, body: &[u8]) {
    bytes.extend(b"MODULE");
    bytes.extend(label);
    bytes.extend([b' '; 25]);
    bytes.extend((body.len() as u32).to_le_bytes());
    bytes.extend(version.to_le_bytes());
    bytes.extend(b"01/01/26");
    bytes.extend(body);
}

#[test]
fn samples_the_first_hundred_records_in_export_order_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("test.mpr");
    let now = UNIX_EPOCH + std::time::Duration::from_secs(123_456);
    let bytes = write_fixture(&input, 107, true);
    fs::File::options()
        .write(true)
        .open(&input)
        .unwrap()
        .set_modified(now)
        .unwrap();

    let preview = inspect(&input).unwrap();

    assert_eq!(preview.total_rows, 107);
    assert_eq!(preview.headings, ["mode", "Ewe/V", "time/s", "Cs/µF"]);
    assert_eq!(preview.rows.len(), 100);
    let metadata: std::collections::HashMap<_, _> = preview.metadata.into_iter().collect();
    assert_eq!(metadata.get("Technique").unwrap(), "Cyclic Voltammetry");
    assert_eq!(metadata.get("Channel index").unwrap(), "2");
    assert_eq!(metadata.get("Data module version").unwrap(), "11");
    assert!(
        metadata
            .get("Acquisition start (local)")
            .is_some_and(|t| t.contains("12:00"))
    );
    assert_eq!(fs::read(&input).unwrap(), bytes);
    assert_eq!(fs::metadata(&input).unwrap().modified().unwrap(), now);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);

    let report = convert(&input, None).unwrap();
    let output: String = fs::read(report.output)
        .unwrap()
        .into_iter()
        .map(char::from)
        .collect();
    let exported_rows: Vec<Vec<&str>> = output
        .lines()
        .skip(3)
        .take(100)
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(preview.rows, exported_rows);
}

#[test]
fn omits_missing_metadata_in_an_empty_measurement() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("empty.mpr");
    write_fixture(&input, 0, false);
    let preview = inspect(&input).unwrap();
    assert_eq!(preview.total_rows, 0);
    assert!(preview.rows.is_empty());
    assert_eq!(
        preview.metadata,
        [("Data module version".into(), "11".into())]
    );
}

#[test]
fn skips_metadata_whose_bytes_fall_outside_the_module() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("short_metadata.mpr");
    let complete = write_fixture(&input, 1, true);
    let mut bytes = complete[..52].to_vec();
    append_module(&mut bytes, b"VMP Set   ", 0, &[]);
    append_module(&mut bytes, b"VMP LOG   ", 0, &[]);
    bytes.extend_from_slice(&complete[52..]);
    fs::write(&input, bytes).unwrap();

    let preview = inspect(&input).unwrap();
    assert_eq!(
        preview.metadata,
        [("Data module version".into(), "11".into())]
    );
}

#[test]
fn refuses_the_same_ambiguous_layout_as_conversion() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("duplicate.mpr");
    let mut bytes = write_fixture(&input, 1, false);
    bytes.extend_from_within(52..);
    fs::write(&input, bytes).unwrap();

    let Err(error) = inspect(&input) else {
        panic!("ambiguous layout was accepted")
    };
    assert!(matches!(error, ConversionError::Layout { .. }));
    assert!(error.to_string().contains("multiple VMP data modules"));
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}
