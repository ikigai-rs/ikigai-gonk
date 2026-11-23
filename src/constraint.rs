//! The constraint sampler: `urn:kernel:constraint` read on a timer and written into gonk's
//! `ikigai-log` segment, one entry per reading (ledger #4094).
//!
//! The kernel ranks the resources that spent the most uncached compute over its last 512
//! resolutions, top five only, in memory: a restart empties it and nothing records it over
//! time. This is build step 1 of ikigai-core's constraint-history design
//! (`docs/design/constraint-history.md`, sections 3a, 4, 5 and 6): a host-side sampler over
//! that window, with no core change. Every entry says `sampleSource=window`, because a
//! window reading is a frequency over sampled moments, not a total: it sees a resource only
//! if that resource is in the top five of the last 512 resolutions when the sampler reads.
//!
//! # Configuration (the config home, off by default)
//!
//! ```toml
//! gonk.constraint.sample = true          # off unless `true`; anything but true/false fails
//! # gonk.constraint.every = "15m"        # the cadence (this IS the default; one minute or more)
//! # gonk.constraint.host = "gonk"        # the host's configured name (this IS the default):
//!                                        # letters, digits, `.`, `_`, `-`; never a user name
//! # gonk.constraint.deny = "user-inbox"  # an endpoint id whose rows are dropped; repeatable
//! ```
//!
//! A malformed value stops the server at start, as every `gonk.` key does.
//!
//! # One entry per reading
//!
//! Each reading (every [`Settings::every`], and once at shutdown on SIGTERM or SIGINT) writes
//! one `ik:ConstraintSample` entry, written even when no row survives, so a quiet window is in
//! the record too. The subject is the sample's skolem IRI (section 4, no blank nodes):
//! `urn:ikigai:constraint:sample:{host}:{started}:{until}`, `{host}` encoded as a name and
//! the two times in milliseconds. A row is `{sample}:row:{n}`, `n` its window rank.
//!
//! An `ikigai-log` key cannot hold a `:`, so the columns spell each term's local name; the
//! mapping to the vocabulary is one-to-one:
//!
//! | column | term | what |
//! |---|---|---|
//! | `sampledHost` | `ik:sampledHost` | [`Settings::host`] |
//! | `sampleSource` | `ik:sampleSource` | always `window` |
//! | `kernelStarted` | `ik:kernelStarted` | the sampler's first reading (the window carries no start) |
//! | `sampleSince` | `ik:sampleSince` | the previous reading's time; absent on the first |
//! | `sampleUntil` | `ik:sampleUntil` | this reading's time |
//! | `sampleEvery` | `ik:sampleEvery` | the cadence, in milliseconds |
//! | `windowResolutions` | `ik:windowResolutions` | how many resolutions the window held |
//! | `reading` | `ik:sampleReading` | `tick`, or `shutdown` for the last one |
//! | `droppedDenied` | `ik:droppedDenied` | rows dropped by `gonk.constraint.deny` |
//! | `droppedUnshaped` | `ik:droppedUnshaped` | rows dropped because no shape was reachable |
//! | `row.{n}` | `ik:sampleRow` | the row's IRI |
//! | `row.{n}.order` | `ik:order` | the window's rank, from 1 |
//! | `row.{n}.endpointId` | `ik:endpointId` | the endpoint's `describe().id` |
//! | `row.{n}.targetShape` | `ik:targetShape` | see Privacy |
//! | `row.{n}.uncachedCallCount` | `ik:uncachedCallCount` | computed resolutions in the window |
//! | `row.{n}.cachedCallCount` | `ik:cachedCallCount` | cache hits in the window |
//! | `row.{n}.uncachedMillis` | `ik:uncachedMillis` | absent on a clockless kernel |
//!
//! `ik:order`, `ik:uncachedCallCount`, `ik:cachedCallCount`, `ik:uncachedMillis` and
//! `ik:windowResolutions` exist in the vocabulary today. The rest are PROPOSED: the note's
//! `ik:ConstraintSample`, `ik:sampledHost`, `ik:sampleSource`, `ik:kernelStarted`,
//! `ik:sampleSince`, `ik:sampleUntil`, `ik:sampleRow`, `ik:endpointId` and `ik:targetShape`,
//! and this sampler's own `ik:sampleEvery`, `ik:sampleReading`, `ik:droppedDenied` and
//! `ik:droppedUnshaped`.
//!
//! # Privacy (section 5)
//!
//! The window holds each target's full IRI, and a template target's IRI carries its
//! argument values (`urn:file:/home/alice/notes.md`). That IRI is never written. The row
//! records the endpoint id and the target's SHAPE, found in the hub's own binding table:
//!
//! - a target bound EXACTLY (it is itself a bound pattern) is recorded as it is bound,
//!   because a host author wrote it into the binding table;
//! - a target that no exact pattern names is recorded as the ONE template pattern bound to
//!   its endpoint (`urn:file:{path}`): the shape is the pattern, so it holds variable names
//!   and no value;
//! - anything else (two templates on one endpoint, a prefix binding, a target the hub no
//!   longer resolves) has no reachable shape and its row is DROPPED and counted
//!   (`droppedUnshaped`). The sampler never guesses a shape from the IRI.
//!
//! Nothing is hashed: a hash of a guessable value is reversible by a dictionary. A row whose
//! endpoint id is listed in `gonk.constraint.deny` is dropped and counted (`droppedDenied`),
//! for an exact binding whose name itself names a person.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use ikigai_core::{ArgRef, Capability, Clock, Iri, Request, Verb};
use ikigai_log::{Entry, Timestamp};

/// Whether the sampler runs: `true` or `false`, off when absent.
pub const SAMPLE_KEY: &str = "gonk.constraint.sample";
/// The cadence, a duration like `15m`.
pub const EVERY_KEY: &str = "gonk.constraint.every";
/// The host's configured name.
pub const HOST_KEY: &str = "gonk.constraint.host";
/// An endpoint id whose rows are dropped; repeatable.
pub const DENY_KEY: &str = "gonk.constraint.deny";

/// The cadence when no `gonk.constraint.every` line says otherwise (section 6).
pub const DEFAULT_EVERY: Duration = Duration::from_secs(15 * 60);
/// The shortest cadence accepted.
pub const MIN_EVERY: Duration = Duration::from_secs(60);
/// The host's name when no `gonk.constraint.host` line names one.
pub const DEFAULT_HOST: &str = "gonk";
/// The longest host name accepted.
pub const MAX_HOST: usize = 64;

/// What the sampler reads.
pub const WINDOW: &str = "urn:kernel:constraint";
/// The capability the window needs, and the only scope the sampler holds.
pub const CAP_INSPECT: &str = crate::operator::CAP_INSPECT;
/// Who reads: the sampler's own principal, never a person.
pub const PRINCIPAL: &str = "urn:iki:gonk:recorder:constraint";
/// The entry's class (proposed in the note, section 4).
pub const CLASS: &str = "https://ikigai-rs.dev/ns#ConstraintSample";
/// The sample IRI's prefix (section 4).
pub const SAMPLE_PREFIX: &str = "urn:ikigai:constraint:sample:";
/// Every entry's `sampleSource`: this sampler reads the window, not totals.
pub const SOURCE: &str = "window";

const IK: &str = "https://ikigai-rs.dev/ns#";

/// The sampler's settings, present only when `gonk.constraint.sample = true`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// How often a reading is taken.
    pub every: Duration,
    /// The host's configured name, `sampledHost`.
    pub host: String,
    /// Endpoint ids whose rows are dropped.
    pub deny: BTreeSet<String>,
}

/// The settings from the four keys. Every value is checked whether or not the sampler is
/// on, so a malformed line fails the start rather than waiting for the day it is turned on.
///
/// ```
/// use ikigai_gonk::constraint::settings;
/// assert_eq!(settings(None, None, None, &[]), Ok(None));
/// let on = settings(Some("true"), Some("30m"), Some("plasma-gonk"), &["inbox".into()])
///     .unwrap()
///     .unwrap();
/// assert_eq!(on.every.as_secs(), 1800);
/// assert_eq!(on.host, "plasma-gonk");
/// assert!(settings(Some("yes"), None, None, &[]).is_err());
/// assert!(settings(Some("true"), Some("soon"), None, &[]).is_err());
/// assert!(settings(Some("true"), None, Some("brian@laptop"), &[]).is_err());
/// ```
pub fn settings(
    sample: Option<&str>,
    every: Option<&str>,
    host: Option<&str>,
    deny: &[String],
) -> std::result::Result<Option<Settings>, String> {
    let on = match sample.map(str::trim) {
        None | Some("false") => false,
        Some("true") => true,
        Some(other) => {
            return Err(format!(
                "{SAMPLE_KEY} = `{other}` is neither `true` nor `false`"
            ))
        }
    };
    let every = match every.map(str::trim) {
        None => DEFAULT_EVERY,
        Some(spelled) => {
            let schedule = ikigai_time::parse_schedule(spelled)
                .map_err(|e| format!("{EVERY_KEY}: {e} — a duration like `15m` or `1h`"))?;
            let interval = schedule.interval();
            if interval < MIN_EVERY {
                return Err(format!("{EVERY_KEY}: `{spelled}` is under a minute"));
            }
            interval
        }
    };
    let host = match host.map(str::trim) {
        None => DEFAULT_HOST.to_string(),
        Some(name) => {
            let ok = !name.is_empty()
                && name.len() <= MAX_HOST
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            if !ok {
                return Err(format!(
                    "{HOST_KEY} = `{name}`: the host's configured name is 1 to {MAX_HOST} \
                     letters, digits, `.`, `_` or `-` (a host's name, never a user's)"
                ));
            }
            name.to_string()
        }
    };
    let mut denied = BTreeSet::new();
    for line in deny {
        let id = line.trim();
        if id.is_empty() || id.chars().any(char::is_whitespace) {
            return Err(format!(
                "{DENY_KEY} = `{line}`: one endpoint id per line, with no spaces"
            ));
        }
        denied.insert(id.to_string());
    }
    Ok(on.then_some(Settings {
        every,
        host,
        deny: denied,
    }))
}

/// One ranked target of the window, as its Turtle face states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRow {
    /// The window's rank, from 1.
    pub order: u64,
    /// The target's full IRI. ⚠ May carry argument values: never written.
    pub target: String,
    /// Computed resolutions in the window.
    pub uncached: u64,
    /// Cache hits in the window.
    pub cached: u64,
    /// Uncached milliseconds, `None` on a clockless kernel.
    pub uncached_ms: Option<u64>,
}

/// One reading of the window: its resolutions and its ranked rows, in rank order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    /// How many resolutions the window held (`ik:windowResolutions`).
    pub resolutions: u64,
    /// The ranked targets.
    pub rows: Vec<WindowRow>,
}

/// Parse the Turtle face of `urn:kernel:constraint`.
pub fn parse_window(turtle: &[u8]) -> std::result::Result<Window, String> {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::model::Term;
    let mut resolutions = 0;
    let mut rows: BTreeMap<String, WindowRow> = BTreeMap::new();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle) {
        let quad =
            quad.map_err(|e| format!("{WINDOW} answered Turtle that does not parse: {e}"))?;
        let subject = quad.subject.to_string();
        let subject = subject.trim_start_matches('<').trim_end_matches('>');
        let Some(local) = quad.predicate.as_str().strip_prefix(IK) else {
            continue;
        };
        let (literal, iri) = match &quad.object {
            Term::Literal(l) => (Some(l.value().to_string()), None),
            Term::NamedNode(n) => (None, Some(n.as_str().to_string())),
            _ => (None, None),
        };
        let number = || -> std::result::Result<u64, String> {
            literal
                .as_deref()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{WINDOW}: ik:{local} of <{subject}> is not a count"))
        };
        if subject == WINDOW {
            if local == "windowResolutions" {
                resolutions = number()?;
            }
            continue;
        }
        let row = rows
            .entry(subject.to_string())
            .or_insert_with(|| WindowRow {
                order: 0,
                target: String::new(),
                uncached: 0,
                cached: 0,
                uncached_ms: None,
            });
        match local {
            "constraintResource" => row.target = iri.unwrap_or_default(),
            "order" => row.order = number()?,
            "uncachedCallCount" => row.uncached = number()?,
            "cachedCallCount" => row.cached = number()?,
            "uncachedMillis" => row.uncached_ms = Some(number()?),
            _ => {}
        }
    }
    let mut rows: Vec<WindowRow> = rows
        .into_values()
        .filter(|row| row.order > 0 && !row.target.is_empty())
        .collect();
    rows.sort_by_key(|row| row.order);
    Ok(Window { resolutions, rows })
}

/// A row's recordable name: the endpoint that computes it and the target's shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    /// The endpoint's `describe().id`.
    pub endpoint: String,
    /// The target with every captured value replaced by its variable's name.
    pub shape: String,
}

/// What the sampler reads through: the window, and the binding table a shape is found in.
/// The server's is [`HubKernel`]; a test injects its own.
pub trait Inspect: Send + Sync {
    /// The window's Turtle face.
    fn window(&self) -> std::result::Result<Vec<u8>, String>;
    /// The target's shape, or `None` when none is reachable (the row is then dropped).
    fn shape(&self, target: &str) -> Option<Shape>;
}

/// The hub, read under the sampler's own capability: [`CAP_INSPECT`] and nothing else.
pub struct HubKernel(pub Arc<ikigai_core::Kernel>);

impl Inspect for HubKernel {
    fn window(&self) -> std::result::Result<Vec<u8>, String> {
        let request = Request::new(Verb::Source, Iri::parse(WINDOW).map_err(|e| e.to_string())?)
            .with_arg("as", ArgRef::Inline(b"text/turtle".to_vec()));
        let capability = Capability::scoped([CAP_INSPECT.to_string()])
            .with_principal(PRINCIPAL)
            .map_err(|e| e.to_string())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|e| format!("no runtime for the reading: {e}"))?;
        runtime
            .block_on(self.0.issue(request, &capability))
            .map(|answer| answer.bytes.to_vec())
            .map_err(|e| format!("reading {WINDOW}: {e}"))
    }

    fn shape(&self, target: &str) -> Option<Shape> {
        let entries = self.0.entries().unwrap_or_default();
        let patterns: Vec<&str> = entries.iter().map(|e| e.pattern.as_str()).collect();
        let id = |pattern: &str| self.0.describe_pattern(pattern).map(|d| d.id);
        shape_in(target, &patterns, id)
    }
}

/// The shape rule over a binding table: `patterns` are the bound patterns, `id` names the
/// endpoint a pattern (or an exact target) is bound to. See the module's Privacy section.
pub fn shape_in(
    target: &str,
    patterns: &[&str],
    id: impl Fn(&str) -> Option<String>,
) -> Option<Shape> {
    if patterns.contains(&target) {
        return id(target).map(|endpoint| Shape {
            endpoint,
            shape: target.to_string(),
        });
    }
    let endpoint = id(target)?;
    let mut templates = patterns
        .iter()
        .filter(|p| p.contains('{'))
        .filter(|p| id(p).as_deref() == Some(endpoint.as_str()));
    let only = templates.next()?;
    if templates.next().is_some() {
        return None;
    }
    Some(Shape {
        endpoint,
        shape: (*only).to_string(),
    })
}

/// Why a reading was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// The cadence.
    Tick,
    /// The last one, at shutdown.
    Shutdown,
}

impl Reading {
    fn as_str(self) -> &'static str {
        match self {
            Reading::Tick => "tick",
            Reading::Shutdown => "shutdown",
        }
    }
}

/// Where an entry goes: the server's record, or a test's collector.
pub type Sink = Arc<dyn Fn(Entry) + Send + Sync>;

#[derive(Default)]
struct State {
    started: Option<u64>,
    previous: Option<u64>,
    stopped: bool,
}

/// The sampler: reads, maps, writes.
pub struct Sampler {
    settings: Settings,
    kernel: Arc<dyn Inspect>,
    clock: Arc<dyn Clock>,
    sink: Sink,
    state: Mutex<State>,
    wake: Condvar,
}

impl Sampler {
    /// A sampler over `kernel`, timed by `clock`, writing to `sink`.
    pub fn new(
        settings: Settings,
        kernel: Arc<dyn Inspect>,
        clock: Arc<dyn Clock>,
        sink: Sink,
    ) -> Arc<Sampler> {
        Arc::new(Sampler {
            settings,
            kernel,
            clock,
            sink,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        })
    }

    /// The server's sampler: the hub, the system clock, gonk's record.
    pub fn over_hub(
        settings: Settings,
        hub: Arc<ikigai_core::Kernel>,
        record: Arc<crate::access::Record>,
    ) -> Arc<Sampler> {
        Sampler::new(
            settings,
            Arc::new(HubKernel(hub)),
            Arc::new(ikigai_core::SystemClock),
            Arc::new(move |entry| record.entry(entry)),
        )
    }

    /// The settings it runs with.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Take one reading and write its entry. A reading that fails writes nothing and says
    /// why; the next one is taken on time. After [`shutdown`](Self::shutdown), nothing.
    pub fn sample(&self, reading: Reading) -> std::result::Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped {
            return Ok(());
        }
        if reading == Reading::Shutdown {
            state.stopped = true;
            self.wake.notify_all();
        }
        let entry = self.read(&mut state, reading)?;
        (self.sink)(entry);
        Ok(())
    }

    fn read(&self, state: &mut State, reading: Reading) -> std::result::Result<Entry, String> {
        let window = parse_window(&self.kernel.window()?)?;
        let mut now = self.clock.now().0;
        // Two readings in one millisecond would share an IRI.
        if let Some(previous) = state.previous {
            now = now.max(previous + 1);
        }
        let started = *state.started.get_or_insert(now);
        let since = state.previous.replace(now);
        let sample = format!(
            "{SAMPLE_PREFIX}{}:{started}:{now}",
            ikigai_core::encode_name(&self.settings.host)
        );
        let mut entry = Entry::new(Timestamp::from_millis(now), CLASS, sample.clone())
            .with("sampledHost", self.settings.host.clone())
            .with("sampleSource", SOURCE)
            .with("kernelStarted", Timestamp::from_millis(started).render());
        if let Some(since) = since {
            entry = entry.with("sampleSince", Timestamp::from_millis(since).render());
        }
        entry = entry
            .with("sampleUntil", Timestamp::from_millis(now).render())
            .with("sampleEvery", self.settings.every.as_millis().to_string())
            .with("windowResolutions", window.resolutions.to_string())
            .with("reading", reading.as_str());
        let (mut denied, mut unshaped) = (0u64, 0u64);
        let mut rows = Vec::new();
        for row in &window.rows {
            let Some(shape) = self.kernel.shape(&row.target) else {
                unshaped += 1;
                continue;
            };
            if self.settings.deny.contains(&shape.endpoint) {
                denied += 1;
                continue;
            }
            rows.push((row, shape));
        }
        entry = entry
            .with("droppedDenied", denied.to_string())
            .with("droppedUnshaped", unshaped.to_string());
        for (row, shape) in rows {
            let n = row.order;
            let key = |name: &str| format!("row.{n}.{name}");
            entry = entry
                .with(format!("row.{n}"), format!("{sample}:row:{n}"))
                .with(key("order"), n.to_string())
                .with(key("endpointId"), shape.endpoint)
                .with(key("targetShape"), shape.shape)
                .with(key("uncachedCallCount"), row.uncached.to_string())
                .with(key("cachedCallCount"), row.cached.to_string());
            if let Some(ms) = row.uncached_ms {
                entry = entry.with(key("uncachedMillis"), ms.to_string());
            }
        }
        Ok(entry)
    }

    /// Run the cadence on a thread of its own: the first reading one interval from now, then
    /// every interval, until [`shutdown`](Self::shutdown).
    pub fn start(self: &Arc<Self>) -> std::io::Result<std::thread::JoinHandle<()>> {
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("gonk-constraint-sampler".to_string())
            .spawn(move || loop {
                let state = me.state.lock().unwrap_or_else(|e| e.into_inner());
                let (state, _) = me
                    .wake
                    .wait_timeout_while(state, me.settings.every, |s| !s.stopped)
                    .unwrap_or_else(|e| e.into_inner());
                if state.stopped {
                    return;
                }
                drop(state);
                if let Err(e) = me.sample(Reading::Tick) {
                    eprintln!("ikigai-gonk: a constraint reading was not taken: {e}");
                }
            })
    }

    /// The last reading, once: stops the cadence and writes a `reading=shutdown` entry.
    pub fn shutdown(&self) {
        if let Err(e) = self.sample(Reading::Shutdown) {
            eprintln!("ikigai-gonk: the shutdown constraint reading was not taken: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikigai_core::FixedClock;

    /// A window as core's Turtle face renders it: an exact target, a template target that
    /// carries a person's path, and a denied one.
    const TURTLE: &str = "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
\n<urn:kernel:constraint> a ik:ConstraintWindow ;\n    ik:windowResolutions \"512\"^^xsd:nonNegativeInteger ;\n    \
ik:currentConstraint <urn:ikigai:constraint:target:urn%3Afile%3A%2Fhome%2Falice%2Fnotes.md> ;\n    \
ik:rankedTarget <urn:ikigai:constraint:target:urn%3Afile%3A%2Fhome%2Falice%2Fnotes.md> .\n\
\n<urn:ikigai:constraint:target:urn%3Afile%3A%2Fhome%2Falice%2Fnotes.md> a ik:ConstraintTarget ;\n    \
ik:constraintResource <urn:file:/home/alice/notes.md> ;\n    ik:order \"1\"^^xsd:integer ;\n    \
ik:callCount \"40\"^^xsd:nonNegativeInteger ;\n    ik:cachedCallCount \"10\"^^xsd:nonNegativeInteger ;\n    \
ik:uncachedCallCount \"30\"^^xsd:nonNegativeInteger ;\n    ik:cachedPercent \"25\"^^xsd:nonNegativeInteger ;\n    \
ik:uncachedMillis \"900\"^^xsd:nonNegativeInteger .\n\
\n<urn:ikigai:constraint:target:urn%3Abooks%3Agraph> a ik:ConstraintTarget ;\n    \
ik:constraintResource <urn:books:graph> ;\n    ik:order \"2\"^^xsd:integer ;\n    \
ik:callCount \"5\"^^xsd:nonNegativeInteger ;\n    ik:cachedCallCount \"0\"^^xsd:nonNegativeInteger ;\n    \
ik:uncachedCallCount \"5\"^^xsd:nonNegativeInteger ;\n    ik:cachedPercent \"0\"^^xsd:nonNegativeInteger ;\n    \
ik:uncachedMillis \"400\"^^xsd:nonNegativeInteger .\n\
\n<urn:ikigai:constraint:target:urn%3Auser%3Aalice%3Ainbox> a ik:ConstraintTarget ;\n    \
ik:constraintResource <urn:user:alice:inbox> ;\n    ik:order \"3\"^^xsd:integer ;\n    \
ik:callCount \"2\"^^xsd:nonNegativeInteger ;\n    ik:cachedCallCount \"0\"^^xsd:nonNegativeInteger ;\n    \
ik:uncachedCallCount \"2\"^^xsd:nonNegativeInteger ;\n    ik:cachedPercent \"0\"^^xsd:nonNegativeInteger ;\n    \
ik:uncachedMillis \"20\"^^xsd:nonNegativeInteger .\n\
\n<urn:ikigai:constraint:target:urn%3Asearch%3Asecret-term> a ik:ConstraintTarget ;\n    \
ik:constraintResource <urn:search:secret-term> ;\n    ik:order \"4\"^^xsd:integer ;\n    \
ik:callCount \"1\"^^xsd:nonNegativeInteger ;\n    ik:cachedCallCount \"0\"^^xsd:nonNegativeInteger ;\n    \
ik:uncachedCallCount \"1\"^^xsd:nonNegativeInteger ;\n    ik:cachedPercent \"0\"^^xsd:nonNegativeInteger ;\n    \
ik:uncachedMillis \"1\"^^xsd:nonNegativeInteger .\n";

    /// The injected kernel: the window above, and a binding table with `urn:file:{path}`
    /// (one template on `file`), two exact names, and `urn:search:` bound under two
    /// templates of one endpoint, so its shape is not reachable.
    struct Injected;

    const PATTERNS: [&str; 5] = [
        "urn:file:{path}",
        "urn:books:graph",
        "urn:user:alice:inbox",
        "urn:search:{term}",
        "urn:search:{term}:{page}",
    ];

    fn id(name: &str) -> Option<String> {
        let id = if name.starts_with("urn:file:") {
            "file"
        } else if name == "urn:books:graph" {
            "books-graph"
        } else if name == "urn:user:alice:inbox" {
            "user-inbox"
        } else if name.starts_with("urn:search:") {
            "search"
        } else {
            return None;
        };
        Some(id.to_string())
    }

    impl Inspect for Injected {
        fn window(&self) -> std::result::Result<Vec<u8>, String> {
            Ok(TURTLE.as_bytes().to_vec())
        }
        fn shape(&self, target: &str) -> Option<Shape> {
            shape_in(target, &PATTERNS, id)
        }
    }

    fn sampler(deny: &[&str], clock: Arc<dyn Clock>) -> (Arc<Sampler>, Arc<Mutex<Vec<Entry>>>) {
        let entries = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&entries);
        let settings = Settings {
            every: DEFAULT_EVERY,
            host: "plasma-gonk".to_string(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        };
        let sampler = Sampler::new(
            settings,
            Arc::new(Injected),
            clock,
            Arc::new(move |e| sink.lock().unwrap().push(e)),
        );
        (sampler, entries)
    }

    /// 2026-11-20T08:00:00Z.
    const T0: u64 = 1_795_161_600_000;

    #[test]
    fn parses_core_s_turtle_face() {
        let window = parse_window(TURTLE.as_bytes()).unwrap();
        assert_eq!(window.resolutions, 512);
        assert_eq!(window.rows.len(), 4);
        assert_eq!(window.rows[0].target, "urn:file:/home/alice/notes.md");
        assert_eq!(window.rows[0].uncached, 30);
        assert_eq!(window.rows[0].cached, 10);
        assert_eq!(window.rows[0].uncached_ms, Some(900));
        assert_eq!(window.rows[1].order, 2);
    }

    #[test]
    fn a_reading_becomes_one_entry() {
        let (sampler, entries) = sampler(&[], Arc::new(FixedClock::at(T0)));
        sampler.sample(Reading::Tick).unwrap();
        let entries = entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        let sample = format!("urn:ikigai:constraint:sample:plasma-gonk:{T0}:{T0}");
        assert_eq!(e.class, CLASS);
        assert_eq!(e.subject, sample);
        assert_eq!(e.get("sampledHost"), Some("plasma-gonk"));
        assert_eq!(e.get("sampleSource"), Some("window"));
        assert_eq!(e.get("kernelStarted"), Some("2026-11-20T08:00:00.000Z"));
        assert_eq!(e.get("sampleUntil"), Some("2026-11-20T08:00:00.000Z"));
        assert_eq!(
            e.get("sampleSince"),
            None,
            "the first reading has no previous one"
        );
        assert_eq!(e.get("sampleEvery"), Some("900000"));
        assert_eq!(e.get("windowResolutions"), Some("512"));
        assert_eq!(e.get("reading"), Some("tick"));
        // The template row: its shape, never its path.
        assert_eq!(e.get("row.1"), Some(format!("{sample}:row:1").as_str()));
        assert_eq!(e.get("row.1.endpointId"), Some("file"));
        assert_eq!(e.get("row.1.targetShape"), Some("urn:file:{path}"));
        assert_eq!(e.get("row.1.uncachedCallCount"), Some("30"));
        assert_eq!(e.get("row.1.cachedCallCount"), Some("10"));
        assert_eq!(e.get("row.1.uncachedMillis"), Some("900"));
        // The exact rows, as bound.
        assert_eq!(e.get("row.2.targetShape"), Some("urn:books:graph"));
        assert_eq!(e.get("row.3.targetShape"), Some("urn:user:alice:inbox"));
        // Two templates on `search`: no reachable shape, dropped and counted.
        assert_eq!(e.get("row.4"), None);
        assert_eq!(e.get("droppedUnshaped"), Some("1"));
        assert_eq!(e.get("droppedDenied"), Some("0"));
        // It renders as a log line: every key is one the grammar accepts.
        let line = e.render(&BTreeMap::new()).unwrap();
        assert!(!line.contains('\n'));
    }

    #[test]
    fn the_next_reading_carries_the_interval_and_the_same_start() {
        let clock = Arc::new(Mutex::new(T0));
        struct Stepped(Arc<Mutex<u64>>);
        impl Clock for Stepped {
            fn now(&self) -> ikigai_core::Time {
                ikigai_core::Time(*self.0.lock().unwrap())
            }
        }
        let (sampler, entries) = sampler(&[], Arc::new(Stepped(Arc::clone(&clock))));
        sampler.sample(Reading::Tick).unwrap();
        *clock.lock().unwrap() = T0 + 900_000;
        sampler.sample(Reading::Tick).unwrap();
        let entries = entries.lock().unwrap();
        assert_eq!(entries.len(), 2);
        let second = &entries[1];
        assert_eq!(
            second.subject,
            format!(
                "urn:ikigai:constraint:sample:plasma-gonk:{T0}:{}",
                T0 + 900_000
            )
        );
        assert_eq!(
            second.get("kernelStarted"),
            Some("2026-11-20T08:00:00.000Z")
        );
        assert_eq!(second.get("sampleSince"), Some("2026-11-20T08:00:00.000Z"));
        assert_eq!(second.get("sampleUntil"), Some("2026-11-20T08:15:00.000Z"));
    }

    #[test]
    fn the_shutdown_reading_is_written_once_and_stops_the_cadence() {
        let (sampler, entries) = sampler(&[], Arc::new(FixedClock::at(T0)));
        let thread = sampler.start().unwrap();
        sampler.shutdown();
        thread.join().unwrap();
        sampler.shutdown();
        sampler.sample(Reading::Tick).unwrap();
        let entries = entries.lock().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "one shutdown reading, and nothing after it"
        );
        assert_eq!(entries[0].get("reading"), Some("shutdown"));
        assert_eq!(entries[0].get("row.1.targetShape"), Some("urn:file:{path}"));
    }

    #[test]
    fn the_deny_list_drops_rows_by_endpoint_and_counts_them() {
        let (sampler, entries) = sampler(&["user-inbox"], Arc::new(FixedClock::at(T0)));
        sampler.sample(Reading::Tick).unwrap();
        let entries = entries.lock().unwrap();
        let e = &entries[0];
        assert_eq!(e.get("row.3"), None);
        assert!(e.fields.iter().all(|(k, _)| !k.starts_with("row.3")));
        assert_eq!(e.get("droppedDenied"), Some("1"));
        assert_eq!(e.get("row.2.endpointId"), Some("books-graph"));
        let line = e.render(&BTreeMap::new()).unwrap();
        assert!(!line.contains("user-inbox"), "{line}");
        assert!(!line.contains("alice:inbox"), "{line}");
    }

    #[test]
    fn no_argument_value_ever_appears_in_an_entry() {
        let (sampler, entries) = sampler(&[], Arc::new(FixedClock::at(T0)));
        sampler.sample(Reading::Tick).unwrap();
        sampler.shutdown();
        for e in entries.lock().unwrap().iter() {
            let line = e.render(&BTreeMap::new()).unwrap();
            for value in [
                "/home/alice",
                "alice/notes",
                "notes.md",
                "secret-term",
                "%2F",
            ] {
                assert!(!line.contains(value), "`{value}` is in {line}");
            }
            assert!(
                e.fields
                    .iter()
                    .all(|(_, v)| !v.starts_with("urn:file:/") && !v.contains("urn:search:")),
                "{line}"
            );
        }
    }

    #[test]
    fn the_shape_rule() {
        assert_eq!(
            shape_in("urn:file:/etc/hosts", &PATTERNS, id),
            Some(Shape {
                endpoint: "file".into(),
                shape: "urn:file:{path}".into()
            })
        );
        assert_eq!(shape_in("urn:search:x", &PATTERNS, id), None);
        assert_eq!(shape_in("urn:unbound:x", &PATTERNS, id), None);
        // A template IS a bound pattern: a target that spells it exactly is recorded as it.
        assert_eq!(
            shape_in("urn:books:graph", &PATTERNS, id).unwrap().shape,
            "urn:books:graph"
        );
    }

    #[test]
    fn settings_fail_loudly() {
        assert_eq!(settings(Some("false"), None, None, &[]), Ok(None));
        let on = settings(Some("true"), None, None, &[]).unwrap().unwrap();
        assert_eq!(on.every, DEFAULT_EVERY);
        assert_eq!(on.host, DEFAULT_HOST);
        assert!(settings(Some("TRUE"), None, None, &[]).is_err());
        assert!(
            settings(None, Some("30s"), None, &[]).is_err(),
            "checked even when off"
        );
        assert!(settings(Some("true"), None, Some(""), &[]).is_err());
        assert!(settings(Some("true"), None, Some("a b"), &[]).is_err());
        assert!(settings(Some("true"), None, None, &["".into()]).is_err());
        assert!(settings(Some("true"), None, None, &["a b".into()]).is_err());
    }

    /// The real hub: core's own window, read under the inspect capability alone, and a
    /// template target recorded by its shape.
    #[test]
    fn reads_a_real_kernel() {
        use ikigai_core::{
            EndpointSpace, Exact, FnEndpoint, ReprType, Representation, UriTemplate,
        };
        let text =
            |b: &str| Representation::new(ReprType::new("text/plain"), b.as_bytes().to_vec());
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:books:graph"),
                FnEndpoint::new("books-graph", move |_| Ok(text("g"))),
            )
            .bind(
                UriTemplate::parse("urn:note:{name}").unwrap(),
                FnEndpoint::new("note", move |_| Ok(text("n"))),
            );
        let kernel = Arc::new(ikigai_core::Kernel::new(Arc::new(space)));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for target in ["urn:books:graph", "urn:note:alice-diary"] {
            let request = Request::new(Verb::Source, Iri::parse(target).unwrap());
            runtime
                .block_on(kernel.issue(request, &Capability::root()))
                .unwrap();
        }
        let hub = HubKernel(Arc::clone(&kernel));
        let window = parse_window(&hub.window().unwrap()).unwrap();
        assert_eq!(window.resolutions, 2);
        assert_eq!(window.rows.len(), 2);
        assert_eq!(
            hub.shape("urn:note:alice-diary"),
            Some(Shape {
                endpoint: "note".into(),
                shape: "urn:note:{name}".into()
            })
        );
        assert_eq!(
            hub.shape("urn:books:graph").unwrap().endpoint,
            "books-graph"
        );
    }
}
