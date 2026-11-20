use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc;

use mpr2mpt::ConversionError;
use mpr2mpt::batch::{BatchJob, convert_batch, convert_batch_with_approvals};
use mpr2mpt::overwrite::OverwriteApproval;

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
fn batch_replaces_only_currently_approved_outputs_after_successful_conversion() {
    let dir = tempfile::tempdir().unwrap();
    let jobs: Vec<_> = ["approved", "unapproved", "corrupt", "changed", "fresh"]
        .into_iter()
        .map(|name| BatchJob {
            input: dir.path().join(name).with_extension("mpr"),
            output: None,
        })
        .collect();
    for (index, job) in jobs.iter().enumerate() {
        fs::write(&job.input, if index == 2 { vec![] } else { sample() }).unwrap();
        if index != 4 {
            fs::write(job.destination(), b"original").unwrap();
        }
    }
    let approvals: HashMap<_, _> = [0, 2, 3]
        .into_iter()
        .map(|index| {
            let approval = OverwriteApproval::capture(&jobs[index].destination())
                .unwrap()
                .unwrap();
            (index, approval)
        })
        .collect();
    fs::write(jobs[3].destination(), b"changed longer replacement").unwrap();

    let (sender, receiver) = mpsc::channel();
    convert_batch_with_approvals(&jobs, &approvals, |outcome| sender.send(outcome).unwrap())
        .unwrap();
    drop(sender);
    let mut outcomes: Vec<_> = receiver.into_iter().collect();
    outcomes.sort_by_key(|item| item.index);
    assert_eq!(outcomes.len(), jobs.len());
    assert!(outcomes[0].result.is_ok());
    assert!(outcomes[1].result.is_err());
    assert!(outcomes[2].result.is_err());
    assert!(matches!(
        outcomes[3].result,
        Err(ConversionError::OverwriteChanged(_))
    ));
    assert!(outcomes[4].result.is_ok());
    assert_eq!(fs::read(jobs[1].destination()).unwrap(), b"original");
    assert_eq!(fs::read(jobs[2].destination()).unwrap(), b"original");
    assert_eq!(
        fs::read(jobs[3].destination()).unwrap(),
        b"changed longer replacement"
    );
    assert_eq!(
        fs::read(jobs[0].destination()).unwrap(),
        fs::read(jobs[4].destination()).unwrap()
    );
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

#[test]
fn batch_keeps_processing_after_a_bad_input_or_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let sources = [
        ("first.mpr", sample()),
        ("broken.mpr", b"not an MPR".to_vec()),
        ("保護.MPR", sample()),
        ("last.MPR", sample()),
    ];
    let jobs: Vec<_> = sources
        .into_iter()
        .map(|(name, bytes)| {
            let input = dir.path().join(name);
            fs::write(&input, bytes).unwrap();
            BatchJob {
                input,
                output: None,
            }
        })
        .collect();
    let existing = jobs[2].input.with_extension("mpt");
    fs::write(&existing, b"keep this output").unwrap();
    let (sender, receiver) = mpsc::channel();

    convert_batch(&jobs, |outcome| sender.send(outcome).unwrap()).unwrap();
    drop(sender);

    let mut outcomes: Vec<_> = receiver.into_iter().collect();
    outcomes.sort_by_key(|outcome| outcome.index);
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.index)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    for index in [0, 3] {
        let report = outcomes[index].result.as_ref().unwrap();
        assert_eq!(report.output, jobs[index].input.with_extension("mpt"));
        assert_eq!(report.rows, 1);
        assert!(report.output.is_file());
    }
    assert!(matches!(
        outcomes[1].result,
        Err(ConversionError::Decode { .. })
    ));
    assert!(matches!(
        outcomes[2].result,
        Err(ConversionError::OutputExists(_))
    ));
    assert!(!jobs[1].input.with_extension("mpt").exists());
    assert_eq!(fs::read(existing).unwrap(), b"keep this output");
}

#[test]
fn batch_reports_every_duplicate_output_without_publishing_it() {
    let dir = tempfile::tempdir().unwrap();
    let subdir = dir.path().join("subdir");
    fs::create_dir(&subdir).unwrap();
    let jobs = [
        BatchJob {
            input: dir.path().join("first.mpr"),
            output: Some(dir.path().join("shared.mpt")),
        },
        BatchJob {
            input: dir.path().join("second.mpr"),
            output: Some(subdir.join("../shared.mpt")),
        },
        BatchJob {
            input: dir.path().join("third.mpr"),
            output: None,
        },
    ];
    for job in &jobs {
        fs::write(&job.input, sample()).unwrap();
    }
    let (sender, receiver) = mpsc::channel();

    convert_batch(&jobs, |outcome| sender.send(outcome).unwrap()).unwrap();
    drop(sender);

    let mut outcomes: Vec<_> = receiver.into_iter().collect();
    outcomes.sort_by_key(|outcome| outcome.index);
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.index)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    for outcome in &outcomes[..2] {
        assert!(matches!(
            &outcome.result,
            Err(ConversionError::DuplicateBatchOutput(path))
                if Some(path) == jobs[outcome.index].output.as_ref()
        ));
    }
    assert!(outcomes[2].result.as_ref().unwrap().output.is_file());
    assert!(!dir.path().join("shared.mpt").exists());
}

#[test]
fn batch_respects_native_case_aliases() {
    assert_native_batch_names("Shared.mpt", "shared.mpt");
}

#[test]
fn batch_respects_native_unicode_aliases() {
    assert_native_batch_names("測定 Café.mpt", "測定 Cafe\u{301}.mpt");
}

#[test]
fn batch_keeps_processing_after_an_invalid_output_basename() {
    let dir = tempfile::tempdir().unwrap();
    let long_name = format!("{}.mpt", "x".repeat(252));
    let invalid = dir.path().join(&long_name);
    let supported = fs::File::create_new(&invalid).is_ok();
    if supported {
        fs::remove_file(&invalid).unwrap();
    }
    let jobs = [
        ("first.mpr", long_name.as_str()),
        ("second.mpr", "valid.mpt"),
    ]
    .map(|(input, output)| BatchJob {
        input: dir.path().join(input),
        output: Some(dir.path().join(output)),
    });
    for job in &jobs {
        fs::write(&job.input, sample()).unwrap();
    }
    let (sender, receiver) = mpsc::channel();

    convert_batch(&jobs, |outcome| sender.send(outcome).unwrap()).unwrap();
    drop(sender);

    let mut outcomes: Vec<_> = receiver.into_iter().collect();
    outcomes.sort_by_key(|outcome| outcome.index);
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes[1].result.as_ref().unwrap().output.is_file());
    if supported {
        assert!(outcomes[0].result.is_ok());
    } else {
        assert!(matches!(
            &outcomes[0].result,
            Err(ConversionError::BatchOutputVerification { path, .. }) if path == &invalid
        ));
    }
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        3 + usize::from(supported)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn batch_converts_valid_outputs_near_path_max() {
    let dir = tempfile::tempdir().unwrap();
    let limit = libc::PATH_MAX as usize;
    let mut destination = fs::canonicalize(dir.path()).unwrap();
    let destination_len = limit - 36;
    while destination.as_os_str().len() + 200 < destination_len {
        destination.push("d".repeat(99));
        fs::create_dir(&destination).unwrap();
    }
    destination.push("d".repeat(destination_len - destination.as_os_str().len() - 1));
    fs::create_dir(&destination).unwrap();

    let jobs = ["alpha", "bravo"].map(|name| {
        let input = dir.path().join(format!("{name}.mpr"));
        fs::write(&input, sample()).unwrap();
        let output = destination.join(format!("{name}{}.mpt", "x".repeat(23)));
        let serial = mpr2mpt::convert(&input, Some(&output)).unwrap();
        assert_eq!(serial.output, output);
        fs::remove_file(serial.output).unwrap();
        BatchJob {
            input,
            output: Some(output),
        }
    });
    let (sender, receiver) = mpsc::channel();
    convert_batch(&jobs, |outcome| sender.send(outcome).unwrap()).unwrap();
    drop(sender);

    let outcomes: Vec<_> = receiver.into_iter().collect();
    assert_eq!(outcomes.len(), 2);
    for outcome in outcomes {
        let report = outcome.result.unwrap();
        assert_eq!(Some(&report.output), jobs[outcome.index].output.as_ref());
        assert!(report.output.is_file());
    }
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 2);
}

#[cfg(target_os = "macos")]
#[test]
fn batch_falls_back_when_acl_allows_files_but_forbids_scratch_directories() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("restricted");
    fs::create_dir(&destination).unwrap();
    let status = Command::new("/bin/chmod")
        .args(["+a", "everyone deny add_subdirectory"])
        .arg(&destination)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(fs::create_dir(destination.join("scratch")).is_err());

    let jobs = ["first", "second", "third"].map(|name| {
        let input = dir.path().join(format!("{name}.mpr"));
        fs::write(&input, sample()).unwrap();
        let output = Some(destination.join(if name == "first" {
            "valid.mpt"
        } else {
            "shared.mpt"
        }));
        BatchJob { input, output }
    });
    let serial = mpr2mpt::convert(&jobs[0].input, jobs[0].output.as_deref()).unwrap();
    fs::remove_file(serial.output).unwrap();
    let (sender, receiver) = mpsc::channel();
    convert_batch(&jobs, |outcome| sender.send(outcome).unwrap()).unwrap();
    drop(sender);

    let outcomes: Vec<_> = receiver.into_iter().collect();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(outcomes[0].index, 0);
    assert!(outcomes[0].result.as_ref().unwrap().output.is_file());
    assert_eq!(outcomes[1].index, 1);
    assert_eq!(outcomes[2].index, 2);
    for outcome in &outcomes[1..] {
        assert!(matches!(
            outcome.result,
            Err(ConversionError::DuplicateBatchOutput(_))
        ));
    }
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
}

#[test]
fn single_conversion_rejects_directory_syntax_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("sample.mpr");
    fs::write(&input, sample()).unwrap();
    for suffix in ["out.mpt/", "out.mpt/."] {
        let output = dir.path().join(suffix);
        assert!(
            matches!(
                mpr2mpt::convert(&input, Some(&output)),
                Err(ConversionError::OutputName(_))
            ),
            "{suffix}"
        );
    }
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn batch_does_not_group_directory_syntax_with_a_file_output() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("valid.mpt");
    let jobs = [output.join("."), output].map(|output| {
        let input = dir.path().join("sample.mpr");
        fs::write(&input, sample()).unwrap();
        BatchJob {
            input,
            output: Some(output),
        }
    });
    let (sender, receiver) = mpsc::channel();
    convert_batch(&jobs, |outcome| sender.send(outcome).unwrap()).unwrap();
    drop(sender);

    let mut outcomes: Vec<_> = receiver.into_iter().collect();
    outcomes.sort_by_key(|outcome| outcome.index);
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes[0].result.is_err());
    assert!(outcomes[1].result.as_ref().unwrap().output.is_file());
}

fn assert_native_batch_names(first: &str, second: &str) {
    let dir = tempfile::tempdir().unwrap();
    let probe = dir.path().join(format!("probe-{first}"));
    fs::write(&probe, b"test filesystem name equivalence").unwrap();
    let aliases = dir.path().join(format!("probe-{second}")).exists();
    fs::remove_file(probe).unwrap();
    let jobs = [("first.mpr", first), ("second.mpr", second)].map(|(input, output)| BatchJob {
        input: dir.path().join(input),
        output: Some(dir.path().join(output)),
    });
    for job in &jobs {
        fs::write(&job.input, sample()).unwrap();
    }
    let (sender, receiver) = mpsc::channel();
    convert_batch(&jobs, |outcome| sender.send(outcome).unwrap()).unwrap();
    drop(sender);

    let outcomes: Vec<_> = receiver.into_iter().collect();
    assert_eq!(outcomes.len(), 2);
    for outcome in outcomes {
        let dest = jobs[outcome.index].output.as_ref().unwrap();
        if aliases {
            assert!(matches!(
                outcome.result,
                Err(ConversionError::DuplicateBatchOutput(path)) if &path == dest
            ));
            assert!(!dest.exists());
        } else {
            assert_eq!(&outcome.result.unwrap().output, dest);
            assert!(dest.is_file());
        }
    }
    let expected_files = if aliases { 2 } else { 4 };
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), expected_files);
}
