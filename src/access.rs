//! The access log: one `log:Access` entry per request a door is asked, in gonk's `ikigai-log`
//! segment (ledger [#739](http://localhost:1060/l/default/item/739),
//! [#383](http://localhost:1062/l/default/item/383)).
//!
//! Brian's demo on 2026-10-03 slowed down for the better part of two hours and
//! `/tmp/ikigai-gonk.log` had no line after startup, so how slow, and which pages, was
//! unknowable afterwards. Each door has written one line per request since. Until ledger #383
//! they went to stderr, in `ikigai-log`'s grammar but in no segment; now they are written by
//! `ikigai-log`'s own door-line helper ([`ikigai_log::AccessLog`], ledger #3390) into ONE
//! hash-chained segment in gonk's data home ([`crate::quic::Layout::log_dir`], `gonk/log/`),
//! beside the passkey sign-ins ([`crate::identity::Passkeys::auditing`]). stderr keeps the
//! startup banner and fatal errors only, so each request has one record:
//!
//! ```text
//! 2026-11-10T14:03:12.345Z log:Access urn:iki:gonk:page:queue seq=12 door=http verb=source outcome=ok bytes=48213 dur=231 principal=anon credential=https://ikigai-rs.dev/ns/log#anonymous
//! 2026-11-10T14:03:12.410Z log:Access urn:iki:ledger:items seq=13 door=socket verb=source outcome=ok bytes=5120 dur=3 principal=urn:iki:host:gonk:owner credential=https://ikigai-rs.dev/ns/log#socketOwner
//! ```
//!
//! The columns are the helper's and are declared in `ikigai-log`'s vocabulary, so a segment
//! transrepts to Turtle (`urn:log:<segment>`) with nothing gonk-specific to parse. gonk decides
//! only what the DOOR knows:
//!
//! | key | what |
//! |---|---|
//! | subject | the IRI the door resolved — for HTTP, what the route mapped the path to |
//! | `door` | `http`, `socket` or `quic` |
//! | `verb` | `source`, `sink`, `exists`, `delete`, `meta` |
//! | `outcome` | `ok`, or the kernel's error kind (`denied`, `not-found`, `invalid-argument`, …) |
//! | `bytes` | the representation's body length; absent on an error |
//! | `dur` | milliseconds, from the door's issue to its answer |
//! | `principal` | who, as THIS door authenticated them: [`OWNER_PRINCIPAL`] on the socket; the client's `urn:iki:host:gonk:client:<fingerprint>` on QUIC; a signed-in passkey's IRI on HTTP; `anon` for anyone the door did not authenticate, on a read as on a write — explicit, so "nobody said who" is in the record |
//! | `credential` | how the door knew: `log:socketOwner`, `log:clientCertificate`, `log:passkey`, `log:anonymous` |
//! | `q` | HTTP only: the request's arguments AS SERVED, percent-encoded; absent when none. The caller's query, plus every declared default the kernel filled in before dispatch (core 0.1.104+, ledger #19) |
//!
//! Past [`MAX_FIELD`] bytes the subject and `q` are cut and end in `…`, a character the
//! encoding never writes, so a cut is unambiguous.
//!
//! **Never written:** a cookie, an `Authorization` header, a session or wire token, or a form
//! body. This layer never sees a header at all; the body of a write is the argument
//! `content`, which is skipped, as are the transport's provenance stamps (`received`,
//! `client`, `principal`) — `principal` gets its own column, from the capability.
//!
//! ⚠ **No `grant=` yet.** The helper has the column; what a door has in hand per request is
//! the capability's scopes, not the NAME of the grant they came from (that is in
//! `clients.json` / the passkey enrollment, read by the admission). Threading it to this
//! overlay is a change of its own.
//!
//! # ★ Per request, never the global tracer
//!
//! The principal comes from the capability the door MINTED ([`crate::admit::principal`]),
//! request by request, so three doors and every passkey and client are attributed separately.
//! gonk installs no tracer at all — not with `set_tracer`, which would build a trace event for
//! every resolution, cached reads included, on the ledger's microsecond read path, and not per
//! call either: none of the three transport crates offers a seam to issue a request through
//! `Kernel::issue_traced`, so the kernel's own resolutions are not in the segment. The door
//! line is. And a write to the log goes through `FileSink` with `std::fs`, never by resolving,
//! so it can never be a trace event of its own (the recursion hazard the log's T5 brief named).
//!
//! # The segments: where, who reads them, and the backup
//!
//! - **Where:** `gonk/log/` beside `clients.json`, unless `log.toml` or `gonk.log.toml` in the
//!   ikigai config home says otherwise (`directory`, `level`, `destination = "off"`, `[seal]`,
//!   `[rotation]`). `ikigai-log` layers the two, so `gonk.log.toml` is gonk's own file.
//! - **Who:** `urn:log:segments`, `urn:log:verify`, `urn:log:<segment>`, `urn:log:config` and
//!   `urn:log:write` are bound in the hub and need `urn:cap:log:{read,write,config}`, which
//!   are the owner's alone ([`crate::grants::CAP_GONK_ADMIN`]): a segment holds every request,
//!   every principal and every item IRI. The socket's root reaches them; the HTTP door answers
//!   `403` and the QUIC door refuses, as for the backup.
//! - **The backup does not hold them.** They live outside the RocksDB store. They carry their
//!   own tamper evidence (the hash chain spans segments and restarts), but a lost disk loses
//!   them: copy `gonk/log/` beside the archives if the record must outlive the machine.
//!
//! # ⚠ What this sees, and what it cannot
//!
//! The log is an overlay on each door's kernel ([`Logged`]), which wraps the endpoint a
//! host-issued request resolves to, so it times exactly the work a door asked for. Sub-requests
//! a page issues are part of that work and are not lines of their own (`Invocation::depth`).
//! What it cannot see, because none of the three transport crates offers a seam:
//!
//! - **the HTTP status and the bytes on the wire.** `ikigai-web` maps the outcome to a status
//!   and adds headers after the kernel returns; `outcome` is the input to that mapping. A
//!   `304`, a `405`, a `406`, a `413` or a strict-route `400` is decided before or after the
//!   kernel and is not logged at all.
//! - **who an anonymous request is from.** `anon`, and nothing that identifies the caller: no
//!   address. A `?principal=` a reader typed is never taken for one (ledger #864, R6).
//! - **a refusal the door kernel makes before dispatch** (a refusal the HTTP EDGE makes is
//!   written by the edge itself, [`AccessLog::refused_at_edge`], ledger #894) — a capability
//!   floor's denial, a name nothing binds (`404`), a nesting refusal — and the kernel's own
//!   `urn:kernel:*` operations and Meta answers, which no endpoint of this server's spaces
//!   handles.
//!
//! # ★ A poll is sampled, not logged every time (ledger [#767](http://localhost:1060/l/default/item/767))
//!
//! Every other line is a person doing something; a POLL is a timer, and it writes whether
//! anyone is looking or not. The Queue badge ([`crate::queue::BADGE_IRI`]) is fetched every
//! ten seconds by every open gonk page that shows it, so one forgotten tab wrote 360 lines an
//! hour into a file launchd never rotates. Measured on the live `/tmp/ikigai-gonk.log` on
//! 2026-10-10, 3.75 days after the log began: 9,609 access lines, **6,198 of them (64%; 850 KB
//! of 1.45 MB) the badge poll** — about 1.2 MB a day per open page, forever, with nobody
//! doing anything.
//!
//! So a subject named by [`AccessLog::sampling_polls`] writes its line only when the line says
//! something: when it FAILED, when it took [`POLL_SLOW_MS`] or more (the same second
//! `grep ' dur=[0-9]\{4,\} '` finds — the reason this log exists, ledger #739), or when that
//! subject has written no quiet line for [`POLL_SAMPLE_EVERY`] (ten minutes), so a quiet poll
//! still shows it is alive. A sampled line is an ordinary line: the grammar does not change.
//!
//! ★ **The sample is per subject and by TIME** (ledger
//! [#1718](http://localhost:1062/l/default/item/1718)). It was one line in 60 polls, counted
//! across every polled subject together: tuned for the badge's ten-second poll, it became one
//! line per three minutes for the Layers page's three-second poll, and a busy subject could use
//! up a quiet one's sample. A clock does not care how often a page asks.
//!
//! ★ **A LONG poll is sampled too, by count alone** (ledger
//! [#1694](http://localhost:1062/l/default/item/1694)). The Layers page's
//! (`crate::layers_view::WAIT_IRI`) waited up to 45 seconds by design, so the slow rule above
//! wrote every one of them: one line per change or per 45 seconds per open Layers tab. (Since
//! ledger [#1705](http://localhost:1062/l/default/item/1705) it is a short poll every three
//! seconds, sampled by the rule above: one quiet line per ten minutes, and every slow one; gonk
//! names no long poll now.) A subject
//! named by [`AccessLog::sampling_long_polls`] writes when it FAILED or once per
//! [`POLL_SAMPLE_EVERY`]; its duration is the wait, not a symptom, and says nothing. A refusal is
//! an outcome other than `ok`, so it is always written (a capability floor's refusal is the
//! kernel's, before dispatch, and is not a line at all: see above).
//! [`AccessLog::gonk_polls`] names gonk's polls for every door.
//!
//! ⚠ **What this does not bound.** The rest of the log grows with what people do, which is
//! what an access log is for, and a server that is slow for a long time logs every slow poll
//! for as long as it is slow — on purpose: that is the incident. Rotation is the segment's
//! (`[rotation]` in `gonk.log.toml`), verified and sealed by `ikigai-log`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Body, Capability, Description, Endpoint, Error, Invocation, Iri, Representation,
    Request, Resolution, Result, Scope, Space, SpaceEntry, Topology, Verb,
};
use ikigai_log::{
    Access, Caller, ClosureSink, Credential, Destination, Entry, LogConfig, LogHandle, Principal,
    Timestamp, Vocabulary,
};

/// The door line's class: `log:Access`, `ikigai-log`'s own (ledger #3390). gonk declares no
/// class of its own for it.
pub const CLASS: &str = ikigai_log::ACCESS_CLASS;

/// The longest subject or `q` written whole, in bytes: `ikigai-log`'s bound, which cuts and
/// marks with `…` ([`ikigai_log::CUT_MARKER`]).
pub const MAX_FIELD: usize = ikigai_log::MAX_FIELD;

/// The principal the socket door writes: the owner, whose peer UID the door checked against
/// its own. An IRI because a `principal=` column is one (`log:onBehalfOf`); minted by this
/// process and by no caller.
pub const OWNER_PRINCIPAL: &str = "urn:iki:host:gonk:owner";

/// The app name `ikigai-log` layers its config under: `log.toml`, then `gonk.log.toml`, in the
/// ikigai config home. The segment's instance name too.
pub const APP: &str = "gonk";

/// A polled subject ([`AccessLog::sampling_polls`]) writes at most one QUIET line per this long,
/// whatever its poll's interval and however many pages ask it (ledger #1718).
pub const POLL_SAMPLE_EVERY: Duration = Duration::from_secs(600);

/// A polled subject's request that took this many milliseconds or more always writes its line.
pub const POLL_SLOW_MS: u128 = 1000;

/// The arguments a line never carries: the body of a write and the transport's own stamps.
/// (`ikigai-log` withholds `content` and every secret-named argument on its own; the stamps are
/// gonk's to name.)
const UNLOGGED: [&str; 5] = ["content", "content-type", "received", "client", "principal"];

/// Which door a line is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Door {
    /// The loopback HTTP door (`ikigai-web`).
    Http,
    /// The owner-only Unix socket (`ikigai-ipc`).
    Socket,
    /// The mutual-TLS QUIC door (`ikigai-quic`).
    Quic,
}

impl Door {
    /// The word the `door=` column carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Door::Http => "http",
            Door::Socket => "socket",
            Door::Quic => "quic",
        }
    }
}

/// Where a test's lines go: one call per entry line, without its newline.
pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

/// gonk's one record of who did what: an `ikigai-log` handle with its segment open, and the
/// shared door-line helper over it ([`ikigai_log::AccessLog`]). Every door's [`AccessLog`] and
/// the passkey sign-in lines ([`crate::identity::Passkeys::auditing`]) write through ONE of
/// these, so they share one hash chain.
pub struct Record {
    handle: Arc<LogHandle>,
    doors: ikigai_log::AccessLog,
}

impl Record {
    /// The server's record: segments in `dir` (gonk's data home's `log/`, unless `log.toml` or
    /// `gonk.log.toml` in `config_home` says otherwise), opened now. `Ok(None)` when those files
    /// turn the destination `off`.
    ///
    /// `config_home` is gonk's own (`--config-home` included), not the environment's, so the
    /// layers read are the ones beside the `config.toml` this server read.
    pub fn open(
        config_home: &Path,
        dir: &Path,
    ) -> std::result::Result<Option<Arc<Record>>, String> {
        let base = LogConfig::default()
            .with_destination(Destination::File)
            .with_instance(APP)
            .with_directory(dir);
        let config = ikigai_log::load::complete_in(config_home, Some(APP), base)
            .map_err(|e| format!("the log config (log.toml, gonk.log.toml): {e}"))?;
        let handle = LogHandle::new(
            Some(config_home.to_path_buf()),
            Some(APP.to_string()),
            config,
        );
        let opened = handle
            .open(Vocabulary::shared_builtin(), now())
            .map_err(|e| format!("opening a log segment: {e}"))?;
        Ok(opened.then(|| Record::over(Arc::new(handle))))
    }

    /// A record whose lines go to `sink` — a test's collector. Only the lines gonk wrote are
    /// passed on: the door lines and the sign-ins, not the segment's header, seals or
    /// `log:ProcessStart`.
    pub fn to(sink: Sink) -> Arc<Record> {
        let handle = Arc::new(LogHandle::new(
            None,
            Some(APP.to_string()),
            LogConfig::default(),
        ));
        handle
            .open_with_sink(
                Vocabulary::shared_builtin(),
                now(),
                Box::new(ClosureSink(move |line: &str| {
                    if is_written_by_gonk(line) {
                        sink(line);
                    }
                })),
            )
            .expect("a closure sink opens");
        Record::over(handle)
    }

    fn over(handle: Arc<LogHandle>) -> Arc<Record> {
        let doors =
            ikigai_log::AccessLog::new(Arc::clone(&handle), Arc::new(ikigai_core::SystemClock));
        Arc::new(Record { handle, doors })
    }

    /// The handle `urn:log:*` is served over ([`ikigai_log::endpoints::space`]).
    pub fn handle(&self) -> Arc<LogHandle> {
        Arc::clone(&self.handle)
    }

    /// Write one door line. A line that does not land is said on stderr, and the request
    /// it describes is not failed.
    pub fn access(&self, access: &Access) {
        if let Err(e) = self.doors.record(access) {
            eprintln!("ikigai-gonk: {e}");
        }
    }

    /// Write one entry of gonk's own (a passkey sign-in or sign-out).
    pub fn entry(&self, entry: Entry) {
        if let Err(e) = self.handle.write(entry) {
            eprintln!("ikigai-gonk: a log entry was not written: {e}");
        }
    }

    /// Write `log:ProcessStop` and close the segment, so the next start chains from a sealed
    /// head.
    pub fn close(&self) {
        let _ = self.handle.close(now());
    }
}

/// Whether a segment line is one gonk wrote: an entry (it starts with its time) whose class is
/// the door line or one of gonk's own, and not `ikigai-log`'s bookkeeping (the header, a seal,
/// `log:ProcessStart`).
fn is_written_by_gonk(line: &str) -> bool {
    let mut columns = line.split(' ');
    let entry = columns
        .next()
        .is_some_and(|time| time.starts_with(|c: char| c.is_ascii_digit()));
    let class = columns.next().unwrap_or("");
    entry && (class == "log:Access" || !class.starts_with("log:"))
}

/// Now, as the log's timestamp.
pub fn now() -> Timestamp {
    Timestamp::from_millis(millis(SystemTime::now()))
}

/// One door's access log.
#[derive(Clone)]
pub struct AccessLog {
    door: Door,
    record: Arc<Record>,
    polls: Arc<Polls>,
}

/// The subjects a page polls on a timer, and when each last wrote a quiet line.
#[derive(Default)]
struct Polls {
    subjects: Vec<String>,
    /// Long polls: their duration is the wait they were asked for, so it is never a reason to
    /// write.
    long: Vec<String>,
    /// Keyed by subject, so one entry per configured poll at most: the map is bounded by the
    /// two lists above, never by what callers ask for.
    sampled: Mutex<HashMap<String, Instant>>,
}

impl AccessLog {
    /// `door`'s lines, into `record`'s segment — what `main` builds for each door.
    pub fn on(door: Door, record: Arc<Record>) -> AccessLog {
        AccessLog {
            door,
            record,
            polls: Arc::default(),
        }
    }

    /// Lines to `sink` — a test's collector ([`Record::to`]).
    pub fn to(door: Door, sink: Sink) -> AccessLog {
        AccessLog::on(door, Record::to(sink))
    }
    /// Sample the lines of `subjects` — resources a page polls on a timer — instead of writing
    /// one per request: a polled request writes its line when it fails, when it took
    /// [`POLL_SLOW_MS`] or more, or when its subject has written no quiet line for
    /// [`POLL_SAMPLE_EVERY`] (its first poll included). Every other subject is unaffected. See
    /// the module docs for why (ledger #767, #1718).
    pub fn sampling_polls<S: Into<String>>(
        self,
        subjects: impl IntoIterator<Item = S>,
    ) -> AccessLog {
        AccessLog {
            polls: Arc::new(Polls {
                subjects: subjects.into_iter().map(Into::into).collect(),
                long: self.polls.long.clone(),
                sampled: Mutex::default(),
            }),
            ..self
        }
    }

    /// Sample the lines of `subjects` — LONG polls, which wait for news by design — as
    /// [`AccessLog::sampling_polls`] does, but never write one for its duration: a long poll's
    /// request writes its line when it fails, or once per [`POLL_SAMPLE_EVERY`] (the first
    /// included). Ledger #1694.
    pub fn sampling_long_polls<S: Into<String>>(
        self,
        subjects: impl IntoIterator<Item = S>,
    ) -> AccessLog {
        AccessLog {
            polls: Arc::new(Polls {
                subjects: self.polls.subjects.clone(),
                long: subjects.into_iter().map(Into::into).collect(),
                sampled: Mutex::default(),
            }),
            ..self
        }
    }

    /// gonk's own polls, sampled: the Queue badge, the header's layers count and an open Layers
    /// page's poll, all short polls (the Layers page's was a long poll until ledger #1705). What
    /// `main` gives every door.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use ikigai_gonk::access::{AccessLog, Door};
    ///
    /// let log = AccessLog::to(Door::Http, Arc::new(|_: &str| {})).gonk_polls();
    /// assert!(log.samples(ikigai_gonk::queue::BADGE_IRI));
    /// assert!(log.samples(ikigai_gonk::layers_view::COUNT_IRI));
    /// assert!(log.samples(ikigai_gonk::layers_view::WAIT_IRI));
    /// assert!(!log.samples("urn:iki:gonk:page:layers"));
    /// ```
    pub fn gonk_polls(self) -> AccessLog {
        self.sampling_polls([
            crate::queue::BADGE_IRI,
            crate::layers_view::COUNT_IRI,
            crate::layers_view::WAIT_IRI,
        ])
    }

    /// Whether `subject`'s lines are sampled rather than written for every request.
    pub fn samples(&self, subject: &str) -> bool {
        self.polls.subjects.iter().any(|polled| polled == subject)
            || self.polls.long.iter().any(|polled| polled == subject)
    }

    /// Whether a request for `subject` that ended `ok` (or not) after `dur_ms` writes a line.
    fn writes(&self, subject: &str, ok: bool, dur_ms: u128) -> bool {
        self.writes_at(subject, ok, dur_ms, Instant::now())
    }

    /// [`writes`](Self::writes), at `now`: a test's clock.
    fn writes_at(&self, subject: &str, ok: bool, dur_ms: u128, now: Instant) -> bool {
        let long = self.polls.long.iter().any(|polled| polled == subject);
        if !long && !self.polls.subjects.iter().any(|polled| polled == subject) {
            return true;
        }
        if !ok || (!long && dur_ms >= POLL_SLOW_MS) {
            return true;
        }
        // A quiet poll: written when this subject's last quiet line is a sample period old (or
        // there was none). A failure or a slow line does not reset the period: it says something
        // else, and a quiet line after it still says the poll is alive.
        let mut sampled = self.polls.sampled.lock().unwrap_or_else(|e| e.into_inner());
        match sampled.get(subject) {
            Some(at) if now.saturating_duration_since(*at) < POLL_SAMPLE_EVERY => false,
            _ => {
                sampled.insert(subject.to_string(), now);
                true
            }
        }
    }

    /// The door this log describes.
    pub fn door(&self) -> Door {
        self.door
    }

    /// Write the line for a request the door's EDGE refused before any kernel saw it (ledger
    /// [#894](http://localhost:1062/l/default/item/894)): `outcome=denied`, no `bytes`, `dur=0`,
    /// and the anonymous caller, because a refused request's cookie is not believed. Always
    /// written: a refusal is never sampled.
    ///
    /// ```
    /// use std::sync::{Arc, Mutex};
    /// use ikigai_core::{Iri, Request, Verb};
    /// use ikigai_gonk::access::{AccessLog, Door};
    ///
    /// let kept = Arc::new(Mutex::new(Vec::<String>::new()));
    /// let into = Arc::clone(&kept);
    /// let log = AccessLog::to(Door::Http, Arc::new(move |l: &str| into.lock().unwrap().push(l.to_string())));
    /// let write = Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap());
    /// log.refused_at_edge(&write, "a cross-site write");
    /// let line = kept.lock().unwrap().iter().find(|l| l.contains(" log:Access ")).cloned().unwrap();
    /// assert!(line.contains(
    ///     " log:Access urn:iki:ledger:append seq=2 door=http verb=sink outcome=denied dur=0 principal=anon \
    ///      credential=https://ikigai-rs.dev/ns/log#anonymous"
    /// ), "{line}");
    /// ```
    pub fn refused_at_edge(&self, request: &Request, why: &str) {
        let access = access(
            millis(SystemTime::now()),
            self.door,
            request,
            &Capability::scoped(Vec::<String>::new()),
            Err(&Error::Denied(why.to_string())),
            0,
        );
        self.record.access(&access);
    }

    /// Wrap `space` so every host-issued request resolved through it writes a line.
    pub fn over(&self, space: Arc<dyn Space>) -> Logged {
        Logged {
            inner: space,
            log: self.clone(),
        }
    }
}

/// A space that times what its kernel's host asks of it: an overlay with no identity or
/// structure of its own — it forwards [`Space::id`], [`Space::topology`] and
/// [`Space::entries`], so `urn:kernel:topology` reads the same with the log on or off — that
/// wraps each resolved endpoint in a timing endpoint (`Timed`).
pub struct Logged {
    inner: Arc<dyn Space>,
    log: AccessLog,
}

impl Space for Logged {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        // `map_endpoint` carries the resolution's canonical name and answering space through,
        // and leaves a limiter alone — the two things a hand-written wrap gets wrong.
        self.inner.resolve(request, scope).map_endpoint(|endpoint| {
            Arc::new(Timed {
                inner: endpoint,
                log: self.log.clone(),
            }) as Arc<dyn Endpoint>
        })
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        self.inner.entries()
    }

    fn id(&self) -> Option<Iri> {
        self.inner.id()
    }

    fn topology(&self) -> Topology {
        self.inner.topology()
    }
}

/// The endpoint [`Logged`] resolves to: the real one, timed.
///
/// ⚠ A fresh wrapper per resolution, so the kernel's floor memo (keyed on the endpoint's
/// address) misses and re-reads `describe()` for each request through a logged door — the
/// cost every per-resolution overlay pays, bounded by the kernel. Measured with the log on
/// and off (`examples/access-cost.rs`, release, interleaved, 2026-10-05): a 50-item ledger
/// page through the HTTP door, median 21.45 ms off and 21.47 ms on (inside the noise); the
/// cheapest read a door serves, a cached `urn:iki:ledger:items` over the socket door's kernel,
/// 4.6 µs off and 5.8 µs on — about a microsecond per request, the line's formatting and its
/// one write.
struct Timed {
    inner: Arc<dyn Endpoint>,
    log: AccessLog,
}

impl Timed {
    /// Write the line for one request that arrived at the door, when the log takes it.
    fn record(
        &self,
        inv: &Invocation<'_>,
        started: SystemTime,
        clock: Instant,
        answer: std::result::Result<usize, &Error>,
    ) {
        let dur_ms = clock.elapsed().as_millis();
        if self
            .log
            .writes(inv.request.target.as_str(), answer.is_ok(), dur_ms)
        {
            let access = access(
                millis(started),
                self.log.door,
                inv.request,
                inv.capability,
                answer,
                dur_ms,
            );
            self.log.record.access(&access);
        }
    }
}

#[async_trait]
impl Endpoint for Timed {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // A sub-request a page issues is part of the request that page answers, not a line.
        if inv.depth() > 0 {
            return self.inner.invoke(inv).await;
        }
        let started = SystemTime::now();
        let clock = Instant::now();
        let answer = self.inner.invoke(inv).await;
        self.record(inv, started, clock, answer.as_ref().map(|r| r.bytes.len()));
        answer
    }

    /// The body as the endpoint produced it, timed the same way (ledger #2730): a body the
    /// door streams (the backup archive's download) is not collected here. Its line records
    /// the body's length when it is known before reading, and `0` for a one-shot stream; the
    /// duration is the time to the answer, not to its last byte.
    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        if inv.depth() > 0 {
            return self.inner.invoke_body(inv).await;
        }
        let started = SystemTime::now();
        let clock = Instant::now();
        let answer = self.inner.invoke_body(inv).await;
        let bytes = answer
            .as_ref()
            .map(|r| r.bytes.len_hint().map_or(0, |n| n as usize));
        self.record(inv, started, clock, bytes);
        answer
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        self.inner.describe()
    }

    fn forwards(&self) -> Option<ikigai_core::Forward> {
        self.inner.forwards()
    }

    fn confinement(&self) -> Option<Topology> {
        self.inner.confinement()
    }
}

fn millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One request as its door knew it: the door line `ikigai-log` writes.
///
/// ```
/// use ikigai_core::{ArgRef, Capability, Iri, Request, Verb};
/// use ikigai_gonk::access::{access, Door};
///
/// let request = Request::new(Verb::Source, Iri::parse("urn:iki:gonk:k").unwrap())
///     .with_arg("c", ArgRef::Inline(b"source urn:repo:x:tree as=text/html".to_vec()));
/// let anonymous = Capability::scoped(["urn:cap:ledger:read:default"]);
/// let entry = access(1_791_201_792_345, Door::Http, &request, &anonymous, Ok(4096), 17)
///     .entry()
///     .unwrap();
/// assert_eq!(entry.class, ikigai_log::ACCESS_CLASS);
/// assert_eq!(entry.subject, "urn:iki:gonk:k");
/// for (key, value) in [
///     ("door", "http"),
///     ("verb", "source"),
///     ("outcome", "ok"),
///     ("bytes", "4096"),
///     ("dur", "17"),
///     ("principal", "anon"),
///     ("credential", ikigai_log::ANONYMOUS_CREDENTIAL),
///     ("q", "c=source%20urn:repo:x:tree%20as%3Dtext/html"),
/// ] {
///     assert_eq!(entry.get(key), Some(value), "{key}");
/// }
/// ```
///
/// A QUIC request names the client its connection authenticated, from the principal the door
/// minted into the session capability (ledger #816, #879, #1077) — never from an argument:
///
/// ```
/// use ikigai_core::{ArgRef, Capability, Iri, Request, Verb};
/// use ikigai_gonk::access::{access, Door};
///
/// let laptop = ikigai_gonk::quic::client_iri("ab12");
/// let session = Capability::scoped(["urn:cap:ledger:write:default"]).with_principal(&laptop).unwrap();
/// let request = Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
///     .with_arg("principal", ArgRef::Inline(b"urn:iki:host:gonk:client:forged".to_vec()));
/// let entry = access(0, Door::Quic, &request, &session, Ok(1), 1).entry().unwrap();
/// assert_eq!(entry.get("principal"), Some("urn:iki:host:gonk:client:ab12"));
/// assert_eq!(entry.get("credential"), Some(ikigai_log::CLIENT_CERTIFICATE_CREDENTIAL));
/// ```
pub fn access(
    at_ms: u64,
    door: Door,
    request: &Request,
    capability: &Capability,
    answer: std::result::Result<usize, &Error>,
    dur_ms: u128,
) -> Access {
    // The kind's word is core's (`Error::kind()`: `denied`, `retry-after`, …), never a table of
    // this crate's: a copy here read every variant core added later as `error` (ledger #1546,
    // #1923).
    let outcome = match answer {
        Ok(_) => "ok",
        Err(e) => e.kind(),
    };
    let mut line = Access::new(
        Timestamp::from_millis(at_ms),
        request.target.as_str(),
        door.as_str(),
        verb(request.verb),
        outcome,
        caller(door, capability),
    )
    .duration_ms(u64::try_from(dur_ms).unwrap_or(u64::MAX));
    if let Ok(bytes) = answer {
        line = line.bytes(bytes as u64);
    }
    // `q` is the HTTP door's: the other two doors' arguments are a client's protocol, not a
    // reader's query.
    if door == Door::Http {
        for (name, value) in &request.args {
            if UNLOGGED.contains(&name.as_str()) {
                continue;
            }
            let value = match value {
                ArgRef::Inline(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                ArgRef::Reference(iri) => iri.as_str().to_string(),
                ArgRef::Content(id) => id.to_string(),
            };
            line = line.argument(name.as_str(), value);
        }
    }
    line
}

fn verb(verb: Verb) -> &'static str {
    match verb {
        Verb::Source => "source",
        Verb::Sink => "sink",
        Verb::Exists => "exists",
        Verb::Delete => "delete",
        Verb::Meta => "meta",
    }
}

/// Who asked, as THIS door authenticated them — never a token, and never a value the caller
/// typed.
///
/// - **socket**: the owner ([`OWNER_PRINCIPAL`], `log:socketOwner`): the door checked the peer's
///   UID. A principal in a capability the owner carried is the caller's own choice and is not
///   read ([`crate::admit::principal`]).
/// - **QUIC**: the client the connection's certificate authenticated
///   (`urn:iki:host:gonk:client:<fingerprint>`, `log:clientCertificate`), minted into the
///   session capability by the door.
/// - **HTTP**: the signed-in passkey's IRI (`log:passkey`), minted from the session cookie.
///   Anyone else is [`Caller::Anonymous`] — on a read as on a write, so "nobody said who" is
///   in the record rather than absent from it.
///
/// ⚠ Audit round 4 (ledger #864, R6): this once read the `principal` ARGUMENT, which on a read
/// is whatever the caller put in the query string. It reads the capability, which nothing a
/// caller sends reaches.
fn caller(door: Door, capability: &Capability) -> Caller {
    let (iri, credential) = match door {
        Door::Socket => (Some(OWNER_PRINCIPAL.to_string()), Credential::SocketOwner),
        Door::Quic => (
            crate::admit::principal(door, capability),
            Credential::ClientCertificate,
        ),
        Door::Http => (
            crate::admit::principal(door, capability),
            Credential::Passkey,
        ),
    };
    // A principal the log cannot write as a column is written as nobody rather than not at all:
    // the request still happened. Every principal a door mints is an IRI core bounded already.
    match iri.and_then(|iri| Principal::new(iri).ok()) {
        Some(principal) => Caller::authenticated(principal, credential),
        None => Caller::Anonymous,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
        let mut request = Request::new(verb, Iri::parse(iri).expect("an IRI"));
        for (k, v) in args {
            request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
        }
        request
    }

    /// Polls of `subject` every `every` for `span`, from `start`: how many quiet lines `log`
    /// writes.
    fn quiet_lines(
        log: &AccessLog,
        subject: &str,
        dur_ms: u128,
        every: Duration,
        span: Duration,
    ) -> usize {
        let start = Instant::now();
        let polls = (span.as_millis() / every.as_millis()) as u32;
        (0..polls)
            .filter(|n| log.writes_at(subject, true, dur_ms, start + every * *n))
            .count()
    }

    /// Ledger #767, #1718: a polled subject writes its first quiet poll and then one per
    /// [`POLL_SAMPLE_EVERY`], whatever its interval; every failure and every slow one; any other
    /// subject writes every request.
    #[test]
    fn a_poll_is_sampled_and_nothing_else_is() {
        const BADGE: &str = "urn:iki:gonk:fragment:queue-depth";
        let log = AccessLog::to(Door::Http, Arc::new(|_: &str| {})).sampling_polls([BADGE]);
        let twenty = POLL_SAMPLE_EVERY * 2;
        assert_eq!(
            quiet_lines(&log, BADGE, 3, Duration::from_secs(10), twenty),
            2,
            "two quiet lines in twenty minutes of a ten-second poll"
        );
        assert!(
            log.writes(BADGE, false, 3),
            "a failed poll is always written"
        );
        assert!(
            log.writes(BADGE, true, POLL_SLOW_MS),
            "a slow poll is always written"
        );
        assert!(
            (0..200).all(|_| log.writes("urn:iki:gonk:page:home", true, 3)),
            "a page is never sampled"
        );
        // Without `sampling_polls`, nothing is sampled — the badge included.
        let plain = AccessLog::to(Door::Http, Arc::new(|_: &str| {}));
        assert!((0..200).all(|_| plain.writes(BADGE, true, 3)));
    }

    /// Ledger #1718, on the real clock: sixty-one polls of the Layers page in a row write ONE
    /// quiet line (a count of one in 60 wrote two), and they do not spend the Queue badge's
    /// sample, whose first poll is still written (a count shared by every subject skipped it).
    #[test]
    fn a_busy_poll_does_not_spend_another_subjects_sample() {
        let log = AccessLog::to(Door::Http, Arc::new(|_: &str| {})).gonk_polls();
        let quiet = (0..61)
            .filter(|_| log.writes(crate::layers_view::WAIT_IRI, true, 3))
            .count();
        assert_eq!(quiet, 1, "one quiet line in a burst of 61 polls");
        assert!(
            log.writes(crate::queue::BADGE_IRI, true, 3),
            "the badge's first poll is its own"
        );
    }

    /// ★ Ledger #1718: the sample is by TIME and per SUBJECT. The count it replaced (one in 60
    /// polls, across every subject) wrote a three-second poll's quiet line every three minutes,
    /// and let a busy subject spend a quiet one's sample: here a ten-second badge beside a
    /// three-second page poll each write one line per period, the first included.
    #[test]
    fn the_sample_is_per_subject_and_by_time() {
        let log = AccessLog::to(Door::Http, Arc::new(|_: &str| {})).gonk_polls();
        let wait = crate::layers_view::WAIT_IRI;
        let badge = crate::queue::BADGE_IRI;
        let thirty = POLL_SAMPLE_EVERY * 3;
        assert_eq!(
            quiet_lines(&log, wait, 3, Duration::from_secs(3), thirty),
            3,
            "a three-second poll writes once per period, not once per 60 polls"
        );
        // The busy subject did not spend the other's sample: its first poll is written.
        assert!(log.writes_at(badge, true, 3, Instant::now()));
        assert!(!log.writes_at(badge, true, 3, Instant::now()));
        assert!(log.writes_at(
            badge,
            true,
            3,
            Instant::now() + POLL_SAMPLE_EVERY + Duration::from_secs(1)
        ));
    }

    /// Ledger #1694: a long poll's 45-second wait is not slowness, so it is sampled by time
    /// alone; a failed one is still written. Named as a short poll it would not have helped: its
    /// every answer is "slow". (gonk serves no long poll since ledger #1705; the rule stays for
    /// a host that does.)
    #[test]
    fn a_long_poll_is_sampled_whatever_it_waited() {
        let wait = "urn:example:long-poll";
        let short_only = AccessLog::to(Door::Http, Arc::new(|_: &str| {})).sampling_polls([wait]);
        assert!(
            (0..60).all(|_| short_only.writes(wait, true, 45_000)),
            "as a short poll, every 45-second wait is written"
        );
        let log = AccessLog::to(Door::Http, Arc::new(|_: &str| {}))
            .sampling_polls([crate::queue::BADGE_IRI])
            .sampling_long_polls([wait]);
        assert_eq!(
            quiet_lines(
                &log,
                wait,
                45_000,
                Duration::from_secs(45),
                POLL_SAMPLE_EVERY * 2
            ),
            2,
            "two lines in twenty minutes of long polls"
        );
        assert!(log.writes(wait, false, 45_000), "a failed one is written");
        assert!(log.writes(wait, false, 3), "a refused one is written");
        // The short polls keep their slow rule beside it.
        assert!(log.writes(crate::queue::BADGE_IRI, true, POLL_SLOW_MS));
        assert!((0..60).all(|_| log.writes("urn:iki:gonk:page:layers", true, 45_000)));
    }

    /// Ledger #1705: an open Layers page's poll answers at once, so it is sampled as the short
    /// poll it is: a slow answer is a symptom again, and is written.
    #[test]
    fn the_layers_page_poll_is_a_short_poll() {
        let log = AccessLog::to(Door::Http, Arc::new(|_: &str| {})).gonk_polls();
        for subject in [
            crate::queue::BADGE_IRI,
            crate::layers_view::COUNT_IRI,
            crate::layers_view::WAIT_IRI,
        ] {
            assert!(
                log.writes(subject, true, POLL_SLOW_MS),
                "{subject}: slow is written"
            );
            assert!(
                log.writes(subject, false, 3),
                "{subject}: a failure is written"
            );
        }
    }

    /// The door line for one answered request, as the helper builds it.
    fn entry(
        door: Door,
        request: &Request,
        capability: &Capability,
        answer: std::result::Result<usize, &Error>,
        dur_ms: u128,
    ) -> Entry {
        access(0, door, request, capability, answer, dur_ms)
            .entry()
            .expect("a writable door line")
    }

    #[test]
    fn a_write_never_logs_its_body_or_the_transports_stamps() {
        let r = request(
            Verb::Sink,
            "urn:iki:gonk:act",
            &[
                ("content", "_ledger=default&secret=x"),
                ("content-type", "application/x-www-form-urlencoded"),
                ("received", "2026-10-05T00:00:00Z"),
                ("client", "127.0.0.1"),
            ],
        );
        let e = entry(Door::Http, &r, &nobody(), Ok(2), 1);
        assert_eq!(e.get("principal"), Some("anon"));
        assert_eq!(e.get("q"), None, "{e:?}");
        assert!(!format!("{e:?}").contains("secret") && !format!("{e:?}").contains("127.0.0.1"));
    }

    /// A capability with no principal: an anonymous HTTP caller's.
    fn nobody() -> Capability {
        Capability::scoped(["urn:cap:ledger:read:default"])
    }

    /// ★ Ledger #383: every door names who its request was from, as THAT door authenticated
    /// them, and how; an anonymous caller is named `anon`, on a read as on a write — never
    /// absent — and a principal argument a caller sent is never read.
    #[test]
    fn each_door_names_the_caller_it_authenticated() {
        let brian = nobody().with_principal("urn:iki:gonk:passkey:abc").unwrap();
        let laptop = nobody()
            .with_principal(crate::quic::client_iri("ab12").as_str())
            .unwrap();
        let w = request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("principal", "urn:iki:gonk:passkey:forged")],
        );
        let r = request(Verb::Source, "urn:iki:ledger:items", &[]);
        let who = |door, cap: &Capability, req: &Request| {
            let e = entry(door, req, cap, Ok(0), 0);
            (
                e.get("principal").unwrap().to_string(),
                e.get("credential").unwrap().to_string(),
            )
        };
        let passkey = (
            "urn:iki:gonk:passkey:abc".to_string(),
            ikigai_log::PASSKEY_CREDENTIAL.to_string(),
        );
        let anon = (
            "anon".to_string(),
            ikigai_log::ANONYMOUS_CREDENTIAL.to_string(),
        );
        assert_eq!(who(Door::Http, &brian, &w), passkey);
        assert_eq!(who(Door::Http, &brian, &r), passkey);
        assert_eq!(who(Door::Http, &nobody(), &w), anon);
        assert_eq!(
            who(Door::Http, &nobody(), &r),
            anon,
            "an anonymous READ is named too"
        );
        assert_eq!(
            who(Door::Quic, &laptop, &r),
            (
                "urn:iki:host:gonk:client:ab12".to_string(),
                ikigai_log::CLIENT_CERTIFICATE_CREDENTIAL.to_string()
            )
        );
        assert_eq!(who(Door::Quic, &nobody(), &r), anon);
        // The socket's caller is the owner, whatever its capability names.
        assert_eq!(
            who(Door::Socket, &brian, &r),
            (
                OWNER_PRINCIPAL.to_string(),
                ikigai_log::SOCKET_OWNER_CREDENTIAL.to_string()
            )
        );
    }

    #[test]
    fn a_long_field_is_cut_and_marked() {
        let long = "x".repeat(MAX_FIELD * 2);
        let r = request(Verb::Source, "urn:iki:gonk:sparql", &[("query", &long)]);
        let e = entry(Door::Http, &r, &nobody(), Ok(0), 0);
        let q = e.get("q").expect("a q column");
        assert!(q.ends_with(ikigai_log::CUT_MARKER), "{q}");
        assert!(q.len() <= MAX_FIELD + ikigai_log::CUT_MARKER.len(), "{q}");
    }

    #[test]
    fn an_error_is_its_kind_and_carries_no_bytes() {
        let r = request(Verb::Source, "urn:iki:ledger:default:item:9", &[]);
        let denied = Error::Denied("no".to_string());
        let e = entry(Door::Socket, &r, &nobody(), Err(&denied), 4);
        assert_eq!(e.get("outcome"), Some("denied"));
        assert_eq!(e.get("bytes"), None);
        assert_eq!(e.get("dur"), Some("4"));
    }

    /// ★ A rate-limited run is attributed as what it is (ledger #1216 part 1, #1923):
    /// `outcome=retry-after`, core's word for the kind.
    #[test]
    fn a_rate_limited_run_is_attributed_retry_after() {
        let r = request(Verb::Source, "urn:plan:eval", &[]);
        for (e, word) in [
            (
                Error::RetryAfter {
                    delay: Some(std::time::Duration::from_secs(42)),
                    detail: "rate-limited".into(),
                },
                "retry-after",
            ),
            (Error::OverBudget { detail: "x".into() }, "over-budget"),
            (Error::Refused("x".into()), "refused"),
            (
                Error::Unsupported {
                    verb: Some(Verb::Sink),
                    detail: "x".into(),
                },
                "unsupported",
            ),
        ] {
            assert_eq!(
                entry(Door::Http, &r, &nobody(), Err(&e), 1).get("outcome"),
                Some(word)
            );
        }
    }

    /// The line lands in the segment as one line of bare columns, in the helper's order.
    #[test]
    fn every_line_is_one_line_of_bare_columns() {
        let (lines, sink) = {
            let lines = Arc::new(Mutex::new(Vec::<String>::new()));
            let keep = Arc::clone(&lines);
            let sink: Sink = Arc::new(move |l: &str| keep.lock().unwrap().push(l.to_string()));
            (lines, sink)
        };
        let log = AccessLog::to(Door::Http, sink);
        let r = request(
            Verb::Source,
            "urn:iki:gonk:k",
            &[
                ("c", "source urn:repo:x:file:a b.rs\nnext"),
                ("text", "q=\"x\"&y"),
            ],
        );
        let e = access(0, Door::Http, &r, &nobody(), Ok(1), 1);
        log.record.access(&e);
        let lines = lines.lock().unwrap();
        let l = lines
            .iter()
            .find(|l| l.contains(" log:Access "))
            .expect("a door line");
        assert!(!l.contains('\n') && !l.contains('"'), "{l}");
        let keys: Vec<&str> = l.split(' ').collect::<Vec<_>>()[3..]
            .iter()
            .map(|c| c.split_once('=').expect("key=value").0)
            .collect();
        assert_eq!(
            keys,
            [
                "seq",
                "door",
                "verb",
                "outcome",
                "bytes",
                "dur",
                "principal",
                "credential",
                "q"
            ]
        );
    }
}
