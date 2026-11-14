//! Bounded pieces of ONE snapshot, with a manifest published last.
//!
//! These are full snapshots split incrementally during serialization, not change logs.
//! Neither a graph nor the complete N-Quads document has to fit a store answer/load limit.
//! Every piece ends between quads, so a graph can span arbitrarily many pieces. Blank-node
//! labels belong to the whole snapshot and are deliberately NOT renamed per piece.

use super::*;
use serde::{Deserialize, Serialize};

/// Take a full snapshot in bounded per-graph pieces.
pub const BACKUP_CHUNKS: &str = "urn:iki:gonk:backup:chunks";
/// Restore a named chunked snapshot from the backup directory into a new store.
pub const RESTORE_CHUNKS: &str = "urn:iki:gonk:restore:chunks";
const FORMAT: &str = "gonk-chunks/1";
const CHUNK_BYTES: u64 = 64 << 20;
const CHUNK_ROWS: u64 = 100_000;
const MANIFEST_BYTES: u64 = 16 << 20;
const PREFIX: &str = "gonk-chunks-";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Piece {
    bytes: u64,
    sha256: String,
    graphs: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    taken_at_ms: u64,
    // Names are implicit (00000000.nq.gz, ...), never supplied by the archive.
    pieces: Vec<Piece>,
    graphs: BTreeMap<String, u64>,
    scripts_sha256: Option<String>,
}

pub(super) fn bind(space: EndpointSpace, backups: Backups) -> EndpointSpace {
    space
        .bind(
            Exact::new(BACKUP_CHUNKS),
            Take {
                backups: backups.clone(),
            },
        )
        .bind(Exact::new(RESTORE_CHUNKS), RestoreChunks { backups })
}

pub(super) async fn backup(backups: &Backups, inv: &Invocation<'_>) -> Result<Representation> {
    let now = inv
        .now()
        .ok_or_else(|| Error::Endpoint("no backup clock".into()))?
        .as_millis();
    let dataset = backups.dataset.clone();
    let bounds = Bounds::for_caller(&dataset, inv.capability);
    let settings = backups.settings.clone();
    off_thread(move || take(&dataset, &settings, bounds, now))
        .await
        .map(plain)
}

pub(super) struct Latest {
    pub name: String,
    pub taken_at_ms: u64,
    pub text: String,
    pub json: serde_json::Value,
}

pub(super) fn newest(dir: &Path) -> Option<Latest> {
    snapshots(dir).into_iter().rev().find_map(|path| {
        let bytes = read_archive_bytes(&path.join("manifest.json"), MANIFEST_BYTES).ok()?;
        let manifest: Manifest = serde_json::from_slice(&bytes).ok()?;
        if manifest.format != FORMAT { return None; }
        let quads = total(&manifest.graphs).ok()?;
        let name = path.file_name()?.to_str()?.to_string();
        let sha256 = digest(&bytes);
        let text = format!("    taken_at       {}\n    quads          {quads}\n    graphs         {}\n    pieces         {}\n    manifest_sha256 {sha256}\n", stamp_iso(manifest.taken_at_ms), manifest.graphs.len(), manifest.pieces.len());
        let json = serde_json::json!({
            "format": FORMAT, "archive": name, "taken_at": stamp_iso(manifest.taken_at_ms),
            "taken_at_ms": manifest.taken_at_ms, "quads": quads,
            "graphs": manifest.graphs, "pieces": manifest.pieces.len(), "manifest_sha256": sha256,
        });
        Some(Latest { name, taken_at_ms: manifest.taken_at_ms, text, json })
    })
}

struct Take {
    backups: Backups,
}
struct RestoreChunks {
    backups: Backups,
}

#[async_trait]
impl Endpoint for Take {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(unsupported(self.name(), inv.request.verb));
        }
        for scope in [CAP_BACKUP, ikigai_store::CAP_READ] {
            if !inv.capability.allows(scope) {
                return Err(Error::Denied(format!("a chunked backup needs {scope}")));
            }
        }
        backup(&self.backups, inv).await
    }

    fn name(&self) -> &str {
        "gonk-backup-chunks"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("Back up one snapshot in bounded N-Quads pieces")
            .summary("Writes a full snapshot into a private directory under the backup directory. Each per-graph piece is at most 64 MiB and 100,000 quads, further bounded by the caller's answer grants; the caller's time budget applies per bounded window. Publishes a manifest with SHA-256 digests and graph counts only after every piece succeeds. Retains the configured number of chunked snapshots, independently of single-file archives. Scripts are included when configured.")
            .verb(Verb::Source).verb(Verb::Meta)
            .requires(CAP_BACKUP).requires(ikigai_store::CAP_READ)
            .output("text/plain")
    }
}

#[async_trait]
impl Endpoint for RestoreChunks {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(unsupported(self.name(), inv.request.verb));
        }
        if !inv.capability.allows(CAP_RESTORE) {
            return Err(Error::Denied(format!(
                "a chunked restore needs {CAP_RESTORE}"
            )));
        }
        let name = inv.inline_str("content")?.to_string();
        if !own_name(&name) {
            return Err(invalid("invalid chunked snapshot name"));
        }
        let into = PathBuf::from(inv.inline_str("into")?);
        Restore {
            backups: self.backups.clone(),
        }
        .check_target(&into)?;
        let from = self.backups.settings.dir.join(&name);
        // Serialize reading against rotation for the entire drill. A pruned snapshot must
        // never turn into a short successful restore. The filesystem lock works across processes.
        let rotation_dir = self.backups.settings.dir.clone();
        off_thread(move || {
            let _rotation = lock_rotation(&rotation_dir)?;
            restore(&from, &into)
        })
        .await
        .map(plain)
    }

    fn name(&self) -> &str {
        "gonk-restore-chunks"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("Restore and verify a chunked snapshot into a NEW store")
            .summary("Reads a named snapshot from the backup directory one bounded piece at a time, checks its manifest, digests and per-graph counts, and verifies the restored graph counts before publishing the new directory. Blank nodes retain their identities across pieces. Refuses the live store and existing nonempty targets. Scripts remain a separate restore operation.")
            .verb(Verb::Sink).verb(Verb::Meta).requires(CAP_RESTORE)
            .input(ArgSpec::new("content").summary("the gonk-chunks directory name in the backup directory").class(XSD_STRING))
            .input(ArgSpec::new("into").summary("a NEW store directory, absent or empty").class(XSD_STRING))
            .output("text/plain")
    }
}

async fn off_thread<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("gonk-backup-chunks".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| Error::Endpoint(format!("starting chunked backup worker: {e}")))?;
    rx.await
        .map_err(|_| Error::Endpoint("chunked backup worker panicked".into()))?
}

fn total(counts: &BTreeMap<String, u64>) -> Result<u64> {
    counts.values().try_fold(0_u64, |sum, count| {
        sum.checked_add(*count)
            .ok_or_else(|| invalid("quad count overflow"))
    })
}

fn persist(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = create_private(path).map_err(|e| Error::Endpoint(e.to_string()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| Error::Endpoint(e.to_string()))
}

fn sync_dir(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| Error::Endpoint(format!("syncing {}: {e}", path.display())))
}

fn invalid(detail: &str) -> Error {
    Error::InvalidArgument {
        name: "snapshot".into(),
        detail: detail.into(),
    }
}

fn own_name(name: &str) -> bool {
    let Some(stamp) = name.strip_prefix(PREFIX) else {
        return false;
    };
    // Milliseconds disambiguate snapshots within a second. A same-millisecond collision
    // is refused at publication rather than overwriting an existing backup.
    stamp.len() == 22
        && stamp.bytes().zip(b"dddd-dd-ddTddddddZ-ddd").all(|(b, p)| {
            if *p == b'd' {
                b.is_ascii_digit()
            } else {
                b == *p
            }
        })
}

fn piece_name(index: usize) -> String {
    format!("{index:08}.nq.gz")
}

fn add_counts(to: &mut BTreeMap<String, u64>, from: &BTreeMap<String, u64>) -> Result<()> {
    for (graph, count) in from {
        let entry = to.entry(graph.clone()).or_default();
        *entry = entry
            .checked_add(*count)
            .ok_or_else(|| invalid("quad count overflow"))?;
    }
    Ok(())
}

fn take(dataset: &Dataset, settings: &Settings, bounds: Bounds, now: u64) -> Result<String> {
    create_private_dir(&settings.dir).map_err(|e| Error::Endpoint(e.to_string()))?;
    let name = format!("{PREFIX}{}-{:03}", stamp_compact(now), now % 1000);
    let into = settings.dir.join(&name);
    let staging = Staging::begin(&into)?;
    // Staging is also used by store restore; make the backup directory private before data.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::Endpoint(e.to_string()))?;
    }
    let scripts = settings
        .scripts
        .as_deref()
        .map(snapshot_scripts)
        .transpose()?;
    let mut manifest = export(dataset, staging.path(), bounds, now)?;
    if let Some(scripts) = scripts {
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(GZIP_LEVEL));
        encoder
            .write_all(&scripts.json)
            .map_err(|e| Error::Endpoint(e.to_string()))?;
        let archive = encoder
            .finish()
            .map_err(|e| Error::Endpoint(e.to_string()))?;
        check_bytes(archive.len() as u64, ARCHIVE_MAX_BYTES)?;
        manifest.scripts_sha256 = Some(digest(&archive));
        persist(&staging.path().join("scripts.json.gz"), &archive)?;
    }
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| Error::Endpoint(e.to_string()))?;
    check_bytes(json.len() as u64, MANIFEST_BYTES)?;
    persist(&staging.path().join("manifest.json"), &json)?;
    let _rotation = lock_rotation(&settings.dir)?;
    if into.exists() {
        return Err(invalid("a snapshot already exists at this timestamp"));
    }
    sync_dir(staging.path())?;
    staging.finish(&into)?;
    sync_dir(&settings.dir)?;
    let mut snapshots = snapshots(&settings.dir);
    // Keep the snapshot just published even if a clock moved backwards.
    snapshots.retain(|p| p != &into);
    let remove = snapshots.len().saturating_sub(settings.keep.max(1) - 1);
    for old in snapshots.into_iter().take(remove) {
        std::fs::remove_dir_all(&old)
            .map_err(|e| Error::Endpoint(format!("pruning {}: {e}", old.display())))?;
    }
    Ok(format!(
        "backup {name}\n  {} quads in {} graph(s), {} bounded pieces\n  manifest sha256 {}\n",
        manifest.graphs.values().sum::<u64>(),
        manifest.graphs.len(),
        manifest.pieces.len(),
        digest(&json)
    ))
}

fn snapshots(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_type().is_ok_and(|t| t.is_dir()) && own_name(&e.file_name().to_string_lossy())
        })
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

fn export(dataset: &Dataset, dir: &Path, bounds: Bounds, now: u64) -> Result<Manifest> {
    let max_bytes = bounds.bytes.min(CHUNK_BYTES);
    let max_rows = bounds.rows.min(CHUNK_ROWS);
    let mut manifest = Manifest {
        format: FORMAT.into(),
        taken_at_ms: now,
        pieces: Vec::new(),
        graphs: BTreeMap::new(),
        scripts_sha256: None,
    };
    // Group a bounded window by graph: every piece belongs to exactly one graph,
    // but a large graph may have any number of pieces. The whole window (not each
    // graph separately) is bounded, so many small graphs cannot multiply memory use.
    let mut buffers: BTreeMap<String, (Vec<u8>, u64)> = BTreeMap::new();
    let mut bytes: u64 = 0;
    let mut rows: u64 = 0;
    let mut started = Instant::now();
    // ONE iterator owns ONE snapshot for the entire export, including all graph boundaries.
    for quad in dataset.handle.iter() {
        let quad = quad.map_err(|e| Error::Endpoint(format!("reading snapshot: {e}")))?;
        let mut line = Vec::new();
        let mut writer = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut line);
        writer
            .serialize_quad(&quad)
            .map_err(|e| Error::Endpoint(e.to_string()))?;
        writer
            .finish()
            .map_err(|e| Error::Endpoint(e.to_string()))?;
        if line.len() as u64 > max_bytes || max_rows == 0 {
            return Err(invalid("a single quad exceeds the chunk's answer bound"));
        }
        if rows > 0 && (bytes + line.len() as u64 > max_bytes || rows == max_rows) {
            finish_window(dir, &mut manifest, &buffers, started, bounds.time)?;
            if started.elapsed() >= bounds.time {
                return Err(past_time(bounds.time));
            }
            buffers.clear();
            bytes = 0;
            rows = 0;
            started = Instant::now();
        }
        bytes += line.len() as u64;
        let (buffer, count) = buffers.entry(graph_key(&quad.graph_name)).or_default();
        buffer.extend_from_slice(&line);
        *count += 1;
        rows += 1;
        if rows.is_multiple_of(CLOCK_EVERY) && started.elapsed() >= bounds.time {
            return Err(past_time(bounds.time));
        }
    }
    finish_window(dir, &mut manifest, &buffers, started, bounds.time)?;
    if started.elapsed() >= bounds.time {
        return Err(past_time(bounds.time));
    }
    Ok(manifest)
}

fn finish_window(
    dir: &Path,
    manifest: &mut Manifest,
    buffers: &BTreeMap<String, (Vec<u8>, u64)>,
    started: Instant,
    budget: Duration,
) -> Result<()> {
    for (graph, (bytes, rows)) in buffers {
        if started.elapsed() >= budget {
            return Err(past_time(budget));
        }
        finish_piece(
            dir,
            manifest,
            bytes,
            &BTreeMap::from([(graph.clone(), *rows)]),
        )?;
    }
    Ok(())
}

fn finish_piece(
    dir: &Path,
    manifest: &mut Manifest,
    bytes: &[u8],
    graphs: &BTreeMap<String, u64>,
) -> Result<()> {
    let mut encoder = flate2::GzBuilder::new()
        .mtime(GZIP_MTIME)
        .operating_system(GZIP_OS)
        .write(Vec::new(), flate2::Compression::new(GZIP_LEVEL));
    encoder
        .write_all(bytes)
        .map_err(|e| Error::Endpoint(e.to_string()))?;
    let archive = encoder
        .finish()
        .map_err(|e| Error::Endpoint(e.to_string()))?;
    persist(&dir.join(piece_name(manifest.pieces.len())), &archive)?;
    add_counts(&mut manifest.graphs, graphs)?;
    manifest.pieces.push(Piece {
        bytes: bytes.len() as u64,
        sha256: digest(&archive),
        graphs: graphs.clone(),
    });
    // Bound metadata growth as well as piece size, before accumulating unlimited state.
    if manifest.pieces.len() > 100_000 {
        return Err(invalid("too many snapshot pieces"));
    }
    Ok(())
}

fn restore(from: &Path, into: &Path) -> Result<String> {
    if std::fs::symlink_metadata(from)
        .map_err(|e| Error::Endpoint(e.to_string()))?
        .file_type()
        .is_symlink()
    {
        return Err(invalid("snapshot directory must not be a symlink"));
    }
    let json = read_archive_bytes(&from.join("manifest.json"), MANIFEST_BYTES)?;
    let manifest: Manifest =
        serde_json::from_slice(&json).map_err(|e| invalid(&format!("invalid manifest: {e}")))?;
    if manifest.format != FORMAT || manifest.pieces.len() > 100_000 {
        return Err(invalid("unsupported snapshot format or too many pieces"));
    }
    let mut expected = BTreeMap::new();
    for piece in &manifest.pieces {
        if piece.bytes > CHUNK_BYTES
            || total(&piece.graphs)? > CHUNK_ROWS
            || piece.graphs.len() != 1
        {
            return Err(invalid("manifest piece exceeds the format's bounds"));
        }
        add_counts(&mut expected, &piece.graphs)?;
    }
    if expected != manifest.graphs {
        return Err(invalid("manifest graph counts disagree"));
    }
    total(&manifest.graphs)?;
    let staging = Staging::begin(into)?;
    let store =
        ikigai_store::Store::open(staging.path()).map_err(|e| Error::Endpoint(e.to_string()))?;
    let mut loader = store
        .bulk_loader()
        .with_num_threads(2)
        .with_max_memory_size_in_megabytes(128);
    for (index, piece) in manifest.pieces.iter().enumerate() {
        let archive = read_archive_bytes(&from.join(piece_name(index)), 2 * CHUNK_BYTES)?;
        if digest(&archive) != piece.sha256 {
            return Err(invalid("snapshot piece digest mismatch"));
        }
        // A gzip bomb cannot exceed one piece, regardless of untrusted manifest lengths.
        let decoder = flate2::read::MultiGzDecoder::new(archive.as_slice());
        let bytes = read_archive_input(decoder, piece.bytes)?;
        if bytes.len() as u64 != piece.bytes {
            return Err(invalid("snapshot piece length mismatch"));
        }
        // Independent parser/count path, bounded to one piece rather than the whole dataset.
        if counts_in_nquads(&bytes)? != piece.graphs {
            return Err(invalid("snapshot piece graph counts mismatch"));
        }
        let quads: std::result::Result<Vec<_>, _> = RdfParser::from_format(RdfFormat::NQuads)
            .for_reader(bytes.as_slice())
            .collect();
        // load_quads preserves labels. load_from_reader renames them independently per call,
        // which would split one blank node shared by two chunks into two distinct nodes.
        loader
            .load_quads(quads.map_err(|e| invalid(&format!("parsing piece: {e}")))?)
            .map_err(|e| Error::Endpoint(format!("loading piece: {e}")))?;
    }
    loader
        .commit()
        .map_err(|e| Error::Endpoint(format!("committing snapshot: {e}")))?;
    let mut actual = BTreeMap::new();
    for quad in store.iter() {
        let quad = quad.map_err(|e| Error::Endpoint(e.to_string()))?;
        *actual.entry(graph_key(&quad.graph_name)).or_default() += 1;
    }
    if actual != manifest.graphs {
        return Err(invalid(
            "restored graph set and per-graph counts do not match the snapshot",
        ));
    }
    if let Some(expected) = manifest.scripts_sha256 {
        let archive = read_archive_bytes(&from.join("scripts.json.gz"), ARCHIVE_MAX_BYTES)?;
        if digest(&archive) != expected {
            return Err(invalid("scripts archive digest mismatch"));
        }
    }
    drop(store);
    staging.finish(into)?;
    Ok(format!("restored {} quads in {} graph(s) into {}\n  every piece digest and graph count verified\n  graph set and per-graph counts match the snapshot exactly\n  this server still holds the LIVE store; stop gonk before swapping directories\n", actual.values().sum::<u64>(), actual.len(), into.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Dataset {
        let (store, handle) = ikigai_store::DurableStore::in_memory_shared().unwrap();
        // One blank node is an object, a subject, and a graph name in different pieces.
        handle.load_from_slice(RdfFormat::NQuads, b"<urn:a> <urn:p> _:shared .\n_:shared <urn:p> <urn:b> <urn:g> .\n<urn:c> <urn:p> <urn:d> _:shared .\n<urn:e> <urn:p> \"quoted \\\" and \\n .\" <urn:g> .\n").unwrap();
        Dataset::new(&store, handle)
    }

    fn bounds() -> Bounds {
        Bounds {
            time: Duration::from_secs(120),
            rows: 1,
            bytes: 1024,
        }
    }

    fn save_manifest(dir: &Path, manifest: &Manifest) {
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn pieces_cross_graph_and_blank_node_boundaries_and_restore_exactly() {
        let source = fixture();
        let scratch = tempfile::tempdir().unwrap();
        let manifest = export(&source, scratch.path(), bounds(), 0).unwrap();
        assert_eq!(manifest.pieces.len(), 4);
        save_manifest(scratch.path(), &manifest);
        let into = scratch.path().join("restored");
        let report = restore(scratch.path(), &into).unwrap();
        assert!(report.contains("restored 4 quads"), "{report}");
        let restored = ikigai_store::Store::open(&into).unwrap();
        let quads = |store: &ikigai_store::Store| {
            store
                .iter()
                .map(|q| q.unwrap().to_string())
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(quads(&source.handle), quads(&restored));
    }

    #[test]
    fn one_graph_larger_than_the_piece_bound_is_split_instead_of_refused() {
        let (store, handle) = ikigai_store::DurableStore::in_memory_shared().unwrap();
        let doc: String = (0..100)
            .map(|n| {
                format!(
                    "<urn:s:{n}> <urn:p> \"{}\" <urn:large> .\n",
                    "x".repeat(100)
                )
            })
            .collect();
        handle
            .load_from_slice(RdfFormat::NQuads, doc.as_bytes())
            .unwrap();
        let dataset = Dataset::new(&store, handle);
        let limits = Bounds {
            bytes: 1024,
            rows: 1000,
            ..bounds()
        };
        assert!(
            dump(&dataset, limits).is_err(),
            "the legacy path reproduces the whole-answer refusal"
        );
        let scratch = tempfile::tempdir().unwrap();
        let manifest = export(&dataset, scratch.path(), limits, 0).unwrap();
        assert!(manifest.pieces.len() > 10);
        assert_eq!(manifest.graphs["urn:large"], 100);
        assert!(manifest.pieces.iter().all(|p| p.bytes <= limits.bytes));
        save_manifest(scratch.path(), &manifest);
        restore(scratch.path(), &scratch.path().join("restored")).unwrap();
    }

    #[test]
    fn corrupt_missing_and_duplicate_pieces_never_publish_and_retry_works() {
        let source = fixture();
        let scratch = tempfile::tempdir().unwrap();
        let mut manifest = export(&source, scratch.path(), bounds(), 0).unwrap();
        save_manifest(scratch.path(), &manifest);
        let into = scratch.path().join("restored");
        let first = scratch.path().join(piece_name(0));
        let saved = std::fs::read(&first).unwrap();
        std::fs::write(&first, &saved[..saved.len() - 1]).unwrap();
        assert!(restore(scratch.path(), &into).is_err());
        assert!(!into.exists());
        std::fs::remove_file(&first).unwrap();
        assert!(restore(scratch.path(), &into).is_err());
        assert!(!into.exists());
        std::fs::write(&first, &saved).unwrap();
        // An internally consistent extra piece repeats an existing statement. Summing the
        // piece counts would accept it; counting the restored set must reject it.
        std::fs::write(
            scratch.path().join(piece_name(manifest.pieces.len())),
            &saved,
        )
        .unwrap();
        let repeated: Piece =
            serde_json::from_slice(&serde_json::to_vec(&manifest.pieces[0]).unwrap()).unwrap();
        add_counts(&mut manifest.graphs, &repeated.graphs).unwrap();
        manifest.pieces.push(repeated);
        save_manifest(scratch.path(), &manifest);
        assert!(restore(scratch.path(), &into)
            .unwrap_err()
            .to_string()
            .contains("per-graph counts"));
        assert!(!into.exists());
        manifest.pieces.pop();
        manifest.graphs = export(&source, scratch.path(), bounds(), 0).unwrap().graphs;
        save_manifest(scratch.path(), &manifest);
        restore(scratch.path(), &into).unwrap();
    }

    #[test]
    fn oversized_quad_timeout_and_invalid_manifest_leave_no_snapshot() {
        let source = fixture();
        let scratch = tempfile::tempdir().unwrap();
        assert!(export(
            &source,
            scratch.path(),
            Bounds {
                bytes: 1,
                ..bounds()
            },
            0
        )
        .is_err());
        assert!(export(
            &source,
            scratch.path(),
            Bounds {
                time: Duration::ZERO,
                ..bounds()
            },
            0
        )
        .is_err());
        let mut manifest = export(&source, scratch.path(), bounds(), 0).unwrap();
        manifest.pieces[0].bytes = CHUNK_BYTES + 1;
        save_manifest(scratch.path(), &manifest);
        let into = scratch.path().join("restored");
        assert!(restore(scratch.path(), &into).is_err());
        assert!(!into.exists());
    }

    #[test]
    fn failed_export_cleans_up_and_scripts_are_in_the_committed_snapshot() {
        let source = fixture();
        let scratch = tempfile::tempdir().unwrap();
        let scripts = tempfile::tempdir().unwrap();
        let settings = Settings {
            dir: scratch.path().join("backups"),
            keep: 1,
            every: None,
            store_path: scratch.path().join("live"),
            scripts: Some(scripts.path().to_path_buf()),
        };
        assert!(take(
            &source,
            &settings,
            Bounds {
                time: Duration::ZERO,
                ..bounds()
            },
            0
        )
        .is_err());
        assert_eq!(std::fs::read_dir(&settings.dir).unwrap().count(), 0);
        take(&source, &settings, bounds(), 0).unwrap();
        let paths = snapshots(&settings.dir);
        assert_eq!(paths.len(), 1);
        let snapshot = &paths[0];
        let into = scratch.path().join("restored");
        let scripts_archive = snapshot.join("scripts.json.gz");
        let saved = std::fs::read(&scripts_archive).unwrap();
        std::fs::write(&scripts_archive, b"corrupt").unwrap();
        assert!(restore(snapshot, &into)
            .unwrap_err()
            .to_string()
            .contains("scripts archive digest"));
        assert!(!into.exists());
        std::fs::write(&scripts_archive, saved).unwrap();
        restore(snapshot, &into).unwrap();
    }

    #[test]
    fn untrusted_manifest_counts_cannot_overflow() {
        let source = fixture();
        let scratch = tempfile::tempdir().unwrap();
        let mut manifest = export(&source, scratch.path(), bounds(), 0).unwrap();
        manifest.pieces[0].graphs =
            BTreeMap::from([("urn:a".into(), u64::MAX), ("urn:b".into(), 1)]);
        save_manifest(scratch.path(), &manifest);
        assert!(restore(scratch.path(), &scratch.path().join("restored")).is_err());
    }

    #[test]
    fn names_cannot_escape_the_backup_directory() {
        assert!(own_name("gonk-chunks-2026-11-14T200109Z-123"));
        for name in [
            "../gonk-chunks-2026-11-14T200109Z-123",
            "gonk-chunks-2026-11-14T200109Z-123/../x",
            "gonk-chunks-x",
        ] {
            assert!(!own_name(name));
        }
    }
}
