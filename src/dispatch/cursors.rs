//! Private bounded snapshots. Only hashes reach disk; no ledger triples or bearer tokens.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::PathBuf;

use ikigai_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) type Snapshot = BTreeSet<String>;
const MAX_SCOPES: usize = 128;
pub(super) const MAX_RECORDS: usize = 65_536;
const MAX_HASHES: usize = 262_144;
const MAX_FILE: u64 = 32 * 1024 * 1024;
const RETAIN_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const PENDING_MS: u64 = 15 * 60 * 1000;
const MAX_PENDING: usize = 4;

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Entry {
    touched: u64,
    seen: Option<Snapshot>,
    #[serde(skip)]
    pending: Vec<Pending>,
}
#[derive(Clone)]
struct Pending {
    token: String,
    at: u64,
    seen: Snapshot,
}

pub(super) struct Cursors {
    path: Option<PathBuf>,
    entries: BTreeMap<String, Entry>,
}
impl Cursors {
    pub(super) fn open(path: Option<PathBuf>) -> Result<Self> {
        let entries: BTreeMap<String, Entry> = match path.as_ref() {
            Some(path) if path.try_exists().map_err(io_error)? => {
                let meta = std::fs::symlink_metadata(path).map_err(io_error)?;
                if !meta.is_file() || meta.len() > MAX_FILE {
                    return Err(Error::Endpoint(
                        "dispatch: invalid or oversized cursor file".into(),
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o077 != 0 {
                        return Err(Error::Endpoint(
                            "dispatch: cursor file must be owner-only".into(),
                        ));
                    }
                }
                let mut bytes = Vec::new();
                std::fs::File::open(path)
                    .map_err(io_error)?
                    .take(MAX_FILE + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io_error)?;
                if bytes.len() as u64 > MAX_FILE {
                    return Err(over_budget());
                }
                serde_json::from_slice(&bytes)
                    .map_err(|e| Error::Endpoint(format!("dispatch: invalid cursor file: {e}")))?
            }
            _ => BTreeMap::new(),
        };
        let valid_hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if entries.len() > MAX_SCOPES
            || entries.values().map(hashes).sum::<usize>() > MAX_HASHES
            || entries.iter().any(|(key, entry)| {
                !valid_hash(key)
                    || entry.seen.as_ref().is_some_and(|seen| {
                        seen.len() > MAX_RECORDS || seen.iter().any(|hash| !valid_hash(hash))
                    })
            })
        {
            return Err(Error::Endpoint(
                "dispatch: invalid cursor bounds or hashes".into(),
            ));
        }
        Ok(Self { path, entries })
    }

    pub(super) fn baseline(&mut self, key: &str, now: u64) -> Option<Snapshot> {
        self.prune(now);
        let entry = self.entries.get_mut(key)?;
        entry.touched = now;
        entry.seen.clone()
    }

    pub(super) fn stage(
        &mut self,
        key: &str,
        baseline: &Option<Snapshot>,
        seen: Snapshot,
        now: u64,
    ) -> Result<String> {
        self.prune(now);
        if self.entries.get(key).and_then(|e| e.seen.as_ref()) != baseline.as_ref() {
            return Err(Error::Conflict(
                "dispatch: cursor changed during the read; look again".into(),
            ));
        }
        if seen.len() > MAX_RECORDS {
            return Err(over_budget());
        }
        let mut random = [0; 32];
        getrandom::fill(&mut random)
            .map_err(|e| Error::Endpoint(format!("dispatch: cursor randomness: {e}")))?;
        let token = digest(&random);
        let entry = self.entries.entry(key.into()).or_default();
        entry.touched = now;
        while entry.pending.len() >= MAX_PENDING {
            entry.pending.remove(0);
        }
        entry.pending.push(Pending {
            token: token.clone(),
            at: now,
            seen,
        });
        self.bound(key)?;
        Ok(token)
    }

    pub(super) fn acknowledge(&mut self, key: &str, token: &str, now: u64) -> Result<()> {
        self.prune(now);
        let pending = self
            .entries
            .get(key)
            .and_then(|e| e.pending.iter().find(|p| p.token == token))
            .cloned()
            .ok_or_else(|| {
                Error::Conflict(
                    "dispatch: unknown, expired or superseded cursor; look again".into(),
                )
            })?;
        // Persist before replacing the in-memory baseline. A failed write never says read.
        let mut next = self.entries.clone();
        let entry = next.get_mut(key).expect("the pending cursor's entry");
        entry.seen = Some(pending.seen);
        entry.touched = now;
        entry.pending.clear();
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec(&next).map_err(|e| Error::Endpoint(e.to_string()))?;
            if bytes.len() as u64 > MAX_FILE {
                return Err(over_budget());
            }
            let temp = path.with_extension(format!("{}.tmp", pending.token));
            let result = (|| {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&temp)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                std::fs::rename(&temp, path)?;
                Ok::<_, std::io::Error>(())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            result.map_err(io_error)?;
        }
        self.entries = next;
        Ok(())
    }

    fn prune(&mut self, now: u64) {
        self.entries.retain(|_, entry| {
            entry
                .pending
                .retain(|p| now.saturating_sub(p.at) < PENDING_MS);
            now.saturating_sub(entry.touched) < RETAIN_MS
        });
    }
    fn bound(&mut self, keep: &str) -> Result<()> {
        while self.entries.len() > MAX_SCOPES
            || self.entries.values().map(hashes).sum::<usize>() > MAX_HASHES
        {
            let oldest = self
                .entries
                .iter()
                .filter(|(k, _)| k.as_str() != keep)
                .min_by_key(|(_, e)| e.touched)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(key) => {
                    self.entries.remove(&key);
                }
                None => {
                    // At most four pending snapshots plus one baseline. Drop oldest pending
                    // previews first; never silently discard the acknowledged baseline.
                    let entry = self.entries.get_mut(keep).expect("the staged entry");
                    if entry.pending.len() > 1 {
                        entry.pending.remove(0);
                    } else {
                        return Err(over_budget());
                    }
                }
            }
        }
        Ok(())
    }
}
fn hashes(entry: &Entry) -> usize {
    entry.seen.as_ref().map_or(0, Snapshot::len)
        + entry.pending.iter().map(|p| p.seen.len()).sum::<usize>()
}
fn io_error(e: std::io::Error) -> Error {
    Error::Endpoint(format!(
        "dispatch: cannot read or save private cursors: {e}"
    ))
}
pub(super) fn over_budget() -> Error {
    Error::OverBudget {
        detail: "dispatch: summary exceeds its bound; no cursor advanced; use an individual item"
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(name: &str) -> Snapshot {
        [digest(name.as_bytes())].into_iter().collect()
    }
    #[test]
    fn acknowledged_hashes_survive_restart_but_preview_tokens_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cursors.json");
        let key = digest(b"reader and scope");
        let seen = snapshot("a comment with private text");
        let mut cursors = Cursors::open(Some(path.clone())).unwrap();
        let token = cursors.stage(&key, &None, seen.clone(), 1000).unwrap();
        assert!(!path.exists(), "a preview is not a durable read receipt");
        cursors.acknowledge(&key, &token, 1001).unwrap();
        let pending = cursors
            .stage(&key, &Some(seen.clone()), snapshot("new"), 1002)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("private text"));
        assert!(!String::from_utf8_lossy(&bytes).contains(&token));
        let mut reopened = Cursors::open(Some(path.clone())).unwrap();
        assert_eq!(reopened.baseline(&key, 1003), Some(seen));
        assert!(matches!(
            reopened.acknowledge(&key, &pending, 1004),
            Err(Error::Conflict(_))
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn expiry_and_eviction_produce_full_rereads_and_tokens_are_bounded() {
        let mut cursors = Cursors::open(None).unwrap();
        let key = digest(b"first");
        let token = cursors.stage(&key, &None, snapshot("old"), 1).unwrap();
        cursors.acknowledge(&key, &token, 2).unwrap();
        assert_eq!(cursors.baseline(&key, RETAIN_MS + 2), None);
        let token = cursors
            .stage(&key, &None, snapshot("old"), RETAIN_MS + 3)
            .unwrap();
        assert!(matches!(
            cursors.acknowledge(&key, &token, RETAIN_MS + 3 + PENDING_MS),
            Err(Error::Conflict(_))
        ));
        let token = cursors
            .stage(&key, &None, snapshot("old"), RETAIN_MS + PENDING_MS + 4)
            .unwrap();
        for n in 0..MAX_PENDING {
            cursors
                .stage(
                    &key,
                    &None,
                    snapshot(&n.to_string()),
                    RETAIN_MS + PENDING_MS + 5,
                )
                .unwrap();
        }
        assert!(matches!(
            cursors.acknowledge(&key, &token, RETAIN_MS + PENDING_MS + 6),
            Err(Error::Conflict(_))
        ));
        for n in 0..MAX_SCOPES {
            cursors
                .stage(
                    &digest(n.to_string().as_bytes()),
                    &None,
                    snapshot("one"),
                    RETAIN_MS + PENDING_MS + 7,
                )
                .unwrap();
        }
        assert_eq!(cursors.entries.len(), MAX_SCOPES);
        assert_eq!(cursors.baseline(&key, RETAIN_MS + PENDING_MS + 8), None);
    }
    #[test]
    fn a_failed_save_does_not_acknowledge_or_consume_the_preview() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/cursors.json");
        let mut cursors = Cursors::open(Some(path)).unwrap();
        let key = digest(b"scope");
        let token = cursors.stage(&key, &None, snapshot("one"), 1).unwrap();
        assert!(cursors.acknowledge(&key, &token, 2).is_err());
        assert_eq!(cursors.baseline(&key, 3), None);
        std::fs::create_dir(dir.path().join("missing")).unwrap();
        cursors.acknowledge(&key, &token, 4).unwrap();
        assert_eq!(cursors.baseline(&key, 5), Some(snapshot("one")));
    }
    #[test]
    fn an_oversized_snapshot_is_refused_without_a_receipt() {
        let mut cursors = Cursors::open(None).unwrap();
        let key = digest(b"scope");
        let seen = (0..=MAX_RECORDS).map(|n| format!("{n:064x}")).collect();
        assert!(matches!(
            cursors.stage(&key, &None, seen, 1),
            Err(Error::OverBudget { .. })
        ));
        assert_eq!(cursors.baseline(&key, 2), None);
        assert!(cursors.entries.is_empty());
    }
    #[test]
    fn invalid_or_public_cursor_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cursors.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(Cursors::open(Some(path.clone())).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&path, "{}").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(Cursors::open(Some(path)).is_err());
        }
    }
}
