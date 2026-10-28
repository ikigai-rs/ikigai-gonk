//! Parsed authority files, shared by layout clones rather than by authenticated sessions.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, RwLock};

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    mode: u32,
    owner: (u32, u32),
}

#[cfg(unix)]
impl Stamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            mode: metadata.mode(),
            owner: (metadata.uid(), metadata.gid()),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Version {
    #[cfg(unix)]
    Metadata(Stamp),
    Digest([u8; 32]),
}

impl Version {
    fn digest(text: &str) -> Self {
        use sha2::{Digest, Sha256};
        Self::Digest(Sha256::digest(text.as_bytes()).into())
    }
}

/// A parsed file, keyed on Unix device/inode, length, nanosecond mtime AND ctime,
/// mode and ownership. Inode detects atomic replacement; nanoseconds detect equal-length
/// edits within a second; ctime also detects writes that restore mtime and permission changes.
/// When both timestamps have zero fractional seconds, or on platforms without Unix
/// metadata, hash the contents instead of trusting potentially coarse timestamps.
/// Every lookup opens the file first: missing or unreadable files never reuse a cached
/// parse. Strict lookups report absence as an error; optional enrollment lookup reports
/// absence so HTTP can end unenrolled sessions. A miss checks the version after reading.
/// Only parsed configuration is shared: credential/grant lookup and clamping remain per call.
#[derive(Debug)]
pub(crate) struct FileCache<T> {
    entry: RwLock<Option<(Version, Arc<T>)>>,
}

impl<T> Default for FileCache<T> {
    fn default() -> Self {
        Self {
            entry: RwLock::new(None),
        }
    }
}

impl<T> FileCache<T> {
    pub(crate) fn get(
        &self,
        path: &Path,
        parse: impl Fn(&str) -> Result<T, String>,
    ) -> Result<Arc<T>, String> {
        self.get_optional(path, parse)?
            .ok_or_else(|| format!("no authority file at {}", path.display()))
    }

    /// Distinguish an absent credential file from a malformed one so HTTP can permanently
    /// end unenrolled credentials' sessions, as it does for an empty credential map.
    pub(crate) fn get_optional(
        &self,
        path: &Path,
        parse: impl Fn(&str) -> Result<T, String>,
    ) -> Result<Option<Arc<T>>, String> {
        let error = |e: std::io::Error| format!("reading {}: {e}", path.display());
        for _ in 0..3 {
            let mut file = match File::open(path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(error(e)),
            };
            let mut text = String::new();
            #[cfg(unix)]
            let stamp = Stamp::of(&file.metadata().map_err(error)?);
            #[cfg(unix)]
            let version = if stamp.modified.1 == 0 && stamp.changed.1 == 0 {
                file.read_to_string(&mut text).map_err(error)?;
                Version::digest(&text)
            } else {
                Version::Metadata(stamp.clone())
            };
            #[cfg(not(unix))]
            let version = {
                file.read_to_string(&mut text).map_err(error)?;
                if file.metadata().map_err(error)?.len() != text.len() as u64 {
                    continue;
                }
                Version::digest(&text)
            };
            {
                let entry = self
                    .entry
                    .read()
                    .map_err(|_| "authority file cache poisoned")?;
                if let Some((cached, parsed)) = entry.as_ref() {
                    if *cached == version {
                        return Ok(Some(Arc::clone(parsed)));
                    }
                }
            }
            let mut entry = self
                .entry
                .write()
                .map_err(|_| "authority file cache poisoned")?;
            // Another request may have refreshed this version while we waited.
            if let Some((cached, parsed)) = entry.as_ref() {
                if *cached == version {
                    return Ok(Some(Arc::clone(parsed)));
                }
            }
            #[cfg(unix)]
            {
                if matches!(version, Version::Metadata(_)) {
                    file.read_to_string(&mut text).map_err(error)?;
                }
                if stamp != Stamp::of(&file.metadata().map_err(error)?)
                    || stamp != Stamp::of(&std::fs::metadata(path).map_err(error)?)
                {
                    continue;
                }
            }
            let parsed = Arc::new(parse(&text).map_err(|e| format!("{}: {e}", path.display()))?);
            *entry = Some((version, Arc::clone(&parsed)));
            return Ok(Some(parsed));
        }
        Err(format!(
            "{} changed repeatedly while reading",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_file_reuses_the_parse_and_errors_never_fall_back() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.json");
        std::fs::write(&path, "[]").unwrap();
        let cache = FileCache::default();
        let first = cache
            .get(&path, |s| {
                serde_json::from_str::<serde_json::Value>(s).map_err(|e| e.to_string())
            })
            .unwrap();
        let second = cache
            .get(&path, |_| panic!("unchanged file must not be parsed"))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        std::fs::write(&path, "malformed").unwrap();
        assert!(cache.get(&path, |_| Err("invalid JSON".into())).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(cache
            .get(&path, |_| panic!("missing file must not be parsed"))
            .is_err());
    }
}
