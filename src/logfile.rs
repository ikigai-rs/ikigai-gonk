//! The file gonk's stderr is written to: owner-only, and rotated daily (ledger
//! [#3159](http://localhost:1062/l/default/item/3159)).
//!
//! Under `launchd` the access log ([`crate::access`]) goes wherever the agent's
//! `StandardErrorPath` names, and launchd creates that file under its own umask: `0644`. Every
//! access line names its caller's principal (a passkey, a client certificate's fingerprint), so
//! a world-readable log tells every local account who called gonk and when. Ledger #2801 made
//! the store owner-only; this does the same for the log.
//!
//! - **The mode.** At start, [`narrow_and_rotate`] sets every one of stdout and stderr that is
//!   a regular file to `0600`, whatever mode it had — a file launchd created `0644` before this
//!   change included. A pipe or a terminal is left alone.
//! - **Rotation.** When stderr is a regular file, gonk rotates it once a day (UTC): the file
//!   becomes `<file>.1`, `<file>.1` becomes `<file>.2`, and so on, and [`KEEP`] (14) rotated
//!   files are kept — `<file>.14` is deleted on the next rotation. A fresh `<file>` is created
//!   `0600` and becomes this process's stderr (and its stdout, when stdout was the same file).
//!   A start on a later day than the file was last written rotates first, so a restart does not
//!   append to yesterday's file. Access lines are kept for 14 days after the day they were
//!   written, plus the current one.
//! - **The directory is the operator's.** gonk does not change the mode of the directory it
//!   is logged into (it could be `/tmp` or `$HOME`); the runbook creates
//!   `~/Library/Logs/ikigai-gonk` with `mkdir -m 700`, and gonk warns at start when the
//!   directory is readable by anyone else.

use std::path::{Path, PathBuf};

/// How many rotated files are kept beside the live one: fourteen days of access lines.
pub const KEEP: usize = 14;

/// `<path>.<n>`.
fn numbered(path: &Path, n: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Shift `path`'s rotated files up by one (`<path>.k` → `<path>.k+1`, the oldest beyond `keep`
/// deleted) and move `path` to `<path>.1`. A file that is missing is skipped. The caller opens
/// a new `path`.
///
/// # Errors
///
/// When a rename fails for any reason other than the file not being there.
pub fn shift(path: &Path, keep: usize) -> std::io::Result<()> {
    let ignore_missing = |r: std::io::Result<()>| match r {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    };
    if keep == 0 {
        return ignore_missing(std::fs::remove_file(path));
    }
    ignore_missing(std::fs::remove_file(numbered(path, keep)))?;
    for n in (1..keep).rev() {
        ignore_missing(std::fs::rename(numbered(path, n), numbered(path, n + 1)))?;
    }
    ignore_missing(std::fs::rename(path, numbered(path, 1)))
}

/// Create (or open) `path` for appending, owner-only from the moment it exists, and narrow it
/// to `0600` if it was there already.
///
/// # Errors
///
/// When the file cannot be opened or its mode cannot be set.
pub fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// The day number (days since the Unix epoch, UTC) of `time`.
fn day_of(time: std::time::SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400)
}

/// Narrow stdout and stderr to `0600` where they are regular files, and, when stderr is one
/// whose path can be found, rotate it now if it was last written on an earlier day and start a
/// thread that rotates it at each UTC midnight. Called first thing by `serve`. What it could not
/// do is printed on stderr and never stops the server: a log it cannot narrow is the old
/// behavior, not a reason to refuse to serve.
#[cfg(unix)]
pub fn narrow_and_rotate() {
    use rustix::fs::{FileType, Mode};

    let regular = |fd: rustix::fd::BorrowedFd<'_>| {
        rustix::fs::fstat(fd)
            .ok()
            .filter(|st| FileType::from_raw_mode(st.st_mode) == FileType::RegularFile)
    };
    let out = regular(rustix::stdio::stdout());
    let err = regular(rustix::stdio::stderr());
    for (fd, stat, name) in [
        (rustix::stdio::stdout(), &out, "stdout"),
        (rustix::stdio::stderr(), &err, "stderr"),
    ] {
        if stat.is_some() {
            if let Err(e) = rustix::fs::fchmod(fd, Mode::from_raw_mode(0o600)) {
                eprintln!("ikigai-gonk: cannot make the {name} log owner-only (0600): {e}");
            }
        }
    }
    let Some(err) = err else { return };
    let same_file = out.is_some_and(|o| o.st_dev == err.st_dev && o.st_ino == err.st_ino);
    let Some(path) = stderr_path() else {
        eprintln!("ikigai-gonk: cannot find the path of the stderr log; it will not be rotated");
        return;
    };
    warn_if_shared_directory(&path);

    let rotation = Rotation { path, same_file };
    let written = std::fs::metadata(&rotation.path)
        .and_then(|m| m.modified())
        .ok();
    let today = day_of(std::time::SystemTime::now());
    if written.is_some_and(|w| day_of(w) < today) {
        rotation.rotate();
    }
    let spawned = std::thread::Builder::new()
        .name("gonk-log-rotate".into())
        .spawn(move || {
            let mut day = today;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                let now = day_of(std::time::SystemTime::now());
                if now != day {
                    day = now;
                    rotation.rotate();
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("ikigai-gonk: cannot start the log rotation thread: {e}");
    }
}

/// Not a Unix: nothing to narrow.
#[cfg(not(unix))]
pub fn narrow_and_rotate() {}

#[cfg(unix)]
struct Rotation {
    path: PathBuf,
    same_file: bool,
}

#[cfg(unix)]
impl Rotation {
    /// Shift the files, open a fresh `0600` file at the path and make it stderr (and stdout,
    /// when that was the same file). A line being written while this runs lands whole in one
    /// file or the other: `dup2` swaps the descriptor atomically.
    fn rotate(&self) {
        let result = shift(&self.path, KEEP)
            .and_then(|()| open_private(&self.path))
            .and_then(|file| {
                rustix::stdio::dup2_stderr(&file)?;
                if self.same_file {
                    rustix::stdio::dup2_stdout(&file)?;
                }
                Ok(())
            });
        if let Err(e) = result {
            eprintln!(
                "ikigai-gonk: cannot rotate the log {}: {e}",
                self.path.display()
            );
        }
    }
}

/// The path stderr is open on.
#[cfg(unix)]
fn stderr_path() -> Option<PathBuf> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::unix::ffi::OsStringExt;
        let path = rustix::fs::getpath(rustix::stdio::stderr()).ok()?;
        Some(PathBuf::from(std::ffi::OsString::from_vec(
            path.into_bytes(),
        )))
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        std::fs::read_link("/proc/self/fd/2").ok()
    }
}

/// Say so when the log's directory lets anyone else in.
#[cfg(unix)]
fn warn_if_shared_directory(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Some(dir) = path.parent() else { return };
    if let Ok(meta) = std::fs::metadata(dir) {
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            eprintln!(
                "ikigai-gonk: the log {} is owner-only, but its directory {} is {mode:03o}: the \
                 rotated files' names are visible to other accounts. Log into an owner-only \
                 directory (`mkdir -m 700 ~/Library/Logs/ikigai-gonk`).",
                path.display(),
                dir.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shift_moves_each_file_up_one_and_drops_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("gonk.log");
        std::fs::write(&log, "today").unwrap();
        std::fs::write(numbered(&log, 1), "yesterday").unwrap();
        std::fs::write(numbered(&log, 3), "oldest").unwrap();

        shift(&log, 3).unwrap();

        assert!(!log.exists());
        assert_eq!(std::fs::read_to_string(numbered(&log, 1)).unwrap(), "today");
        assert_eq!(
            std::fs::read_to_string(numbered(&log, 2)).unwrap(),
            "yesterday"
        );
        assert!(!numbered(&log, 3).exists(), "the one beyond `keep` is gone");
        assert!(!numbered(&log, 4).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_reopened_log_is_owner_only_even_when_it_was_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("gonk.log");
        std::fs::write(&log, "old\n").unwrap();
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(open_private(&log).unwrap());
        let mode = std::fs::metadata(&log).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "old\n",
            "appended, not cut"
        );
    }
}
