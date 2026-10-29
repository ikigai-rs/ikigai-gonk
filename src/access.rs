//! The access log: one line per request a door is asked, on stderr (ledger
//! [#739](http://localhost:1060/l/default/item/739)).
//!
//! Brian's demo on 2026-10-03 slowed down for the better part of two hours and
//! `/tmp/ikigai-gonk.log` had no line after startup, so how slow, and which pages, was
//! unknowable afterwards. Each door now writes one line per request it serves:
//!
//! ```text
//! 2026-10-05T14:03:12.345Z gonk:Access urn:iki:gonk:page:queue door=http verb=source outcome=ok bytes=48213 dur=231 principal=- q=-
//! 2026-10-05T14:03:12.410Z gonk:Access urn:iki:ledger:items door=socket verb=source outcome=ok bytes=5120 dur=3 principal=owner q=-
//! ```
//!
//! ★ **The grammar is `ikigai-log`'s**: `<RFC 3339 ms Z> <class CURIE> <subject IRI>
//! key=value…`, so a later arc can turn this stream into a segment (ledger
//! [#383](http://localhost:1060/l/default/item/383)) by declaring `gonk:Access` and the keys,
//! not by parsing something else. `dur` and `principal` are the key names `ikigai-log`'s term
//! table already binds (`log:durationMs`, `log:onBehalfOf`). Every line carries every key in
//! this order; a value the door does not have is `-`. The time is when the request STARTED,
//! so a slow request's line, written when it ends, still says when it began.
//!
//! | key | what |
//! |---|---|
//! | subject | the IRI the door resolved — for HTTP, what the route mapped the path to |
//! | `door` | `http`, `socket` or `quic` |
//! | `verb` | `source`, `sink`, `exists`, `delete`, `meta` |
//! | `outcome` | `ok`, or the kernel's error kind (`denied`, `not-found`, `invalid-argument`, …) |
//! | `bytes` | the representation's body length; `-` on an error |
//! | `dur` | milliseconds, from the door's issue to its answer |
//! | `principal` | `owner` on the socket; a signed-in passkey's IRI on any HTTP request, `anon` on an anonymous write; the client's `urn:iki:host:gonk:client:<fingerprint>` on QUIC (`urn:iki:gonk:client:<fingerprint>` in lines written before 2026-10-10, which are never rewritten); `-` where the door names nobody |
//! | `q` | HTTP only: the request's arguments AS SERVED, percent-encoded; `-` when none. The caller's query, plus every declared default the kernel filled in before dispatch (core 0.1.104+, ledger #19: a default arrives as the inline argument a caller spelling it out would send, so a ledger page's `limit=50&offset=0` is in the line whether or not the caller typed it; only the trace's `defaulted` note tells the two apart) |
//!
//! Past [`MAX_FIELD`] characters the subject and `q` are cut and end in `…` — a character
//! the encoding never writes, so a cut is unambiguous.
//!
//! **Never written:** a cookie, an `Authorization` header, a session or wire token, or a form
//! body. This layer never sees a header at all; the body of a write is the argument
//! `content`, which is skipped, as are the transport's provenance stamps (`received`,
//! `client`) — `principal` gets its own column.
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
//! - **who an anonymous request is from.** The column is the principal the DOOR minted into
//!   the request's capability ([`crate::admit::principal`]; core 0.1.93, ledger #1077): a
//!   signed-in passkey on every HTTP verb, reads included, and the client the QUIC door
//!   authenticated (`urn:iki:host:gonk:client:<fingerprint>`, ledger #816, #1183) whatever capability it
//!   carries — core's clamp keeps the session's name (ledger #879). An anonymous HTTP read is
//!   `-`, and a `?principal=` a reader typed is never taken for one (ledger #864, R6): nothing
//!   a caller sends reaches the capability.
//! - **a refusal the door kernel makes before dispatch** (a refusal the HTTP EDGE makes is
//!   written by the edge itself, [`AccessLog::refused_at_edge`], ledger #894) — a capability floor's denial, a name
//!   nothing binds (`404`), a nesting refusal — and the kernel's own `urn:kernel:*`
//!   operations and Meta answers, which no endpoint of this server's spaces handles.
//!
//! The seam that closes the first two for HTTP is an `ikigai-web` hook called once per request
//! after the response is written; the report for ledger #739 states its shape.
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
//! for as long as it is slow — on purpose: that is the incident. Rotation of the file is
//! launchd's (or `newsyslog`'s) and is not this module's to do.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, Error, Invocation, Iri, Representation, Request,
    Resolution, Result, Scope, Space, SpaceEntry, Topology, Verb,
};

/// The line's class, in `ikigai-log`'s CURIE column.
pub const CLASS: &str = "gonk:Access";

/// The longest subject or `q` written whole, in characters.
pub const MAX_FIELD: usize = 200;

/// A polled subject ([`AccessLog::sampling_polls`]) writes at most one QUIET line per this long,
/// whatever its poll's interval and however many pages ask it (ledger #1718).
pub const POLL_SAMPLE_EVERY: Duration = Duration::from_secs(600);

/// A polled subject's request that took this many milliseconds or more always writes its line.
pub const POLL_SLOW_MS: u128 = 1000;

/// The arguments a line never carries: the body of a write and the transport's own stamps.
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

/// Where a line goes: one call per line, the line without its newline.
pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

/// Lines to this process's stderr, one `write_all` per line, newline included — what
/// [`AccessLog::stderr`] writes through, and the passkey sign-in lines beside it
/// ([`crate::identity::Passkeys::auditing`]).
pub fn stderr_sink() -> Sink {
    Arc::new(|line: &str| {
        let mut out = String::with_capacity(line.len() + 1);
        out.push_str(line);
        out.push('\n');
        // A full or closed stderr must not fail the request it is describing.
        let _ = std::io::stderr().lock().write_all(out.as_bytes());
    })
}

/// One door's access log.
#[derive(Clone)]
pub struct AccessLog {
    door: Door,
    sink: Sink,
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
    /// Lines to this process's stderr — which launchd sends to `/tmp/ikigai-gonk.log`.
    ///
    /// ★ ONE `write_all` per line, newline included, on the locked handle. `eprintln!` formats
    /// straight into unbuffered stderr and can issue a write per piece, which lets two doors'
    /// lines interleave mid-line; a whole line in one write cannot.
    pub fn stderr(door: Door) -> AccessLog {
        AccessLog::to(door, stderr_sink())
    }

    /// Lines to `sink` — a test's collector, or anything else.
    pub fn to(door: Door, sink: Sink) -> AccessLog {
        AccessLog {
            door,
            sink,
            polls: Arc::default(),
        }
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
    /// [#894](http://localhost:1062/l/default/item/894)): `outcome=denied`, `bytes=-`, `dur=0`,
    /// and no principal beyond what [`line()`] writes for an empty capability (`anon` on a write,
    /// `-` on a read), because a refused request's cookie is not believed. Always written:
    /// a refusal is never sampled.
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
    /// let line = kept.lock().unwrap()[0].clone();
    /// assert!(line.ends_with(
    ///     " gonk:Access urn:iki:ledger:append door=http verb=sink outcome=denied bytes=- dur=0 principal=anon q=-"
    /// ), "{line}");
    /// ```
    pub fn refused_at_edge(&self, request: &Request, why: &str) {
        let line = line(
            millis(SystemTime::now()),
            self.door,
            request,
            &Capability::scoped(Vec::<String>::new()),
            Err(&Error::Denied(why.to_string())),
            0,
        );
        (self.sink)(&line);
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
        let dur_ms = clock.elapsed().as_millis();
        if self
            .log
            .writes(inv.request.target.as_str(), answer.is_ok(), dur_ms)
        {
            let line = line(
                millis(started),
                self.log.door,
                inv.request,
                inv.capability,
                answer.as_ref().map(|r| r.bytes.len()),
                dur_ms,
            );
            (self.log.sink)(&line);
        }
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

/// One access line, without its newline.
///
/// ```
/// use ikigai_core::{ArgRef, Capability, Iri, Request, Verb};
/// use ikigai_gonk::access::{line, Door};
///
/// let request = Request::new(Verb::Source, Iri::parse("urn:iki:gonk:k").unwrap())
///     .with_arg("c", ArgRef::Inline(b"source urn:repo:x:tree as=text/html".to_vec()));
/// let anonymous = Capability::scoped(["urn:cap:ledger:read:default"]);
/// assert_eq!(
///     line(1_791_201_792_345, Door::Http, &request, &anonymous, Ok(4096), 17),
///     "2026-10-05T12:03:12.345Z gonk:Access urn:iki:gonk:k door=http verb=source \
///      outcome=ok bytes=4096 dur=17 principal=- q=c=source%20urn:repo:x:tree%20as%3Dtext/html"
/// );
/// ```
///
/// A QUIC request names the client its connection authenticated, from the principal the door
/// minted into the session capability (ledger #816, #879, #1077) — never from an argument:
///
/// ```
/// use ikigai_core::{ArgRef, Capability, Iri, Request, Verb};
/// use ikigai_gonk::access::{line, Door};
///
/// let laptop = ikigai_gonk::quic::client_iri("ab12");
/// let session = Capability::scoped(["urn:cap:ledger:write:default"]).with_principal(&laptop).unwrap();
/// let request = Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
///     .with_arg("principal", ArgRef::Inline(b"urn:iki:host:gonk:client:forged".to_vec()));
/// assert!(line(0, Door::Quic, &request, &session, Ok(1), 1)
///     .contains(" principal=urn:iki:host:gonk:client:ab12 "));
/// ```
pub fn line(
    at_ms: u64,
    door: Door,
    request: &Request,
    capability: &Capability,
    answer: std::result::Result<usize, &Error>,
    dur_ms: u128,
) -> String {
    let (outcome, bytes) = match answer {
        Ok(bytes) => ("ok", bytes.to_string()),
        // The kind's word is core's (`Error::kind()`: `denied`, `retry-after`, …), never a
        // table of this crate's: a copy here read every variant core added later as `error`,
        // so a rate-limited run was attributed `endpoint` and would have been attributed
        // nothing useful once it became `RetryAfter` (ledger #1546, #1923).
        Err(e) => (e.kind(), "-".to_string()),
    };
    format!(
        "{} {CLASS} {} door={} verb={} outcome={outcome} bytes={bytes} dur={dur_ms} principal={} q={}",
        crate::backup::stamp_iso_ms(at_ms),
        cut(request.target.as_str().to_string()),
        door.as_str(),
        verb(request.verb),
        principal(door, request, capability),
        query(door, request),
    )
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

/// Who asked, as far as this door is told — never a token, and never a value the caller
/// typed.
///
/// ⚠ Audit round 4 (ledger #864, R6): this used to read the `principal` argument on every
/// HTTP verb, and on a READ that argument is whatever the caller put in the query string —
/// `ikigai-web` reserves the provenance names on mutating verbs only (its half is
/// ikigai-cli's, filed separately). So an anonymous `GET /l/default?principal=urn:…` was
/// logged as that person. The column now comes from [`crate::admit::principal`], which takes
/// the door's own stamp and nothing else.
fn principal(door: Door, request: &Request, capability: &Capability) -> String {
    match door {
        // The socket's peer UID is checked against this process's own: it is the owner.
        Door::Socket => "owner".to_string(),
        // The client the door authenticated, minted into the session capability; `-` only on
        // a session the minter named nobody for, which this server's minter never mints.
        Door::Quic => crate::admit::principal(door, capability)
            .map(cut)
            .unwrap_or_else(|| "-".to_string()),
        Door::Http => match crate::admit::principal(door, capability) {
            // Written only when it is bare — an IRI is — so a value cannot break the line.
            Some(value) => cut(value),
            // An anonymous write; an anonymous read names nobody.
            None if request.verb.is_mutating() => "anon".to_string(),
            None => "-".to_string(),
        },
    }
}

/// The query arguments of an HTTP request, as one percent-encoded `k=v&k=v` value.
fn query(door: Door, request: &Request) -> String {
    if door != Door::Http {
        return "-".to_string();
    }
    let mut out = String::new();
    for (name, value) in &request.args {
        if UNLOGGED.contains(&name.as_str()) {
            continue;
        }
        if !out.is_empty() {
            out.push('&');
        }
        encode_into(&mut out, name);
        out.push('=');
        match value {
            ArgRef::Inline(bytes) => encode_into(&mut out, &String::from_utf8_lossy(bytes)),
            ArgRef::Reference(iri) => encode_into(&mut out, iri.as_str()),
            ArgRef::Content(id) => encode_into(&mut out, &id.to_string()),
        }
    }
    if out.is_empty() {
        "-".to_string()
    } else {
        cut(out)
    }
}

/// Percent-encode everything but what reads plainly in an IRI-ish value.
///
/// ⚠ **Deliberately NOT `ikigai_fn::html::url_segment`** (field guide, module design: a private
/// copy states why the shared one cannot serve). This is a LOG-LINE rule, not a URL rule: the
/// `q` column is read by a person grepping the access log, and an IRI argument
/// (`urn:repo:x:file:src/lib.rs`) has to stay legible there, so `:` `/` `@` `,` `*` `+` `!` `(`
/// `)` are kept. What the column needs is only that a value cannot split the line (space, tab,
/// newline) or fake another field (`=`, `&`, `%`), and every one of those is encoded. The
/// shared rule encodes the IRI's punctuation too, which would make every line read as
/// `urn%3Arepo%3Ax%3Afile%3Asrc%2Flib.rs`; and `ikigai_browse::iri_encode` keeps `&`, `=` and
/// `'`, which would let a value fake a `k=v` pair. Neither is a log format, so this one stays,
/// and the module docs' `q=c=source%20urn:repo:x:tree%20as%3Dtext/html` example pins it.
fn encode_into(out: &mut String, text: &str) {
    for b in text.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b':'
            | b'/'
            | b'@'
            | b','
            | b'*'
            | b'+'
            | b'!'
            | b'('
            | b')' => out.push(b as char),
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
}

/// Keep [`MAX_FIELD`] characters and mark the cut with `…`.
fn cut(value: String) -> String {
    match value.char_indices().nth(MAX_FIELD) {
        Some((at, _)) => format!("{}…", &value[..at]),
        None => value,
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
        let l = line(0, Door::Http, &r, &nobody(), Ok(2), 1);
        assert!(l.ends_with("principal=anon q=-"), "{l}");
        assert!(!l.contains("secret") && !l.contains("127.0.0.1"), "{l}");
    }

    /// A capability with no principal: an anonymous HTTP caller's.
    fn nobody() -> Capability {
        Capability::scoped(["urn:cap:ledger:read:default"])
    }

    #[test]
    fn a_signed_in_request_names_the_passkey_and_an_anonymous_read_names_nobody() {
        let brian = nobody().with_principal("urn:iki:gonk:passkey:abc").unwrap();
        // The capability's principal is the door's word; a stamped argument is not read.
        let w = request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("principal", "urn:iki:gonk:passkey:forged")],
        );
        assert!(line(0, Door::Http, &w, &brian, Ok(0), 0)
            .contains(" principal=urn:iki:gonk:passkey:abc "));
        assert!(line(0, Door::Http, &w, &nobody(), Ok(0), 0).contains(" principal=anon "));
        let r = request(Verb::Source, "urn:iki:ledger:items", &[]);
        // Since ledger #1077 a signed-in READ is named too: the principal is in the capability
        // on every verb, not stamped on writes only.
        assert!(line(0, Door::Http, &r, &brian, Ok(0), 0)
            .contains(" principal=urn:iki:gonk:passkey:abc "));
        assert!(line(0, Door::Http, &r, &nobody(), Ok(0), 0).contains(" principal=- "));
        assert!(line(0, Door::Socket, &r, &brian, Ok(0), 0).contains(" principal=owner "));
        assert!(line(0, Door::Quic, &r, &nobody(), Ok(0), 0).contains(" principal=- "));
    }

    #[test]
    fn a_long_field_is_cut_and_marked() {
        let long = "x".repeat(MAX_FIELD * 2);
        let r = request(Verb::Source, "urn:iki:gonk:sparql", &[("query", &long)]);
        let l = line(0, Door::Http, &r, &nobody(), Ok(0), 0);
        let q = l.split(" q=").nth(1).expect("a q column");
        assert_eq!(q.chars().count(), MAX_FIELD + 1, "{q}");
        assert!(q.ends_with('…'), "{q}");
    }

    #[test]
    fn an_error_is_its_kind_and_carries_no_bytes() {
        let r = request(Verb::Source, "urn:iki:ledger:default:item:9", &[]);
        let e = Error::Denied("no".to_string());
        let l = line(0, Door::Socket, &r, &nobody(), Err(&e), 4);
        assert!(l.contains(" outcome=denied bytes=- dur=4 "), "{l}");
    }

    /// ★ A rate-limited run is attributed as what it is (ledger #1216 part 1, #1923): the
    /// window's refusal is `RetryAfter` since ikigai-throttle 0.2.0, and the line says
    /// `outcome=retry-after`, core's word for the kind — not `endpoint` (what the 0.1.11
    /// refusal was) and not the `error` a local table fell to for a kind it did not know.
    #[test]
    fn a_rate_limited_run_is_attributed_retry_after() {
        let r = request(Verb::Source, "urn:plan:eval", &[]);
        let e = Error::RetryAfter {
            delay: Some(std::time::Duration::from_secs(42)),
            detail: "rate-limited: `urn:iki:gonk:anonymous:runs` is capped at 30 per 60s".into(),
        };
        let l = line(0, Door::Http, &r, &nobody(), Err(&e), 1);
        assert!(l.contains(" outcome=retry-after bytes=- dur=1 "), "{l}");
        for (e, word) in [
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
            let l = line(0, Door::Http, &r, &nobody(), Err(&e), 1);
            assert!(l.contains(&format!(" outcome={word} ")), "{l}");
        }
    }

    #[test]
    fn every_line_is_one_line_of_bare_columns() {
        let r = request(
            Verb::Source,
            "urn:iki:gonk:k",
            &[
                ("c", "source urn:repo:x:file:a b.rs\nnext"),
                ("text", "q=\"x\"&y"),
            ],
        );
        let l = line(0, Door::Http, &r, &nobody(), Ok(1), 1);
        assert!(!l.contains('\n') && !l.contains('"'), "{l}");
        let columns: Vec<&str> = l.split(' ').collect();
        assert_eq!(columns.len(), 10, "{l}");
        let keys: Vec<&str> = columns[3..]
            .iter()
            .map(|c| c.split_once('=').expect("key=value").0)
            .collect();
        assert_eq!(
            keys,
            ["door", "verb", "outcome", "bytes", "dur", "principal", "q"]
        );
    }
}
