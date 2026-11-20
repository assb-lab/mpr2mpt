use std::io;
use std::path::{Path, PathBuf};

pub(super) enum PathPart {
    Absolute,
    Parent,
    Name,
    Stem,
}

impl PathPart {
    pub(super) fn text(self, path: &Path) -> io::Result<&str> {
        let part = match self {
            Self::Absolute => Some(path.as_os_str()),
            Self::Parent => path.parent().map(Path::as_os_str),
            Self::Name => path.file_name(),
            Self::Stem => path.file_stem(),
        };
        part.and_then(std::ffi::OsStr::to_str).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "The selected path cannot be copied as UTF-8 text",
            )
        })
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn copy_text(text: &str) -> io::Result<()> {
    use std::process::Command;

    #[cfg(target_os = "macos")]
    {
        write_clipboard(Command::new("pbcopy").env("LC_ALL", "en_US.UTF-8"), text)
    }
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let result = write_clipboard(
                Command::new("wl-copy").args(["--type", "text/plain;charset=utf-8"]),
                text,
            );
            if result.is_ok() || std::env::var_os("DISPLAY").is_none() {
                return result;
            }
        }
        write_clipboard(
            Command::new("xclip").args(["-selection", "clipboard", "-in"]),
            text,
        )
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn write_clipboard(command: &mut std::process::Command, text: &str) -> io::Result<()> {
    use std::io::Write;
    use std::process::Stdio;

    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            io::Error::new(error.kind(), format!("Cannot start {program}: {error}"))
        })?;
    let written = child
        .stdin
        .take()
        .expect("clipboard command has piped stdin")
        .write_all(text.as_bytes());
    let status = child.wait()?;
    if status.success() {
        written
    } else {
        Err(io::Error::other(format!("{program} exited with {status}")))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn copy_text(_: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "System clipboard is unavailable on this platform",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn copy_into(source: &Path, destination_directory: &Path) -> io::Result<PathBuf> {
    use rustix::fs::{CWD, Mode, OFlags, openat};
    use std::fs::{self, File, Permissions};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let name = source.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "The source has no file name")
    })?;
    let source_metadata = fs::symlink_metadata(source)?;
    let directory = destination_directory.canonicalize()?;
    let destination = openat(
        CWD,
        &directory,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;

    let mut ancestors = Vec::new();
    if source_metadata.is_dir() {
        for parent in directory.ancestors() {
            let metadata = fs::metadata(parent)?;
            ancestors.push((metadata.dev(), metadata.ino()));
        }
        if ancestors.contains(&(source_metadata.dev(), source_metadata.ino())) {
            return Err(recursive_copy_error());
        }
    }

    let output = directory.join(name);
    match fs::symlink_metadata(&output) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // Stage beside the destination so a no-replace rename can publish the complete copy.
    let staging = tempfile::Builder::new()
        .prefix(".mpr2mpt-copy-")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(&directory)?;
    let staging_file = File::open(staging.path())?;
    let staging_metadata = staging_file.metadata()?;
    ancestors.push((staging_metadata.dev(), staging_metadata.ino()));
    let mut directory_permissions = Vec::new();
    copy_entry(
        CWD,
        source,
        &staging.path().join(name),
        &mut ancestors,
        &mut directory_permissions,
    )?;

    // A cross-directory rename can require write access to the directory being
    // moved. Keep its mode private and writable until it has been published.
    let published_directory = match directory_permissions.pop() {
        Some((path, mode)) => Some((File::open(path)?, Permissions::from_mode(mode))),
        None => None,
    };
    // Restore owner access from the top down if publishing fails so TempDir can
    // delete the staged tree.
    let published = directory_permissions
        .iter()
        .try_for_each(|(path, mode)| fs::set_permissions(path, Permissions::from_mode(*mode)))
        .and_then(|()| {
            publish_staged(
                &staging_file,
                &destination,
                name,
                published_directory.is_some(),
            )
        });
    if let Err(error) = published {
        for (path, _) in directory_permissions.iter().rev() {
            let _ = fs::set_permissions(path, Permissions::from_mode(0o700));
        }
        return Err(error);
    }
    if let Some((directory, permissions)) = published_directory {
        directory.set_permissions(permissions).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Copy saved, but directory permissions could not be restored: {error}"),
            )
        })?;
    }
    Ok(destination_directory.join(name))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn publish_staged(
    staging: &std::fs::File,
    destination: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    is_directory: bool,
) -> io::Result<()> {
    use rustix::fs::{AtFlags, RenameFlags, linkat, renameat, renameat_with, statat};
    use rustix::io::Errno;

    match renameat_with(staging, name, destination, name, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(Errno::NOSYS | Errno::NOTSUP | Errno::OPNOTSUPP | Errno::INVAL) => {
            if !is_directory {
                match linkat(staging, name, destination, name, AtFlags::empty()) {
                    Ok(()) => return Ok(()),
                    Err(Errno::PERM | Errno::NOTSUP | Errno::OPNOTSUPP | Errno::NOSYS) => {}
                    Err(link_err) => return Err(link_err.into()),
                }
            }
            // Fallback for filesystems that support neither RENAME_NOREPLACE nor linkat.
            // Note: statat -> renameat is not atomic, so a concurrent creator could still race.
            match statat(destination, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => Err(io::ErrorKind::AlreadyExists.into()),
                Err(Errno::NOENT) => renameat(staging, name, destination, name).map_err(Into::into),
                Err(stat_err) => Err(stat_err.into()),
            }
        }
        Err(errno) => Err(errno.into()),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn copy_entry(
    parent: impl std::os::fd::AsFd,
    source: &Path,
    target: &Path,
    ancestors: &mut Vec<(u64, u64)>,
    directory_permissions: &mut Vec<(PathBuf, u32)>,
) -> io::Result<()> {
    use rustix::fs::{
        AtFlags, CWD, Dir, FileType, Mode, OFlags, openat, readlinkat, statat, symlinkat,
    };
    use std::ffi::OsStr;
    use std::fs::{DirBuilder, File, Permissions};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let stat = statat(&parent, source, AtFlags::SYMLINK_NOFOLLOW)?;
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::RegularFile => {
            let mut source = File::from(openat(&parent, source, flags, Mode::empty())?);
            let metadata = source.metadata()?;
            if !metadata.is_file() {
                return Err(io::Error::other("The source changed during copy"));
            }
            let mut target = File::create_new(target)?;
            io::copy(&mut source, &mut target)?;
            target.set_permissions(Permissions::from_mode(metadata.mode() & 0o777))
        }
        FileType::Directory => {
            let directory = File::from(openat(
                &parent,
                source,
                flags | OFlags::DIRECTORY,
                Mode::empty(),
            )?);
            let metadata = directory.metadata()?;
            let identity = (metadata.dev(), metadata.ino());
            if ancestors.contains(&identity) {
                return Err(recursive_copy_error());
            }
            DirBuilder::new().mode(0o700).create(target)?;
            ancestors.push(identity);
            for entry in Dir::read_from(&directory)? {
                let entry = entry?;
                let name = entry.file_name().to_bytes();
                if name != b"." && name != b".." {
                    let name = Path::new(OsStr::from_bytes(name));
                    copy_entry(
                        &directory,
                        name,
                        &target.join(name),
                        ancestors,
                        directory_permissions,
                    )?;
                }
            }
            ancestors.pop();
            directory_permissions.push((target.to_path_buf(), metadata.mode() & 0o777));
            Ok(())
        }
        FileType::Symlink => {
            let link = readlinkat(&parent, source, Vec::new())?;
            symlinkat(link.as_c_str(), CWD, target)?;
            Ok(())
        }
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Only files, directories, and symbolic links can be copied",
        )),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn recursive_copy_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "A directory cannot be copied into itself or one of its descendants",
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn copy_into(_source: &Path, _destination_directory: &Path) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "File copy is unavailable on this platform",
    ))
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn copies_file_and_refuses_an_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input.mpr");
        let destination = temp.path().join("output");
        fs::write(&source, b"first").unwrap();
        fs::create_dir(&destination).unwrap();

        let output = copy_into(&source, &destination).unwrap();
        assert_eq!(fs::read(output).unwrap(), b"first");
        fs::write(&source, b"second").unwrap();
        let error = copy_into(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(destination.join("input.mpr")).unwrap(), b"first");
    }

    #[test]
    fn copies_a_directory_and_rejects_an_empty_directory_collision() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input");
        let destination = temp.path().join("output");
        let existing = destination.join("input");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/input.mps"), b"settings").unwrap();
        fs::create_dir_all(&existing).unwrap();

        let error = copy_into(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_dir(&existing).unwrap().count(), 0);
        fs::remove_dir(&existing).unwrap();

        let output = copy_into(&source, &destination).unwrap();
        assert_eq!(
            fs::read(output.join("nested/input.mps")).unwrap(),
            b"settings"
        );
    }

    #[test]
    fn rejects_copying_a_directory_into_itself_or_a_descendant() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input");
        let child = source.join("nested");
        fs::create_dir_all(&child).unwrap();

        for destination in [&source, &child] {
            let error = copy_into(&source, destination).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(!destination.join("input").exists());
        }
        assert_eq!(fs::read_dir(&child).unwrap().count(), 0);
    }

    #[test]
    fn preserves_directory_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input");
        let destination = temp.path().join("output");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/input.mpr"), b"data").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(source.join("nested"), fs::Permissions::from_mode(0o550)).unwrap();

        let result = copy_into(&source, &destination);
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(source.join("nested"), fs::Permissions::from_mode(0o700)).unwrap();

        let output = result.unwrap();
        let top_mode = fs::metadata(&output).unwrap().permissions().mode() & 0o777;
        let nested_mode = fs::metadata(output.join("nested"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(output.join("nested"), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(top_mode, 0o500);
        assert_eq!(nested_mode, 0o550);
    }

    #[test]
    fn copies_a_deep_directory_tree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input");
        let destination = temp.path().join("output");
        let deep_path: PathBuf = (0..48).map(|_| "nested").collect();
        fs::create_dir_all(source.join(&deep_path)).unwrap();
        fs::write(source.join(&deep_path).join("input.mpr"), b"data").unwrap();
        fs::create_dir(&destination).unwrap();

        let output = copy_into(&source, &destination).unwrap();
        assert_eq!(
            fs::read(output.join(deep_path).join("input.mpr")).unwrap(),
            b"data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_symlinks_and_refuses_to_replace_a_dangling_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input");
        let destination = temp.path().join("output");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        symlink("missing.mpt", source.join("input.mpt")).unwrap();
        symlink("missing-directory", destination.join("input")).unwrap();

        let error = copy_into(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_link(destination.join("input")).unwrap(),
            Path::new("missing-directory")
        );
        fs::remove_file(destination.join("input")).unwrap();

        let output = copy_into(&source, &destination).unwrap();
        assert_eq!(
            fs::read_link(output.join("input.mpt")).unwrap(),
            Path::new("missing.mpt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_destination_symlink_to_a_descendant() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input");
        let child = source.join("nested");
        let alias = temp.path().join("alias");
        fs::create_dir_all(&child).unwrap();
        symlink(&child, &alias).unwrap();

        let error = copy_into(&source, &alias).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_dir(&child).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn does_not_publish_an_incomplete_directory_copy() {
        use std::os::unix::net::UnixListener;

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input");
        let destination = temp.path().join("output");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("regular.mpr"), b"data").unwrap();
        let _listener = UnixListener::bind(source.join("socket")).unwrap();

        let error = copy_into(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    }
}
