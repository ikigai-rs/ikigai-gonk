//! Bounded segments of one snapshot, concatenated as ordinary gzip members.
//!
//! Small snapshots retain the original single-member format. Larger snapshots begin
//! with a comment identifying this format and end with a mandatory, hashed completion
//! record. The record detects truncation even exactly between complete gzip members.
//! All framing is N-Quads comments, so `gzip -dc` still produces valid N-Quads.
use super::*;
use std::io::{BufRead, Read};

const HEADER: &[u8] = b"# gonk-segmented/1\n";
const FOOTER: &[u8] = b"# gonk-end/1 ";
const SEGMENT_BYTES: usize = 64 << 20;
const SEGMENT_ROWS: u64 = 250_000;
const MAX_EXPANDED: u64 = 64 << 30;
const MAX_ROWS: u64 = 1_000_000_000;

fn io_error(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("segmented backup: {e}"))
}

fn member(archive: Vec<u8>, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::GzBuilder::new()
        .mtime(GZIP_MTIME)
        .operating_system(GZIP_OS)
        .write(
            BoundedWriter::new(archive, ARCHIVE_MAX_BYTES),
            flate2::Compression::new(GZIP_LEVEL),
        );
    // The writer's counter includes prior members as well as this one.
    encoder.get_mut().bytes = encoder.get_ref().inner.len() as u64;
    let failed = |e: std::io::Error| {
        if e.get_ref().is_some_and(|inner| inner.is::<PastBound>()) {
            Error::OverBudget {
                detail: format!("backup archive exceeds the {ARCHIVE_MAX_BYTES}-byte limit"),
            }
        } else {
            io_error(e)
        }
    };
    encoder.write_all(bytes).map_err(failed)?;
    Ok(encoder.finish().map_err(failed)?.inner)
}

pub(super) fn dump(dataset: &Dataset, bounds: Bounds) -> Result<Dump> {
    dump_with_segments(dataset, bounds, SEGMENT_BYTES, SEGMENT_ROWS)
}

fn dump_with_segments(
    dataset: &Dataset,
    bounds: Bounds,
    segment_bytes: usize,
    segment_rows: u64,
) -> Result<Dump> {
    use ikigai_store::budget::Measure;
    let mut archive = Vec::new();
    let mut segment = Vec::new();
    let mut line = Vec::new();
    let mut rows = 0;
    let mut total_rows = 0;
    let mut total_bytes = 0;
    let mut hasher = Sha256::new();
    let mut graphs = BTreeMap::new();
    let mut deadline = Instant::now() + bounds.time;
    let mut segmented = false;
    // Keep this ONE iterator alive across segments: separate graph queries would take
    // separate snapshots and could lose a concurrent move between graphs.
    for quad in dataset.handle.iter() {
        let quad = quad.map_err(io_error)?;
        line.clear();
        let mut serializer = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut line);
        serializer.serialize_quad(&quad).map_err(io_error)?;
        serializer.finish().map_err(io_error)?;
        if rows > 0 && (segment.len() + line.len() > segment_bytes || rows >= segment_rows) {
            if Instant::now() >= deadline {
                return Err(past_time(bounds.time));
            }
            if !segmented {
                archive = member(archive, HEADER)?;
                segmented = true;
            }
            archive = member(archive, &segment)?;
            if Instant::now() >= deadline {
                return Err(past_time(bounds.time));
            }
            segment.clear();
            rows = 0;
            deadline = Instant::now() + bounds.time;
        }
        rows += 1;
        total_rows += 1;
        if rows > bounds.rows {
            return Err(past(Measure::Rows, bounds.rows));
        }
        if segment.len() as u64 + line.len() as u64 > bounds.bytes {
            return Err(past(Measure::Bytes, bounds.bytes));
        }
        total_bytes += line.len() as u64;
        if total_bytes > MAX_EXPANDED || total_rows > MAX_ROWS {
            return Err(Error::OverBudget {
                detail: "backup exceeds 64 GiB or one billion quads".into(),
            });
        }
        if rows.is_multiple_of(CLOCK_EVERY) && Instant::now() >= deadline {
            return Err(past_time(bounds.time));
        }
        hasher.update(&line);
        segment.extend_from_slice(&line);
        *graphs.entry(graph_key(&quad.graph_name)).or_default() += 1;
    }
    if Instant::now() >= deadline {
        return Err(past_time(bounds.time));
    }
    archive = member(archive, &segment)?;
    if Instant::now() >= deadline {
        return Err(past_time(bounds.time));
    }
    let nquads_sha256 = hex(&hasher.finalize());
    if segmented {
        let footer = serde_json::json!({
            "bytes": total_bytes, "quads": total_rows,
            "sha256": nquads_sha256, "graphs": graphs,
        });
        let mut record = FOOTER.to_vec();
        record.extend_from_slice(footer.to_string().as_bytes());
        record.push(b'\n');
        archive = member(archive, &record)?;
        if Instant::now() >= deadline {
            return Err(past_time(bounds.time));
        }
    }
    Ok(Dump {
        archive,
        nquads_bytes: total_bytes,
        nquads_sha256,
        graphs,
    })
}

pub(super) fn is_segmented(content: &[u8]) -> Result<bool> {
    if !content.starts_with(&[0x1f, 0x8b]) {
        return Ok(false);
    }
    let mut header = [0; HEADER.len()];
    match flate2::read::MultiGzDecoder::new(content).read_exact(&mut header) {
        Ok(()) => Ok(header == HEADER),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(io_error(e)),
    }
}

/// Exposes just the quad lines to one parser, validating the completion record at EOF.
/// One parser preserves blank-node identity across members; no document-sized HashSet
/// or expanded document is held in RAM. The bulk loader deduplicates on disk.
struct Verified<R> {
    inner: R,
    line: std::io::Cursor<Vec<u8>>,
    hasher: Sha256,
    bytes: u64,
    rows: u64,
    expected: Option<BTreeMap<String, u64>>,
}

impl<R: BufRead> Verified<R> {
    fn new(mut inner: R) -> std::io::Result<Self> {
        let mut header = [0; HEADER.len()];
        inner.read_exact(&mut header)?;
        if header != HEADER {
            return Err(std::io::Error::other("missing segmented backup header"));
        }
        Ok(Self {
            inner,
            line: std::io::Cursor::new(Vec::new()),
            hasher: Sha256::new(),
            bytes: 0,
            rows: 0,
            expected: None,
        })
    }

    fn next_line(&mut self) -> std::io::Result<bool> {
        let mut line = std::mem::take(self.line.get_mut());
        line.clear();
        // A single quad can be larger than the preferred segment, but never larger
        // than the old 1 GiB document cap. read_until itself must be bounded too.
        self.inner
            .by_ref()
            .take(RESTORE_MAX_BYTES + 1)
            .read_until(b'\n', &mut line)?;
        if line.is_empty() || line.len() as u64 > RESTORE_MAX_BYTES {
            return Err(std::io::Error::other(
                "missing completion record or oversized quad",
            ));
        }
        if let Some(record) = line.strip_prefix(FOOTER) {
            let record: serde_json::Value =
                serde_json::from_slice(record).map_err(std::io::Error::other)?;
            let graphs: BTreeMap<String, u64> =
                serde_json::from_value(record["graphs"].clone()).map_err(std::io::Error::other)?;
            if record["bytes"].as_u64() != Some(self.bytes)
                || record["quads"].as_u64() != Some(self.rows)
                || record["sha256"].as_str() != Some(hex(&self.hasher.clone().finalize()).as_str())
                || graphs
                    .values()
                    .try_fold(0_u64, |sum, n| sum.checked_add(*n))
                    != Some(self.rows)
            {
                return Err(std::io::Error::other(
                    "completion record does not match the N-Quads",
                ));
            }
            // Drive the decoder through the footer member's CRC and the real EOF.
            // Extra members or junk after the record must not be silently ignored.
            let mut extra = [0];
            if self.inner.read(&mut extra)? != 0 {
                return Err(std::io::Error::other("data after the completion record"));
            }
            self.expected = Some(graphs);
            return Ok(false);
        }
        self.bytes += line.len() as u64;
        self.rows += 1;
        if self.bytes > MAX_EXPANDED || self.rows > MAX_ROWS {
            return Err(std::io::Error::other(
                "backup exceeds 64 GiB or one billion quads",
            ));
        }
        self.hasher.update(&line);
        self.line = std::io::Cursor::new(line);
        Ok(true)
    }
}

impl<R: BufRead> Read for Verified<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() || self.expected.is_some() {
            return Ok(0);
        }
        if self.line.position() == self.line.get_ref().len() as u64 && !self.next_line()? {
            return Ok(0);
        }
        self.line.read(out)
    }
}

/// Keep parsing, loading and verification off the server runtime. The staging guard
/// travels with the result: cancellation or panic drops it without publishing a store.
pub(super) async fn restore_off_thread(
    staging: Staging,
    content: Vec<u8>,
) -> Result<(Staging, BTreeMap<String, u64>)> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("gonk-backup-restore".into())
        .spawn(move || {
            let outcome = restore(staging.path(), &content).map(|counts| (staging, counts));
            let _ = tx.send(outcome);
        })
        .map_err(io_error)?;
    rx.await.map_err(|_| io_error("restore thread panicked"))?
}

pub(super) fn restore(dir: &Path, content: &[u8]) -> Result<BTreeMap<String, u64>> {
    // This private store shares the process with the live one. Reserve descriptors
    // for it and the server even under macOS's common 256-descriptor soft limit.
    let handle = oxigraph::store::Store::open_with_options(
        dir,
        oxigraph::store::StoreOptions::default().with_max_open_files(64),
    )
    .map_err(io_error)?;
    let mut reader = Verified::new(std::io::BufReader::new(flate2::read::MultiGzDecoder::new(
        content,
    )))
    .map_err(io_error)?;
    // Staging provides atomic publication; let RocksDB compact during loading.
    let mut loader = handle
        .bulk_loader()
        .without_atomicity()
        .with_num_threads(2)
        .with_max_memory_size_in_megabytes(64);
    // Use the engine's parser and loader directly in this NEW, private staging store.
    // One parser for the entire archive also avoids renaming shared blank nodes anew
    // for each segment, which repeated store:load requests would do.
    loader
        .load_from_reader(RdfFormat::NQuads, &mut reader)
        .map_err(io_error)?;
    let expected = reader
        .expected
        .ok_or_else(|| io_error("missing completion record"))?;
    loader.commit().map_err(io_error)?;
    // Count graph bindings without decoding every (potentially large) literal again.
    // This query reads the restored indexes, independently of the archive's counters.
    let results = oxigraph::sparql::SparqlEvaluator::new()
        .parse_query("SELECT ?g (COUNT(*) AS ?n) WHERE { { GRAPH ?g { ?s ?p ?o } } UNION { ?s ?p ?o } } GROUP BY ?g")
        .map_err(io_error)?
        .on_store(&handle)
        .execute()
        .map_err(io_error)?;
    let oxigraph::sparql::QueryResults::Solutions(solutions) = results else {
        return Err(io_error("graph count did not return solutions"));
    };
    let mut actual = BTreeMap::new();
    for solution in solutions {
        let solution = solution.map_err(io_error)?;
        let graph = match solution.get("g") {
            None => DEFAULT_GRAPH_KEY.to_string(),
            Some(Term::NamedNode(node)) => node.as_str().to_string(),
            Some(Term::BlankNode(node)) => format!("_:{}", node.as_str()),
            _ => return Err(io_error("invalid graph binding")),
        };
        let Some(Term::Literal(count)) = solution.get("n") else {
            return Err(io_error("missing graph count"));
        };
        actual.insert(graph, count.value().parse::<u64>().map_err(io_error)?);
    }
    // The loader renames blank nodes once for the entire archive, including graphs.
    let actual = blank_graphs_by_shape(actual);
    if actual != blank_graphs_by_shape(expected) {
        return Err(io_error(
            "restored graph set or per-graph counts do not match the completion record",
        ));
    }
    drop(handle);
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Dataset, Bounds) {
        let (store, handle) = ikigai_store::DurableStore::in_memory_shared().unwrap();
        handle.load_from_slice(RdfFormat::NQuads, b"_:shared <urn:p> _:other <urn:g> .\n_:other <urn:q> \"one\" _:graph .\n_:shared <urn:r> \"two\" .\n_:other <urn:t> _:shared _:graph .\n").unwrap();
        let dataset = Dataset::new(&store, handle);
        let bounds = Bounds::for_caller(&dataset, &Capability::root());
        (dataset, bounds)
    }

    #[test]
    fn segments_cross_the_answer_bound_and_preserve_shared_blank_nodes() {
        let (dataset, bounds) = fixture();
        // Each segment fits, but the complete snapshot exceeds both grants.
        let bounds = Bounds {
            rows: 1,
            bytes: 256,
            ..bounds
        };
        let dump = dump_with_segments(&dataset, bounds, 256, 1).unwrap();
        assert!(dump.quads() > bounds.rows);
        assert!(is_segmented(&dump.archive).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("restored");
        let counts = restore(&target, &dump.archive).unwrap();
        assert_eq!(counts, blank_graphs_by_shape(dump.graphs));
        let restored = oxigraph::store::Store::open(&target).unwrap();
        let quads: Vec<_> = restored.iter().map(|q| q.unwrap()).collect();
        let p = quads
            .iter()
            .find(|q| q.predicate.as_str() == "urn:p")
            .unwrap();
        let t = quads
            .iter()
            .find(|q| q.predicate.as_str() == "urn:t")
            .unwrap();
        assert_eq!(p.subject.to_string(), t.object.to_string());
        assert_eq!(p.object.to_string(), t.subject.to_string());
    }

    #[test]
    fn missing_footer_crc_damage_and_trailing_members_are_refused() {
        let (dataset, bounds) = fixture();
        let dump = dump_with_segments(&dataset, bounds, 256, 1).unwrap();
        let mut remaining = dump.archive.as_slice();
        let mut last_member = 0;
        while !remaining.is_empty() {
            last_member = dump.archive.len() - remaining.len();
            let mut decoder = flate2::bufread::GzDecoder::new(remaining);
            std::io::copy(&mut decoder, &mut std::io::sink()).unwrap();
            remaining = decoder.into_inner();
        }
        let mut damaged = dump.archive.clone();
        let n = damaged.len();
        damaged[n - 8] ^= 1;
        let trailing = member(dump.archive.clone(), b"<urn:s> <urn:p> <urn:o> .\n").unwrap();
        for archive in [
            &dump.archive[..last_member],
            &damaged[..],
            &trailing[..],
            &dump.archive[..n - 1],
        ] {
            let dir = tempfile::tempdir().unwrap();
            assert!(restore(&dir.path().join("bad"), archive).is_err());
        }
    }

    #[test]
    fn the_resource_reads_local_archives_checks_authority_and_cleans_failed_staging() {
        let (dataset, bounds) = fixture();
        let dump = dump_with_segments(&dataset, bounds, 256, 1).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let name = "gonk-store-test.nq.gz";
        let archive = scratch.path().join(name);
        let into = scratch.path().join("new/restored");
        let kernel = Kernel::new(Arc::new(space(Backups {
            settings: Arc::new(Settings {
                dir: scratch.path().to_path_buf(),
                keep: 5,
                every: None,
                store_path: scratch.path().join("live"),
                scripts: None,
            }),
            jobs: None,
            next: NextRun::default(),
            dataset,
        })));
        let request = || {
            Request::new(Verb::Sink, iri(RESTORE).unwrap())
                .with_arg("archive", ArgRef::Inline(name.as_bytes().to_vec()))
                .with_arg(
                    "into",
                    ArgRef::Inline(into.to_str().unwrap().as_bytes().to_vec()),
                )
        };
        let owner = Capability::scoped([CAP_RESTORE, CAP_BACKUP]);
        let issue = |request, capability: &Capability| {
            futures::executor::block_on(kernel.issue(request, capability))
        };
        std::fs::write(&archive, &dump.archive).unwrap();
        assert!(matches!(
            issue(request(), &Capability::scoped([CAP_RESTORE])),
            Err(Error::Denied(_))
        ));
        assert!(matches!(
            issue(
                request().with_arg("content", ArgRef::Inline(Vec::new())),
                &owner
            ),
            Err(Error::InvalidArgument { .. })
        ));
        assert!(matches!(
            issue(
                request().with_arg(
                    "archive",
                    ArgRef::Inline(b"../gonk-store-test.nq.gz".to_vec())
                ),
                &owner
            ),
            Err(Error::InvalidArgument { .. })
        ));
        std::fs::write(&archive, &dump.archive[..dump.archive.len() - 8]).unwrap();
        assert!(issue(request(), &owner).is_err());
        assert!(!into.exists());
        assert!(
            !scratch.path().join("new").exists(),
            "failed staging and its ancestors were removed"
        );
        std::fs::write(&archive, &dump.archive).unwrap();
        let answer = issue(request(), &owner).unwrap();
        assert!(String::from_utf8_lossy(&answer.bytes).contains("restored 4 quads"));
        assert!(into.is_dir());
        assert!(
            matches!(issue(request(), &owner), Err(Error::InvalidArgument { .. })),
            "never merge a second restore"
        );
    }

    #[test]
    fn matching_byte_digest_cannot_hide_a_changed_graph_partition() {
        let (dataset, bounds) = fixture();
        let dump = dump_with_segments(&dataset, bounds, 256, 1).unwrap();
        let mut expanded = Vec::new();
        flate2::read::MultiGzDecoder::new(dump.archive.as_slice())
            .read_to_end(&mut expanded)
            .unwrap();
        let offset = expanded
            .windows(FOOTER.len())
            .position(|w| w == FOOTER)
            .unwrap();
        let mut record: serde_json::Value =
            serde_json::from_slice(&expanded[offset + FOOTER.len()..]).unwrap();
        record["graphs"] = serde_json::json!({"urn:wrong-graph": 4});
        expanded.truncate(offset + FOOTER.len());
        expanded.extend_from_slice(record.to_string().as_bytes());
        expanded.push(b'\n');
        let archive = member(Vec::new(), &expanded).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let error = restore(&dir.path().join("bad"), &archive).unwrap_err();
        assert!(error.to_string().contains("per-graph counts"), "{error}");
    }

    #[test]
    fn a_false_completion_record_is_refused() {
        let (dataset, bounds) = fixture();
        let dump = dump_with_segments(&dataset, bounds, 256, 1).unwrap();
        let mut expanded = Vec::new();
        flate2::read::MultiGzDecoder::new(dump.archive.as_slice())
            .read_to_end(&mut expanded)
            .unwrap();
        let offset = expanded
            .windows(FOOTER.len())
            .position(|w| w == FOOTER)
            .unwrap();
        let mut record: serde_json::Value =
            serde_json::from_slice(&expanded[offset + FOOTER.len()..]).unwrap();
        record["bytes"] = serde_json::json!(0);
        expanded.truncate(offset + FOOTER.len());
        expanded.extend_from_slice(record.to_string().as_bytes());
        expanded.push(b'\n');
        let archive = member(Vec::new(), &expanded).unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(restore(&dir.path().join("bad"), &archive).is_err());
    }
}
