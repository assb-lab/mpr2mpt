use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mpr2mpt"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn module(version: u32, body: &[u8]) -> Vec<u8> {
    let mut bytes = b"MODULEVMP data  ".to_vec();
    bytes.extend([b' '; 25]);
    bytes.extend((body.len() as u32).to_le_bytes());
    bytes.extend(version.to_le_bytes());
    bytes.extend(b"01/01/26");
    bytes.extend(body);
    bytes
}

fn data_file(version: u32, ids: &[u16], row: &[u8]) -> Vec<u8> {
    let mut body = 1_u32.to_le_bytes().to_vec();
    if version == 11 {
        body.extend((ids.len() as u16).to_le_bytes());
    } else {
        body.push(ids.len() as u8);
    }
    for id in ids {
        body.extend(id.to_le_bytes());
    }
    body.resize(if version == 11 { 1007 } else { 405 }, 0);
    body.extend(row);

    let mut mpr = b"BIO-LOGIC MODULAR FILE\x1a".to_vec();
    mpr.resize(52, 0);
    mpr.extend(module(version, &body));
    mpr
}

fn sample() -> Vec<u8> {
    let mut row = vec![0b1011_0110];
    row.extend(0.3_f32.to_le_bytes());
    row.extend(43_202.043_968_823_85_f64.to_le_bytes());
    row.extend((-3.6e-30_f64).to_le_bytes());
    row.extend((-12.5_f32).to_le_bytes());
    row.extend(3.0_f64.to_le_bytes());
    row.extend(2.0_f32.to_le_bytes());
    data_file(11, &[1, 2, 3, 21, 31, 65, 6, 4, 11, 8, 24, 169], &row)
}

#[test]
fn writes_stored_values_and_column_order_to_sibling_mpt() {
    let dir = tempfile::tempdir().unwrap();
    let mpr = sample();
    fs::write(dir.path().join("試料.MPR"), &mpr).unwrap();
    let result = run(dir.path(), &["試料.MPR"]);
    assert!(result.status.success(), "{result:?}");
    assert_eq!(fs::read(dir.path().join("試料.MPR")).unwrap(), mpr);
    let mpt = fs::read(dir.path().join("試料.mpt")).unwrap();
    let mpt: String = mpt.into_iter().map(char::from).collect();
    let lines: Vec<_> = mpt.split_terminator("\r\n").collect();
    assert!(mpt.ends_with("\r\n"));
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], "EC-Lab ASCII FILE");
    assert_eq!(lines[1], "Nb header lines : 3");
    assert_eq!(
        lines[2],
        "mode\tox/red\terror\tcontrol changes\tNs changes\tcounter inc.\tEwe/V\ttime/s\t<I>/mA\tI/mA\tcycle number\tCs/µF"
    );
    let values: Vec<_> = lines[3].split('\t').collect();
    assert_eq!(&values[..6], &["2", "1", "0", "1", "1", "1"]);
    assert_eq!(values[6].parse::<f32>().unwrap(), 0.3);
    assert_eq!(values[7].parse::<f64>().unwrap(), 43_202.043_968_823_85);
    assert_eq!(values[8].parse::<f64>().unwrap(), -3.6e-30);
    assert_eq!(values[9].parse::<f32>().unwrap(), -12.5);
    assert_eq!(values[10].parse::<f64>().unwrap(), 3.0);
    assert_eq!(values[11].parse::<f32>().unwrap(), 2.0);
}

#[test]
fn writes_to_explicit_output_and_refuses_existing_entries() {
    let dir = tempfile::tempdir().unwrap();
    let mpr = sample();
    fs::write(dir.path().join("test.mpr"), &mpr).unwrap();
    let result = run(dir.path(), &["test.mpr", "-o", "result.mpt"]);
    assert!(result.status.success(), "{result:?}");
    assert!(!dir.path().join("test.mpt").exists());
    let existing_output = fs::read(dir.path().join("result.mpt")).unwrap();
    for dest in ["result.mpt", "test.mpr"] {
        let result = run(dir.path(), &["test.mpr", "-o", dest]);
        assert!(!result.status.success(), "{dest}");
        assert!(String::from_utf8_lossy(&result.stderr).contains(dest));
    }
    assert_eq!(
        fs::read(dir.path().join("result.mpt")).unwrap(),
        existing_output
    );
    assert_eq!(fs::read(dir.path().join("test.mpr")).unwrap(), mpr);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn refuses_truncated_and_ambiguous_layouts_without_creating_output() {
    let dir = tempfile::tempdir().unwrap();
    let mut truncated = sample();
    truncated.pop();
    let mut duplicate = sample();
    duplicate.extend_from_slice(&sample()[52..]);
    let inputs = [
        ("truncated.MPR", truncated, "past the end"),
        ("duplicate.MPR", duplicate, "multiple VMP data"),
        ("late-flags.MPR", data_file(2, &[4, 1], &[0; 9]), "flag"),
        ("unknown.MPR", data_file(11, &[999], &[0; 4]), "999"),
        ("unrecognized.MPR", data_file(7, &[4], &[0; 8]), "version 7"),
    ];
    let input_count = inputs.len();
    for (name, data, diagnostic) in inputs {
        fs::write(dir.path().join(name), &data).unwrap();
        let result = run(dir.path(), &[name]);
        assert!(!result.status.success(), "{name}");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains(name) && stderr.contains(diagnostic),
            "{stderr}"
        );
    }
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), input_count);
}
