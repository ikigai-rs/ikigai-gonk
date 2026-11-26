//! A tenant's whole graph as streamed N-Quads, under the tenant's own grant (ledger
//! [#3649](http://localhost:1062/l/default/item/3649)).
//!
//! A tenant reads its graph through the store's scoped doors (`urn:iki:store:graph-construct`
//! and its siblings), and every answer there is bounded: **100,000 rows and 16 MiB** for a
//! capability holding no `urn:cap:store:answer:*` grant, refused past either, never truncated.
//! A busy ledger is already past that, so a tenant could not take a complete copy of its own
//! graph. The store's `urn:iki:store:dump` streams, but it reads EVERY graph and so requires the
//! broad `urn:cap:store:read`, which no tenant may hold.
//!
//! [`EXPORT_GRAPH`] closes that gap the way the chunked backup ([`super::chunked`]) does: one
//! snapshot, read in key order, cut into bounded WINDOWS, each no larger than an answer the
//! caller could already have been given on its own (at most the caller's answer bound, and at
//! most [`WINDOW_ROWS`] quads and [`WINDOW_BYTES`] bytes), and each produced within the caller's
//! own time budget. The windows are not separate documents: the body is one N-Quads stream, a
//! chunk of at most [`CHUNK_BYTES`] held at a time, so the whole graph never sits in memory.
//!
//! - **Authority**: the caller holds `urn:cap:store:read:graph:<graph>` for exactly the graph it
//!   asks for (or the broad `urn:cap:store:read`). The grant is matched exactly, as the store
//!   matches it, so a grant over one graph exports nothing of another.
//! - **Refused, never truncated**: a quad too large for one window, a window past the time
//!   budget, or a read error ends the stream with an error item, and the chunks before it are
//!   not a complete export.
//! - **Time**: the budget counts the work of producing a window, not the time the consumer
//!   takes to read it, so a slow link does not refuse an export a fast one completes.
//! - **Concurrency**: at most [`EXPORTS_IN_FLIGHT`] exports read at once on this server; one
//!   past that is refused `RetryAfter`. The export reads the store's snapshot directly, as the
//!   backup does, so the store's own evaluation slots do not count it.

use super::*;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use oxigraph::model::{GraphNameRef, NamedNodeRef};

/// Export one named graph as N-Quads: `Source urn:iki:gonk:export:graph` with `graph=<iri>`.
pub const EXPORT_GRAPH: &str = "urn:iki:gonk:export:graph";

/// The most quads one window holds: the store's base answer, so a tenant holding no answer
/// grant gets windows exactly as large as the answers it could already ask for.
pub const WINDOW_ROWS: u64 = 100_000;

/// The most N-Quads bytes one window holds: the store's base answer, for [`WINDOW_ROWS`]'s
/// reason. A larger answer grant cannot raise a window past it.
pub const WINDOW_BYTES: u64 = 16 << 20;

/// The most bytes one chunk of the body holds.
pub const CHUNK_BYTES: usize = 64 << 10;

/// How many exports may read at once on one server.
pub const EXPORTS_IN_FLIGHT: usize = 2;

const N_QUADS_MEDIA: &str = "application/n-quads";

pub(super) fn bind(space: EndpointSpace, backups: Backups) -> EndpointSpace {
    space.bind(
        Exact::new(EXPORT_GRAPH),
        ExportGraph {
            dataset: backups.dataset,
            in_flight: Arc::new(AtomicUsize::new(0)),
        },
    )
}

struct ExportGraph {
    dataset: Dataset,
    in_flight: Arc<AtomicUsize>,
}

/// One export's claim on [`EXPORTS_IN_FLIGHT`], released when the body is read to its end or
/// dropped.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(in_flight: &Arc<AtomicUsize>) -> Result<Slot> {
        // Taken first and given back when over, so two exports racing cannot both fit.
        let slot = Slot(Arc::clone(in_flight));
        if in_flight.fetch_add(1, Ordering::AcqRel) >= EXPORTS_IN_FLIGHT {
            return Err(Error::RetryAfter {
                detail: format!(
                    "{EXPORTS_IN_FLIGHT} graph exports are already reading on this server; try \
                     again shortly"
                ),
                delay: Some(Duration::from_secs(5)),
            });
        }
        Ok(slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ExportGraph {
    /// The graph the caller asked for, once the caller is shown to hold its read token.
    fn graph(inv: &Invocation<'_>) -> Result<String> {
        if inv.request.verb != Verb::Source {
            return Err(unsupported("gonk-export-graph", inv.request.verb));
        }
        let graph = inv.inline_str("graph")?.to_string();
        NamedNodeRef::new(&graph).map_err(|e| Error::InvalidArgument {
            name: "graph".into(),
            detail: format!("not an IRI: {e}"),
        })?;
        let token = ikigai_store::cap_read_graph(&graph);
        if !(inv.capability.allows(&token) || inv.capability.allows(ikigai_store::CAP_READ)) {
            return Err(Error::Denied(
                format!("exporting {graph} needs {token}").into(),
            ));
        }
        Ok(graph)
    }

    fn open(&self, inv: &Invocation<'_>) -> Result<Export> {
        let graph = Self::graph(inv)?;
        let bounds = Bounds::for_caller(&self.dataset, inv.capability);
        let slot = Slot::take(&self.in_flight)?;
        let node = NamedNodeRef::new_unchecked(&graph);
        // The snapshot is taken now, so the answer is the graph as it was when it was asked for.
        let quads = self.dataset.handle.quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(node)),
        );
        Ok(Export {
            quads: Some(Box::new(quads)),
            window: Window::new(bounds),
            _slot: slot,
        })
    }
}

#[async_trait]
impl Endpoint for ExportGraph {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb == Verb::Source {
            let body = self.invoke_body(inv).await?;
            return match inv.memory_budget() {
                Some(budget) => body.collect_within(budget).await,
                None => Err(Error::Endpoint(
                    "a graph export is read as a body; this invocation has no byte budget to \
                     collect it under"
                        .into(),
                )),
            };
        }
        Err(unsupported("gonk-export-graph", inv.request.verb))
    }

    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let export = self.open(inv)?;
        // Never cacheable: the body is one-shot, and an export is asked for once.
        Ok(Representation::from_body(
            ReprType::new(N_QUADS_MEDIA).with_param("charset", "utf-8"),
            Body::once(export),
        ))
    }

    fn name(&self) -> &str {
        "gonk-export-graph"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("One named graph, whole, as streamed N-Quads")
            .summary(
                "Every quad of one named graph, from one snapshot, as N-Quads streamed a chunk \
                 at a time, so a graph past the 100,000-quad, 16 MiB bound of a scoped SPARQL \
                 answer can still be taken whole. Needs the read token for exactly that graph \
                 (`urn:cap:store:read:graph:<graph>`) or the broad store read. The stream is cut \
                 into windows of at most 100,000 quads and 16 MiB, further bounded by the \
                 caller's answer grants, and each window must be produced within the caller's \
                 time budget. A quad too large for a window, a window past the time budget, or \
                 a read error ends the stream with an error: the chunks before it are not a \
                 complete export. At most two exports read at once; another is refused \
                 RetryAfter. `urn:iki:store:load` with `format=application/n-quads` reads it \
                 back.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_store::CAP_READ_ANY)
            .input(
                ArgSpec::new("graph")
                    .summary("the named graph's IRI; the caller holds its read token")
                    .class(XSD_STRING),
            )
            .output(N_QUADS_MEDIA)
    }
}

/// The current window's bounds and what it has used of them.
struct Window {
    rows_max: u64,
    bytes_max: u64,
    budget: Duration,
    rows: u64,
    bytes: u64,
    worked: Duration,
}

impl Window {
    fn new(bounds: Bounds) -> Window {
        Window {
            rows_max: bounds.rows.min(WINDOW_ROWS),
            bytes_max: bounds.bytes.min(WINDOW_BYTES),
            budget: bounds.time,
            rows: 0,
            bytes: 0,
            worked: Duration::ZERO,
        }
    }

    /// Whether a serialized quad of `len` bytes opens the next window: refused when it could
    /// fit no window at all.
    fn opens_next(&self, len: u64) -> Result<bool> {
        if len > self.bytes_max || self.rows_max == 0 {
            return Err(Error::OverBudget {
                detail: format!(
                    "a quad of {len} bytes does not fit this export's window of {} bytes; the \
                     stream failed, and the chunks before it are not a complete export",
                    self.bytes_max
                ),
            });
        }
        Ok(self.rows > 0 && (self.rows == self.rows_max || self.bytes + len > self.bytes_max))
    }

    fn open_next(&mut self) {
        self.rows = 0;
        self.bytes = 0;
        self.worked = Duration::ZERO;
    }

    fn count(&mut self, len: u64) {
        self.rows += 1;
        self.bytes += len;
    }

    fn check_time(&self, since: Instant) -> Result<()> {
        if self.worked + since.elapsed() >= self.budget {
            return Err(Error::Timeout(format!(
                "a window of this graph export ran past its time budget of {} ms and the \
                 stream was stopped; the chunks before it are not a complete export. A \
                 `urn:cap:store:budget:<ms>` grant raises it, up to the store's ceiling",
                self.budget.as_millis()
            )));
        }
        Ok(())
    }
}

type Quads = Box<dyn Iterator<Item = std::result::Result<Quad, ikigai_store::StorageError>> + Send>;

/// The body: the snapshot's quads, serialized a chunk at a time as the reader polls.
struct Export {
    quads: Option<Quads>,
    window: Window,
    _slot: Slot,
}

impl Export {
    /// The next chunk, `None` at the end of the graph.
    fn next_chunk(&mut self) -> Result<Option<Bytes>> {
        let Some(quads) = self.quads.as_mut() else {
            return Ok(None);
        };
        let mut since = Instant::now();
        let mut chunk = Vec::with_capacity(CHUNK_BYTES + 1024);
        let mut line = Vec::new();
        let mut read: u64 = 0;
        while chunk.len() < CHUNK_BYTES {
            let Some(quad) = quads.next() else {
                self.quads = None;
                break;
            };
            let quad = quad.map_err(|e| Error::Endpoint(format!("reading the snapshot: {e}")))?;
            line.clear();
            let mut writer = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut line);
            writer
                .serialize_quad(&quad)
                .map_err(|e| Error::Endpoint(format!("serializing a quad: {e}")))?;
            writer
                .finish()
                .map_err(|e| Error::Endpoint(format!("serializing a quad: {e}")))?;
            let len = line.len() as u64;
            if self.window.opens_next(len)? {
                // The window just completed is charged everything up to here.
                self.window.check_time(since)?;
                self.window.open_next();
                since = Instant::now();
            }
            self.window.count(len);
            chunk.extend_from_slice(&line);
            read += 1;
            if read.is_multiple_of(CLOCK_EVERY) {
                self.window.check_time(since)?;
            }
        }
        self.window.check_time(since)?;
        self.window.worked += since.elapsed();
        Ok((!chunk.is_empty()).then(|| Bytes::from(chunk)))
    }
}

impl futures_core::Stream for Export {
    type Item = Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.next_chunk() {
            Ok(Some(chunk)) => Poll::Ready(Some(Ok(chunk))),
            Ok(None) => Poll::Ready(None),
            Err(error) => {
                // Refused, never truncated: nothing more is read after the error.
                this.quads = None;
                Poll::Ready(Some(Err(error)))
            }
        }
    }
}
