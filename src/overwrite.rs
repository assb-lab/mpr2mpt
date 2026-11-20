use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf, absolute};
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::ConversionError;

/// A snapshot of one existing destination shown in the overwrite confirmation.
#[derive(Debug, Clone)]
pub struct OverwriteApproval {
    path: PathBuf,
    parent: ParentIdentity,
    snapshot: Snapshot,
}

impl OverwriteApproval {
    /// Capture a regular result file. Missing destinations require no approval.
    pub fn capture(path: &Path) -> Result<Option<Self>, ConversionError> {
        let path = absolute(path).map_err(|source| output_error(path, source))?;
        let before = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(output_error(&path, source)),
        };
        if !before.is_file() || is_source_extension(&path) {
            return Err(ConversionError::OverwriteProtected(path));
        }

        let parent =
            ParentIdentity::capture(&path).map_err(|source| output_error(&path, source))?;
        let snapshot = Snapshot::new(&before);
        let file = open_for_signature(&path).map_err(|source| output_error(&path, source))?;
        let opened = file
            .metadata()
            .map_err(|source| output_error(&path, source))?;
        if !opened.is_file() || snapshot != Snapshot::new(&opened) {
            return Err(ConversionError::OverwriteChanged(path));
        }
        let mut prefix = Vec::new();
        file.take(32)
            .read_to_end(&mut prefix)
            .map_err(|source| output_error(&path, source))?;
        let prefix = prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&prefix);
        if [
            &b"BIO-LOGIC MODULAR FILE"[..],
            b"EC-Lab SETTING FILE",
            b"BT-Lab SETTING FILE",
        ]
        .iter()
        .any(|magic| prefix.starts_with(magic))
        {
            return Err(ConversionError::OverwriteProtected(path));
        }

        let approval = Self {
            path,
            parent,
            snapshot,
        };
        approval.validate(&approval.path, &[])?;
        Ok(Some(approval))
    }

    /// The exact destination approved during preflight.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Recheck and publish `temporary` onto the approved destination.
    /// On Unix, revalidates and renames inside the verified parent directory descriptor
    /// to prevent parent-directory symlink redirection between validation and publish.
    pub(crate) fn publish_temp(
        &self,
        temporary: tempfile::NamedTempFile,
        output: &Path,
        inputs: &[PathBuf],
    ) -> Result<(), ConversionError> {
        self.validate(output, inputs)?;
        #[cfg(unix)]
        {
            use rustix::fs::{CWD, Mode, OFlags, openat, renameat};

            let name = self
                .path
                .file_name()
                .ok_or_else(|| ConversionError::OutputName(self.path.clone()))?;
            let parent_fd = File::from(
                openat(
                    CWD,
                    &self.parent.canonical,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|source| output_error(&self.path, source.into()))?,
            );
            let parent_meta = parent_fd
                .metadata()
                .map_err(|source| output_error(&self.path, source))?;
            if (parent_meta.dev(), parent_meta.ino()) != self.parent.identity {
                return Err(ConversionError::OverwriteChanged(self.path.clone()));
            }
            let target_file = match openat(
                &parent_fd,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => File::from(fd),
                Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP) => {
                    return Err(ConversionError::OverwriteChanged(self.path.clone()));
                }
                Err(source) => return Err(output_error(&self.path, source.into())),
            };
            let target_meta = target_file
                .metadata()
                .map_err(|source| output_error(&self.path, source))?;
            if !target_meta.is_file() || self.snapshot != Snapshot::new(&target_meta) {
                return Err(ConversionError::OverwriteChanged(self.path.clone()));
            }
            if is_input_alias(&self.path, &target_meta, inputs) {
                return Err(ConversionError::OverwriteProtected(self.path.clone()));
            }
            drop(target_file);

            // Note: POSIX renameat cannot atomically assert the target inode, so a
            // narrow same-directory race remains between target_file validation and renameat.
            let temp_path = temporary.into_temp_path();
            let temp_name = temp_path
                .file_name()
                .ok_or_else(|| ConversionError::OutputName(self.path.clone()))?;
            renameat(&parent_fd, temp_name, &parent_fd, name)
                .map_err(|source| output_error(&self.path, source.into()))?;
            let _ = temp_path.keep();
            Ok(())
        }
        #[cfg(not(unix))]
        {
            temporary
                .persist(output)
                .map_err(|error| output_error(output, error.error))?;
            Ok(())
        }
    }

    /// Recheck immediately before publication so stale approvals are rejected.
    pub(crate) fn validate(
        &self,
        output: &Path,
        inputs: &[PathBuf],
    ) -> Result<(), ConversionError> {
        let output = absolute(output).map_err(|source| output_error(output, source))?;
        if output != self.path {
            return Err(ConversionError::OverwriteChanged(output));
        }
        let metadata = match fs::symlink_metadata(&output) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ConversionError::OverwriteChanged(output));
            }
            Err(source) => return Err(output_error(&output, source)),
        };
        if !metadata.is_file() || self.snapshot != Snapshot::new(&metadata) {
            return Err(ConversionError::OverwriteChanged(output));
        }
        let parent =
            ParentIdentity::capture(&output).map_err(|source| output_error(&output, source))?;
        if self.parent != parent {
            return Err(ConversionError::OverwriteChanged(output));
        }
        if is_input_alias(&output, &metadata, inputs) {
            return Err(ConversionError::OverwriteProtected(output));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParentIdentity {
    canonical: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl ParentIdentity {
    fn capture(output: &Path) -> io::Result<Self> {
        let parent = output.parent().ok_or(io::ErrorKind::InvalidInput)?;
        let canonical = parent.canonicalize()?;
        #[cfg(unix)]
        let identity = {
            let metadata = fs::metadata(&canonical)?;
            (metadata.dev(), metadata.ino())
        };
        Ok(Self {
            canonical,
            #[cfg(unix)]
            identity,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(not(unix))]
    created: Option<SystemTime>,
}

impl Snapshot {
    fn new(metadata: &Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            #[cfg(not(unix))]
            created: metadata.created().ok(),
        }
    }
}

fn is_source_extension(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        ["mpr", "mps", "mpl", "mpp", "mpls", "mpsl"]
            .iter()
            .any(|source| extension.eq_ignore_ascii_case(source))
    })
}

#[cfg(unix)]
fn open_for_signature(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    // Keep a pathname race from redirecting the signature peek into a symlink or FIFO.
    Ok(File::from(open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}

#[cfg(not(unix))]
fn open_for_signature(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn is_input_alias(output: &Path, metadata: &Metadata, inputs: &[PathBuf]) -> bool {
    let output_id = (metadata.dev(), metadata.ino());
    inputs.iter().any(|input| {
        input == output
            || fs::metadata(input).is_ok_and(|other| (other.dev(), other.ino()) == output_id)
    })
}

#[cfg(not(unix))]
fn is_input_alias(output: &Path, _metadata: &Metadata, inputs: &[PathBuf]) -> bool {
    let resolved = fs::canonicalize(output).ok();
    inputs.iter().any(|input| {
        input == output
            || resolved.as_ref().is_some_and(|resolved| {
                fs::canonicalize(input).is_ok_and(|other| other == *resolved)
            })
    })
}

fn output_error(path: &Path, source: io::Error) -> ConversionError {
    ConversionError::OutputIo {
        path: path.into(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn does_not_approve_a_missing_destination() {
        let folder = tempdir().unwrap();
        let output = folder.path().join("later.mpt");
        assert!(OverwriteApproval::capture(&output).unwrap().is_none());
        assert!(!output.exists());
    }

    #[test]
    fn approves_only_the_selected_existing_file() {
        let folder = tempdir().unwrap();
        let output = folder.path().join("output.txt");
        fs::write(&output, b"previous result").unwrap();
        let approval = OverwriteApproval::capture(&output).unwrap().unwrap();
        assert_eq!(approval.path(), output);
        approval.validate(&output, &[]).unwrap();
        assert!(matches!(
            approval.validate(&folder.path().join("different.txt"), &[]),
            Err(ConversionError::OverwriteChanged(_))
        ));
        assert_eq!(fs::read(output).unwrap(), b"previous result");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_redirected_parent_even_when_both_outputs_are_hard_links() {
        use std::os::unix::fs::symlink;

        let folder = tempdir().unwrap();
        let first = folder.path().join("first");
        let second = folder.path().join("second");
        let parent_alias = folder.path().join("current");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(first.join("output.mpt"), b"previous result").unwrap();
        fs::hard_link(first.join("output.mpt"), second.join("output.mpt")).unwrap();
        symlink(&first, &parent_alias).unwrap();
        let output = parent_alias.join("output.mpt");
        let approval = OverwriteApproval::capture(&output).unwrap().unwrap();

        fs::remove_file(&parent_alias).unwrap();
        symlink(&second, &parent_alias).unwrap();
        assert!(matches!(
            approval.validate(&output, &[]),
            Err(ConversionError::OverwriteChanged(_))
        ));
    }

    #[test]
    fn refuses_output_changed_in_place() {
        let folder = tempdir().unwrap();
        let output = folder.path().join("output.mpt");
        fs::write(&output, b"older result").unwrap();
        let approval = OverwriteApproval::capture(&output).unwrap().unwrap();
        fs::write(&output, b"updated result with more data").unwrap();
        assert!(matches!(
            approval.validate(&output, &[]),
            Err(ConversionError::OverwriteChanged(_))
        ));
    }

    #[test]
    fn refuses_renamed_or_recreated_output() {
        let folder = tempdir().unwrap();
        let output = folder.path().join("output.mpt");
        fs::write(&output, b"previous result").unwrap();
        let approval = OverwriteApproval::capture(&output).unwrap().unwrap();
        fs::rename(&output, folder.path().join("original.mpt")).unwrap();
        assert!(matches!(
            approval.validate(&output, &[]),
            Err(ConversionError::OverwriteChanged(_))
        ));
        fs::write(&output, b"previous result").unwrap();
        assert!(matches!(
            approval.validate(&output, &[]),
            Err(ConversionError::OverwriteChanged(_))
        ));
    }

    #[test]
    fn refuses_raw_signatures_even_with_text_suffix() {
        let folder = tempdir().unwrap();
        for (filename, content) in [
            ("binary.txt", &b"BIO-LOGIC MODULAR FILE\x1a"[..]),
            ("settings.txt", &b"\xef\xbb\xbfEC-Lab SETTING FILE\r\n"[..]),
        ] {
            let output = folder.path().join(filename);
            fs::write(&output, content).unwrap();
            assert!(matches!(
                OverwriteApproval::capture(&output),
                Err(ConversionError::OverwriteProtected(_))
            ));
        }
    }

    #[test]
    fn refuses_a_directory() {
        let folder = tempdir().unwrap();
        assert!(matches!(
            OverwriteApproval::capture(folder.path()),
            Err(ConversionError::OverwriteProtected(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_even_when_dangling() {
        let folder = tempdir().unwrap();
        let link = folder.path().join("output.mpt");
        std::os::unix::fs::symlink(folder.path().join("missing.mpt"), &link).unwrap();
        assert!(matches!(
            OverwriteApproval::capture(&link),
            Err(ConversionError::OverwriteProtected(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_hardlink_aliases_of_protected_inputs() {
        let folder = tempdir().unwrap();
        let output = folder.path().join("output.mpt");
        let input = folder.path().join("input.mpr");
        fs::write(&input, b"protected fixture").unwrap();
        fs::hard_link(&input, &output).unwrap();
        let approval = OverwriteApproval::capture(&output).unwrap().unwrap();
        assert!(matches!(
            approval.validate(&output, std::slice::from_ref(&input)),
            Err(ConversionError::OverwriteProtected(_))
        ));
        let missing = folder.path().join("missing.mpr");
        approval.validate(&output, &[missing]).unwrap();
        assert_eq!(fs::read(input).unwrap(), b"protected fixture");
    }
}
