use std::env;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::Command;

pub(super) struct Target {
    pub(super) path: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl Target {
    pub(super) fn new(path: PathBuf) -> io::Result<Self> {
        #[cfg(unix)]
        let identity = identity(&path)?;
        #[cfg(not(unix))]
        fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            #[cfg(unix)]
            identity,
        })
    }

    pub(super) fn move_to_trash(&self) -> io::Result<()> {
        #[cfg(unix)]
        if identity(&self.path).ok() != Some(self.identity) {
            return Err(io::Error::other(
                "The item changed. Select it again before moving it to Trash.",
            ));
        }
        move_to_trash(&self.path)
    }
}

#[cfg(unix)]
fn identity(path: &Path) -> io::Result<(u64, u64)> {
    let metadata = fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

pub(super) fn home() -> io::Result<PathBuf> {
    env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Cannot locate the home directory"))
}

pub(super) fn directory() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    return Ok(home()?.join(".Trash"));
    #[cfg(target_os = "linux")]
    {
        let data_home = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        return Ok(match data_home {
            Some(parent) => parent,
            None => home()?.join(".local/share"),
        }
        .join("Trash/files"));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(unsupported())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn move_to_trash(path: &Path) -> io::Result<()> {
    debug_assert!(path.is_absolute());
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("/usr/bin/trash");
        command.arg("-s");
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("/usr/bin/gio");
        command.args(["trash", "--"]);
        command
    };
    let result = command.arg(path).output().map_err(|error| {
        io::Error::new(error.kind(), format!("Cannot start system Trash: {error}"))
    })?;
    if result.status.success() {
        return Ok(());
    }
    let error = String::from_utf8_lossy(&result.stderr);
    Err(io::Error::other(if error.trim().is_empty() {
        format!("System Trash exited with {}", result.status)
    } else {
        error.trim().to_owned()
    }))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn move_to_trash(_: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "System Trash is unavailable on this platform",
    )
}
