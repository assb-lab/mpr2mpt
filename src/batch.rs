use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;

use rayon::prelude::*;
#[cfg(unix)]
use rustix::fs::{Mode, OFlags, open, openat};

use crate::overwrite::OverwriteApproval;
use crate::{ConversionError, ConversionReport, convert, convert_with_approval};

const MAX_WORKERS: usize = 2;

/// One conversion. An absent output uses the same default as [`convert`].
#[derive(Debug)]
pub struct BatchJob {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
}

impl BatchJob {
    pub fn destination(&self) -> PathBuf {
        self.output
            .clone()
            .unwrap_or_else(|| self.input.with_extension("mpt"))
    }
}

/// A conversion result and its index in the original batch.
#[derive(Debug)]
pub struct BatchOutcome {
    pub index: usize,
    pub result: Result<ConversionReport, ConversionError>,
}

/// Preflight batch destinations to identify duplicate or invalid outputs before prompting.
pub fn preflight_destinations(jobs: &[BatchJob]) -> Vec<Option<ConversionError>> {
    let outputs: Vec<_> = jobs.iter().map(BatchJob::destination).collect();
    check_destinations(&outputs)
        .into_iter()
        .map(|check| check.error)
        .collect()
}

/// Convert the jobs and report each outcome as it becomes available.
/// Callbacks may run concurrently. Destination checks reject known collisions before writing.
/// If filesystem alias checks are unavailable, exact duplicates are still rejected and other
/// jobs in that directory are executed in input order using the no-clobber converter.
pub fn convert_batch(
    jobs: &[BatchJob],
    report: impl Fn(BatchOutcome) + Sync,
) -> Result<(), rayon::ThreadPoolBuildError> {
    convert_batch_with_approvals(jobs, &HashMap::new(), report)
}

/// Execute a batch with explicit replacement approvals for particular job indices.
pub fn convert_batch_with_approvals(
    jobs: &[BatchJob],
    approvals: &HashMap<usize, OverwriteApproval>,
    report: impl Fn(BatchOutcome) + Sync,
) -> Result<(), rayon::ThreadPoolBuildError> {
    let outputs: Vec<_> = jobs.iter().map(BatchJob::destination).collect();
    let protected: Vec<_> = if approvals.is_empty() {
        Vec::new()
    } else {
        jobs.iter().map(|job| job.input.clone()).collect()
    };
    let checks = check_destinations(&outputs);
    let run = |(index, check): (usize, DestinationCheck)| {
        let job = &jobs[index];
        let result = check.error.map_or_else(
            || {
                if let Some(approval) = approvals.get(&index) {
                    convert_with_approval(
                        &job.input,
                        job.output.as_deref(),
                        Some(approval),
                        &protected,
                    )
                } else {
                    convert(&job.input, job.output.as_deref())
                }
            },
            Err,
        );
        report(BatchOutcome { index, result });
    };

    let (serial, parallel): (Vec<_>, Vec<_>) = checks
        .into_iter()
        .enumerate()
        .partition(|(_, check)| check.in_order);
    serial.into_iter().for_each(&run);
    let threads = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_WORKERS)
        .min(parallel.len());
    if threads > 1 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()?
            .install(|| parallel.into_par_iter().for_each(run));
    } else {
        parallel.into_iter().for_each(run);
    }
    Ok(())
}

#[derive(Default)]
struct DestinationCheck {
    error: Option<ConversionError>,
    in_order: bool,
}

fn check_destinations(outputs: &[PathBuf]) -> Vec<DestinationCheck> {
    let mut checks: Vec<_> = outputs
        .iter()
        .map(|_| DestinationCheck::default())
        .collect();
    if outputs.len() <= 1 {
        return checks;
    }

    let mut groups: HashMap<PathBuf, Vec<(usize, &OsStr)>> = HashMap::new();
    for (index, output) in outputs.iter().enumerate() {
        if crate::has_invalid_output_name(output) {
            // Path::file_name normalizes trailing separators and "/.", but the raw path asks for a directory.
            checks[index].error = Some(ConversionError::OutputName(output.clone()));
            continue;
        }
        if let (Some(parent), Some(name)) = (output.parent(), output.file_name()) {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            match fs::canonicalize(parent) {
                Ok(resolved) => groups.entry(resolved).or_default().push((index, name)),
                Err(error) => {
                    checks[index].error = Some(verification_error(output, &error.to_string()));
                }
            }
        }
    }

    for (parent, group) in groups.into_iter().filter(|(_, group)| group.len() > 1) {
        if check_group(&parent, &group, outputs, &mut checks).is_err() {
            // A destination may permit creating files while denying subdirectories.
            // Reject exact duplicates and let no-clobber decide any remaining aliases
            // in input order when native probing is not available.
            let mut owners = HashMap::new();
            for (index, name) in group {
                checks[index].in_order = true;
                if let Some(owner) = owners.insert(name, index) {
                    mark_duplicates([owner, index], outputs, &mut checks);
                }
            }
        }
    }
    checks
}

fn mark_duplicates(indices: [usize; 2], outputs: &[PathBuf], checks: &mut [DestinationCheck]) {
    for index in indices {
        checks[index].error = Some(ConversionError::DuplicateBatchOutput(
            outputs[index].clone(),
        ));
    }
}

fn verification_error(output: &Path, reason: &str) -> ConversionError {
    ConversionError::BatchOutputVerification {
        path: output.to_path_buf(),
        reason: reason.into(),
    }
}

fn check_group(
    parent: &Path,
    group: &[(usize, &OsStr)],
    outputs: &[PathBuf],
    checks: &mut [DestinationCheck],
) -> io::Result<()> {
    // Let the destination filesystem compare names in a private lookup directory.
    let mut builder = tempfile::Builder::new();
    builder.prefix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let scratch = builder.tempdir_in(parent)?;
    let directory = ProbeDirectory::open(scratch.path())?;
    for &(index, name) in group {
        match probe_destination(&directory, name, index) {
            Ok(None) => {}
            Ok(Some(owner))
                if owner < index && group.binary_search_by_key(&owner, |(job, _)| *job).is_ok() =>
            {
                mark_duplicates([owner, index], outputs, checks);
            }
            Ok(Some(_)) => {
                checks[index].error =
                    Some(verification_error(&outputs[index], "unknown probe owner"));
            }
            Err(error) => {
                checks[index].error = Some(verification_error(&outputs[index], &error.to_string()));
            }
        }
    }
    drop(directory);
    scratch.close()
}

struct ProbeDirectory {
    #[cfg(unix)]
    directory: File,
    #[cfg(not(unix))]
    path: PathBuf,
}

impl ProbeDirectory {
    fn open(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let directory = open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )?;
            Ok(Self {
                directory: File::from(directory),
            })
        }
        #[cfg(not(unix))]
        Ok(Self { path: path.into() })
    }

    fn marker(&self, name: &OsStr, create: bool) -> io::Result<File> {
        #[cfg(unix)]
        {
            // A valid output can be near PATH_MAX, so never append its basename to
            // the absolute scratch path. Let the directory descriptor anchor it.
            let flags = OFlags::CLOEXEC | OFlags::NOFOLLOW;
            let (flags, mode) = if create {
                (
                    flags | OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
                    Mode::RUSR | Mode::WUSR,
                )
            } else {
                (flags | OFlags::RDONLY, Mode::empty())
            };
            Ok(File::from(openat(&self.directory, name, flags, mode)?))
        }
        #[cfg(not(unix))]
        fs::OpenOptions::new()
            .read(!create)
            .write(create)
            .create_new(create)
            .open(self.path.join(name))
    }
}

fn probe_destination(
    directory: &ProbeDirectory,
    name: &OsStr,
    index: usize,
) -> io::Result<Option<usize>> {
    match directory.marker(name, true) {
        Ok(mut file) => {
            file.write_all(&index.to_le_bytes())?;
            Ok(None)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let mut owner = [0; size_of::<usize>()];
            directory.marker(name, false)?.read_exact(&mut owner)?;
            Ok(Some(usize::from_le_bytes(owner)))
        }
        Err(error) => Err(error),
    }
}
