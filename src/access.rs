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
//! | `principal` | `owner` on the socket; a signed-in passkey's IRI on an HTTP write, `anon` on an anonymous one; the client's `urn:iki:gonk:client:<fingerprint>` on QUIC; `-` where the door is not told |
//! | `q` | HTTP only: the request's query arguments, percent-encoded; `-` when none |
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
//! - **who a READ is from.** `ikigai-web` hands the principal to writes only, so an HTTP
//!   read's `principal` is `-` — and a `?principal=` a reader typed is never taken for one
//!   ([`crate::admit::principal`]; ledger #864, R6). A QUIC line names the client the door
//!   authenticated (`urn:iki:gonk:client:<fingerprint>`, ledger #816) on every verb, read from
//!   the `principal` argument `ikigai-quic` stamps from the connection's session — whatever
//!   capability the client carries (ledger #879; through ikigai-quic 0.1.40 the name rode in
//!   the capability and a narrowed client was logged `-`).
//! - **a refusal the door kernel makes before dispatch** — a capability floor's denial, a name
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
//! `grep ' dur=[0-9]\{4,\} '` finds — the reason this log exists, ledger #739), or once every
//! [`POLL_SAMPLE`] polls (one line per ten minutes per open page), so a quiet poll still shows
//! it is alive. A sampled line is an ordinary line: the grammar does not change.
//!
//! ⚠ **What this does not bound.** The rest of the log grows with what people do, which is
//! what an access log is for, and a server that is slow for a long time logs every slow poll
//! for as long as it is slow — on purpose: that is the incident. Rotation of the file is
//! launchd's (or `newsyslog`'s) and is not this module's to do.

use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Description, Endpoint, Error, Invocation, Iri, Representation, Request, Resolution,
    Result, Scope, Space, SpaceEntry, Topology, Verb,
};

/// The line's class, in `ikigai-log`'s CURIE column.
pub const CLASS: &str = "gonk:Access";

/// The longest subject or `q` written whole, in characters.
pub const MAX_FIELD: usize = 200;

/// A polled subject ([`AccessLog::sampling_polls`]) writes one line in this many — once per ten
/// minutes per open page, for the Queue badge's ten-second poll.
pub const POLL_SAMPLE: u64 = 60;

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

/// One door's access log.
#[derive(Clone)]
pub struct AccessLog {
    door: Door,
    sink: Sink,
    polls: Arc<Polls>,
}

/// The subjects a page polls on a timer, and how many of their requests this log has seen.
#[derive(Default)]
struct Polls {
    subjects: Vec<String>,
    seen: AtomicU64,
}

impl AccessLog {
    /// Lines to this process's stderr — which launchd sends to `/tmp/ikigai-gonk.log`.
    ///
    /// ★ ONE `write_all` per line, newline included, on the locked handle. `eprintln!` formats
    /// straight into unbuffered stderr and can issue a write per piece, which lets two doors'
    /// lines interleave mid-line; a whole line in one write cannot.
    pub fn stderr(door: Door) -> AccessLog {
        AccessLog::to(
            door,
            Arc::new(|line: &str| {
                let mut out = String::with_capacity(line.len() + 1);
                out.push_str(line);
                out.push('\n');
                // A full or closed stderr must not fail the request it is describing.
                let _ = std::io::stderr().lock().write_all(out.as_bytes());
            }),
        )
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
    /// [`POLL_SLOW_MS`] or more, or once every [`POLL_SAMPLE`] polls (the first included). Every
    /// other subject is unaffected. See the module docs for why (ledger #767).
    pub fn sampling_polls<S: Into<String>>(
        self,
        subjects: impl IntoIterator<Item = S>,
    ) -> AccessLog {
        AccessLog {
            polls: Arc::new(Polls {
                subjects: subjects.into_iter().map(Into::into).collect(),
                seen: AtomicU64::new(0),
            }),
            ..self
        }
    }

    /// Whether a request for `subject` that ended `ok` (or not) after `dur_ms` writes a line.
    fn writes(&self, subject: &str, ok: bool, dur_ms: u128) -> bool {
        if !self.polls.subjects.iter().any(|polled| polled == subject) {
            return true;
        }
        // Counted whether or not it is written, so the sample is one in N POLLS, not one in N
        // quiet ones.
        let nth = self.polls.seen.fetch_add(1, Ordering::Relaxed);
        !ok || dur_ms >= POLL_SLOW_MS || nth.is_multiple_of(POLL_SAMPLE)
    }

    /// The door this log describes.
    pub fn door(&self) -> Door {
        self.door
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
/// use ikigai_core::{ArgRef, Iri, Request, Verb};
/// use ikigai_gonk::access::{line, Door};
///
/// let request = Request::new(Verb::Source, Iri::parse("urn:iki:gonk:k").unwrap())
///     .with_arg("c", ArgRef::Inline(b"source urn:repo:x:tree as=text/html".to_vec()));
/// assert_eq!(
///     line(1_791_201_792_345, Door::Http, &request, Ok(4096), 17),
///     "2026-10-05T12:03:12.345Z gonk:Access urn:iki:gonk:k door=http verb=source \
///      outcome=ok bytes=4096 dur=17 principal=- q=c=source%20urn:repo:x:tree%20as%3Dtext/html"
/// );
/// ```
///
/// A QUIC request names the client its connection authenticated, from the `principal`
/// argument the transport stamps (ledger #816, #879):
///
/// ```
/// use ikigai_core::{ArgRef, Iri, Request, Verb};
/// use ikigai_gonk::access::{line, Door};
///
/// let laptop = ikigai_gonk::quic::client_iri("ab12");
/// let request = Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
///     .with_arg("principal", ArgRef::Inline(laptop.into_bytes()));
/// assert!(line(0, Door::Quic, &request, Ok(1), 1)
///     .contains(" principal=urn:iki:gonk:client:ab12 "));
/// ```
pub fn line(
    at_ms: u64,
    door: Door,
    request: &Request,
    answer: std::result::Result<usize, &Error>,
    dur_ms: u128,
) -> String {
    let (outcome, bytes) = match answer {
        Ok(bytes) => ("ok", bytes.to_string()),
        Err(e) => (outcome(e), "-".to_string()),
    };
    format!(
        "{} {CLASS} {} door={} verb={} outcome={outcome} bytes={bytes} dur={dur_ms} principal={} q={}",
        crate::backup::stamp_iso_ms(at_ms),
        cut(request.target.as_str().to_string()),
        door.as_str(),
        verb(request.verb),
        principal(door, request),
        query(door, request),
    )
}

/// The error kind, as a word.
fn outcome(error: &Error) -> &'static str {
    match error {
        Error::Unresolved(_) => "unresolved",
        Error::MissingArgument(_) => "missing-argument",
        Error::InvalidArgument { .. } => "invalid-argument",
        Error::Endpoint(_) => "endpoint",
        Error::Denied(_) => "denied",
        Error::NotFound(_) => "not-found",
        Error::Conflict(_) => "conflict",
        Error::Timeout(_) => "timeout",
        Error::Unavailable(_) => "unavailable",
        Error::DepthExceeded { .. } => "depth-exceeded",
        // `Error` is non-exhaustive: a kind core adds later is still an error, not a crash.
        _ => "error",
    }
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
fn principal(door: Door, request: &Request) -> String {
    match door {
        // The socket's peer UID is checked against this process's own: it is the owner.
        Door::Socket => "owner".to_string(),
        // The client the door authenticated, stamped by the transport; `-` only on a session
        // the minter named nobody for, which this server's minter never mints.
        Door::Quic => crate::admit::principal(door, request)
            .map(cut)
            .unwrap_or_else(|| "-".to_string()),
        Door::Http => match crate::admit::principal(door, request) {
            // Written only when it is bare — an IRI is — so a value cannot break the line.
            Some(value) => cut(value),
            // A write with no principal is an anonymous one; a read is never told.
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

    /// Ledger #767: a polled subject writes the first poll and one in [`POLL_SAMPLE`] after it,
    /// every failure and every slow one; any other subject writes every request.
    #[test]
    fn a_poll_is_sampled_and_nothing_else_is() {
        const BADGE: &str = "urn:iki:gonk:fragment:queue-depth";
        let log = AccessLog::to(Door::Http, Arc::new(|_: &str| {})).sampling_polls([BADGE]);
        let quiet = (0..POLL_SAMPLE * 2)
            .filter(|_| log.writes(BADGE, true, 3))
            .count();
        assert_eq!(quiet, 2, "two quiet lines in {} polls", POLL_SAMPLE * 2);
        assert!(
            log.writes(BADGE, false, 3),
            "a failed poll is always written"
        );
        assert!(
            log.writes(BADGE, true, POLL_SLOW_MS),
            "a slow poll is always written"
        );
        assert!(
            (0..POLL_SAMPLE * 2).all(|_| log.writes("urn:iki:gonk:page:home", true, 3)),
            "a page is never sampled"
        );
        // Without `sampling_polls`, nothing is sampled — the badge included.
        let plain = AccessLog::to(Door::Http, Arc::new(|_: &str| {}));
        assert!((0..POLL_SAMPLE * 2).all(|_| plain.writes(BADGE, true, 3)));
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
        let l = line(0, Door::Http, &r, Ok(2), 1);
        assert!(l.ends_with("principal=anon q=-"), "{l}");
        assert!(!l.contains("secret") && !l.contains("127.0.0.1"), "{l}");
    }

    #[test]
    fn a_signed_in_write_names_the_passkey_and_a_read_names_nobody() {
        let r = request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("principal", "urn:iki:gonk:passkey:abc")],
        );
        assert!(line(0, Door::Http, &r, Ok(0), 0).contains(" principal=urn:iki:gonk:passkey:abc "));
        let r = request(Verb::Source, "urn:iki:ledger:items", &[]);
        assert!(line(0, Door::Http, &r, Ok(0), 0).contains(" principal=- "));
        assert!(line(0, Door::Socket, &r, Ok(0), 0).contains(" principal=owner "));
        assert!(line(0, Door::Quic, &r, Ok(0), 0).contains(" principal=- "));
    }

    #[test]
    fn a_long_field_is_cut_and_marked() {
        let long = "x".repeat(MAX_FIELD * 2);
        let r = request(Verb::Source, "urn:iki:gonk:sparql", &[("query", &long)]);
        let l = line(0, Door::Http, &r, Ok(0), 0);
        let q = l.split(" q=").nth(1).expect("a q column");
        assert_eq!(q.chars().count(), MAX_FIELD + 1, "{q}");
        assert!(q.ends_with('…'), "{q}");
    }

    #[test]
    fn an_error_is_its_kind_and_carries_no_bytes() {
        let r = request(Verb::Source, "urn:iki:ledger:default:item:9", &[]);
        let e = Error::Denied("no".to_string());
        let l = line(0, Door::Socket, &r, Err(&e), 4);
        assert!(l.contains(" outcome=denied bytes=- dur=4 "), "{l}");
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
        let l = line(0, Door::Http, &r, Ok(1), 1);
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
