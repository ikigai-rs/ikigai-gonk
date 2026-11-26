//! Query sensors: published SPARQL ASK or SELECT scripts that gonk runs on its own tick, and
//! that report, or file an item, when they fire (ledger
//! [#3726](http://localhost:1062/l/default/item/3726)).
//!
//! ```text
//! urn:script:harness-{id}           a query sensor: a script whose name starts `harness-`
//! gonk.sensor.every = "15m"         how often the tick runs them (this IS the default; "off")
//! gonk.sensor.ledger = "default"    file an item there when one fires (unset: record only)
//! ```
//!
//! # What a sensor is
//!
//! A harness SENSOR is a control that refuses, warns or reports ([`crate::harness`]). A QUERY
//! sensor is one whose code is a published `ikigai-script` ([`crate::script`]) named
//! `harness-{id}`: a SPARQL ASK (it fires when the answer is `true`) or SELECT (it fires when
//! it has a row). Its control is the script, `urn:script:harness-{id}`, so the code that
//! enforces it is the text its publisher signed, and every version of it stays fetchable.
//!
//! # The authority it runs under
//!
//! The tick runs each sensor as ROOT, through the hub, at `urn:script:harness-{id}:result`, the
//! read every caller makes. The library holds a run to `runner ∩ what the publisher held at
//! publish ∩ the host's ceiling` ([`crate::script::CEILING`], the ledgers' reads), and root is
//! no limit, so a sensor runs under min(what it declares, its publisher's grant, the ceiling):
//! a sensor a customer publishes can never read more than its publisher could.
//!
//! Two refusals at publish say so before the first tick, rather than at every one:
//!
//! - **the library's**: a script that declares a graph its publisher cannot read is refused
//!   (`ikigai_script::authority::grant_at_publish`), sensor or not;
//! - **this module's** ([`publish_refusal`]): a `harness-*` script must be SPARQL, an ASK or a
//!   SELECT, and must declare nothing the ceiling withholds (a browse graph, `GRAPH ?g`), since
//!   the tick could never run it.
//!
//! # What a firing is
//!
//! A sensor FIRES when its answer turns from quiet to firing: the first tick that finds it
//! firing, and not the ticks after, until a tick finds it quiet again. Each firing is one
//! entry in gonk's own sealed log segment ([`crate::access::Record`], the one every door line
//! lands in), class [`FIRED`], its subject the control:
//!
//! ```text
//! 2026-11-15T12:00:00.000Z gonk:SensorFired urn:script:harness-closed-without-evidence seq=… version=sha256:… rows=2
//! ```
//!
//! ★ **Counts only**, like the harness page: the script's version digest and how many rows, never
//! a row. A row is ledger text, and a log line is no place to copy it.
//!
//! With `gonk.sensor.ledger` set, a firing also files ONE item in that ledger, labeled
//! [`LABEL`] and `harness-{id}`, saying which sensor fired, which version and how many rows.
//! While an open item carries the sensor's label nothing more is filed, so a restart (which
//! forgets which sensors were firing) does not file a second.
//!
//! # The first tick
//!
//! A restart never pushes the next tick more than one `every` past the last one (ledger
//! [#4364](http://localhost:1062/l/default/item/4364)). Each tick stamps its time, before it
//! runs, in the data home's `sensors/last-tick` ([`Pace`]), and a start phases its first tick
//! to that stamp ([`first_delay`]), the way the backup timer phases to its newest archive: a
//! restart 30 minutes after a tick on an hourly cadence ticks 30 minutes later, not an hour
//! later. A timer that waited a whole cadence from each start never ran a sensor on a gonk
//! restarted more often than its cadence.
//!
//! With no stamp (the first start, or a stamp that cannot be written), or one already a cadence
//! old, the first tick runs at once. That is chosen over waiting because a tick costs little
//! twice: it is read-only queries plus a filing the open-item rule already guards, so an extra
//! tick after a restart costs a query and files nothing twice. A stamp in the future (a clock
//! stepped back) waits one cadence, no more.
//!
//! # The clock
//!
//! A sensor that compares against the time declares `# @param now xsd:dateTime`, and the tick
//! binds it to the tick's own time. The library refuses an argument a script does not declare,
//! and a query that called `NOW()` would be answered from the hub's cache until the next store
//! write, so the clock is an argument, like every other input.
//!
//! # The examples
//!
//! [`EXAMPLES`], in `src/sensors/`: an item closed without evidence, a pull request merged
//! without a verdict line in its close note, and a lapsed claim nobody took for a day. Each
//! reads the default ledger, and is published like any script (the README says how).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_script::sparql::{Form, SparqlDoor};
use ikigai_script::{Backend, Language, State};
use serde_json::Value;

use crate::access::Record;

/// The name every query sensor's script starts with: `urn:script:harness-{id}`.
pub const PREFIX: &str = "harness-";
/// The log class of a firing.
pub const FIRED: &str = "gonk:SensorFired";
/// The label every item a sensor files carries, beside `harness-{id}`.
pub const LABEL: &str = "sensor";
/// The parameter the tick binds to its own time.
pub const NOW: &str = "now";
/// The config line for the tick's cadence.
pub const EVERY_KEY: &str = "gonk.sensor.every";
/// The config line naming the ledger a firing files into.
pub const LEDGER_KEY: &str = "gonk.sensor.ledger";
/// How often the tick runs when no `gonk.sensor.every` line says otherwise.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(15 * 60);

const RESULTS_JSON: &str = "application/sparql-results+json";

/// One example sensor, shipped beside the code.
pub struct Example {
    /// Its script name: `urn:script:{name}`.
    pub name: &'static str,
    /// Its SPARQL text.
    pub text: &'static str,
}

/// The example sensors, each over the default ledger.
pub const EXAMPLES: [Example; 3] = [
    Example {
        name: "harness-closed-without-evidence",
        text: include_str!("sensors/closed-without-evidence.rq"),
    },
    Example {
        name: "harness-merged-without-verdict",
        text: include_str!("sensors/merged-without-verdict.rq"),
    },
    Example {
        name: "harness-lapsed-claim",
        text: include_str!("sensors/lapsed-claim.rq"),
    },
];

/// Why a publish of `language` text `content` as the script `name` is refused because it is a
/// query sensor that could never run, or `None` (also for every name that is no sensor's).
///
/// ```
/// use ikigai_gonk::sensor::publish_refusal;
/// let ask = "ASK FROM <urn:iki:ledger:graph:default> WHERE { ?s ?p ?o }";
/// assert!(publish_refusal("harness-any", "sparql", ask).is_none());
/// // Not a sensor: not this rule's.
/// assert!(publish_refusal("open-by-label", "plan", "").is_none());
/// // A sensor is an ASK or a SELECT, over what the ceiling admits.
/// let construct = "CONSTRUCT { ?s ?p ?o } FROM <urn:iki:ledger:graph:default> WHERE { ?s ?p ?o }";
/// assert!(publish_refusal("harness-any", "sparql", construct).is_some());
/// assert!(publish_refusal("harness-any", "plan", "").is_some());
/// let browse = "ASK FROM <urn:iki:browse:graph:default> WHERE { ?s ?p ?o }";
/// assert!(publish_refusal("harness-any", "sparql", browse).is_some());
/// ```
pub fn publish_refusal(name: &str, language: &str, content: &str) -> Option<Error> {
    if !name.starts_with(PREFIX) {
        return None;
    }
    let refuse = |why: String| {
        Some(Error::Denied(
            format!(
                "`{name}` is a query sensor (a script named `{PREFIX}*`), which gonk runs on its \
             tick: {why}"
            )
            .into(),
        ))
    };
    if language != "sparql" {
        return refuse(format!(
            "a sensor is a SPARQL ASK or SELECT, and this one is `{language}`"
        ));
    }
    let analysis = match ikigai_script::sparql::analyze(content, &SparqlDoor::store()) {
        Ok(analysis) => analysis,
        // The library's own refusal names what is wrong with the text.
        Err(_) => return None,
    };
    if !matches!(analysis.form, Form::Ask | Form::Select) {
        return refuse(format!(
            "a sensor is an ASK or a SELECT, and this one is {:?}",
            analysis.form
        ));
    }
    let ceiling = crate::script::ceiling();
    let beyond: Vec<String> = analysis
        .requires()
        .into_iter()
        .filter(|scope| !ceiling.allows(scope))
        .collect();
    if !beyond.is_empty() {
        return refuse(format!(
            "it reads `{}`, beyond the ceiling every script here runs under (`{}`), so no tick \
             could ever run it. Name the ledger graphs it reads",
            beyond.join("`, `"),
            crate::script::CEILING.join("`, `")
        ));
    }
    None
}

/// The tick's cadence, from `gonk.sensor.every`: `None` when it says `off`.
///
/// ```
/// use std::time::Duration;
/// use ikigai_gonk::sensor::{every, DEFAULT_EVERY};
/// assert_eq!(every(None), Ok(Some(DEFAULT_EVERY)));
/// assert_eq!(every(Some("off")), Ok(None));
/// assert_eq!(every(Some("1h")), Ok(Some(Duration::from_secs(3600))));
/// assert!(every(Some("10s")).is_err());
/// assert!(every(Some("often")).is_err());
/// ```
pub fn every(spelled: Option<&str>) -> std::result::Result<Option<Duration>, String> {
    let Some(spelled) = spelled else {
        return Ok(Some(DEFAULT_EVERY));
    };
    if spelled.trim().eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    let schedule = ikigai_time::parse_schedule(spelled.trim()).map_err(|e| {
        format!("{EVERY_KEY}: {e} — a duration like `15m` or `1h`, or `off` to run no sensors")
    })?;
    let interval = schedule.interval();
    if interval < Duration::from_secs(60) {
        return Err(format!(
            "{EVERY_KEY}: `{spelled}` is under a minute; every tick runs every sensor"
        ));
    }
    Ok(Some(interval))
}

/// The ledger a firing files into, from `gonk.sensor.ledger`: `None` when unset.
///
/// ```
/// use ikigai_gonk::sensor::ledger;
/// assert_eq!(ledger(None), Ok(None));
/// assert_eq!(ledger(Some("default")), Ok(Some("default".to_string())));
/// assert!(ledger(Some("Not A Ledger")).is_err());
/// ```
pub fn ledger(spelled: Option<&str>) -> std::result::Result<Option<String>, String> {
    let Some(spelled) = spelled.map(str::trim) else {
        return Ok(None);
    };
    ikigai_ledger::Ledger::parse(spelled)
        .map(|_| Some(spelled.to_string()))
        .map_err(|e| format!("{LEDGER_KEY}: `{spelled}` is not a ledger name: {e}"))
}

/// What one tick found for one sensor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The script's name, `harness-{id}`.
    pub name: String,
    /// Whether its answer is firing now (`true`, or at least one row).
    pub firing: bool,
    /// How many rows (an ASK's `true` is one).
    pub rows: usize,
    /// Whether this tick recorded a firing: it turned from quiet to firing.
    pub fired: bool,
    /// The item this firing filed, as the ledger answered.
    pub filed: Option<String>,
    /// Why the sensor could not be read, when it could not.
    pub error: Option<String>,
}

/// The query sensors: what the tick reads, what it remembers between ticks, and where it
/// records.
pub struct Sensors {
    backend: Arc<dyn Backend>,
    record: Option<Arc<Record>>,
    ledger: Option<String>,
    /// The sensors the last tick found firing.
    firing: Mutex<BTreeSet<String>>,
}

impl Sensors {
    /// The sensors among the scripts in `backend` ([`crate::script::Scripts::backend`]),
    /// recording into `record`, filing into `ledger` when one is named.
    pub fn new(
        backend: Arc<dyn Backend>,
        record: Option<Arc<Record>>,
        ledger: Option<String>,
    ) -> Sensors {
        Sensors {
            backend,
            record,
            ledger,
            firing: Mutex::new(BTreeSet::new()),
        }
    }

    /// Run every published sensor once, against `hub`, at `now_ms`.
    pub async fn tick(&self, hub: &Kernel, now_ms: u64) -> Vec<Reading> {
        let names = match self.backend.names() {
            Ok(names) => names,
            Err(e) => {
                eprintln!("ikigai-gonk: the sensors' scripts could not be listed: {e}");
                return Vec::new();
            }
        };
        let mut readings = Vec::new();
        for name in names.into_iter().filter(|n| n.starts_with(PREFIX)) {
            if let Some(reading) = self.read(hub, &name, now_ms).await {
                readings.push(reading);
            }
        }
        readings
    }

    /// One sensor's reading, or `None` when it is not a published SPARQL script.
    async fn read(&self, hub: &Kernel, name: &str, now_ms: u64) -> Option<Reading> {
        let head = self.backend.head(name).ok()??;
        if head.state != State::Published {
            self.forget(name);
            return None;
        }
        let version = self.backend.version(name, &head.version).ok()??;
        if version.language != Language::Sparql {
            return None;
        }
        let mut reading = Reading {
            name: name.to_string(),
            firing: false,
            rows: 0,
            fired: false,
            filed: None,
            error: None,
        };
        let answer = match self.run(hub, name, &version.source, now_ms).await {
            Ok(rows) => rows,
            Err(e) => {
                reading.error = Some(e);
                return Some(reading);
            }
        };
        reading.rows = answer;
        reading.firing = answer > 0;
        let was = {
            let mut firing = self.firing.lock().unwrap_or_else(|p| p.into_inner());
            if reading.firing {
                !firing.insert(name.to_string())
            } else {
                firing.remove(name);
                true
            }
        };
        if reading.firing && !was {
            reading.fired = true;
            self.record_firing(name, &head.version, answer, now_ms);
            if let Some(ledger) = &self.ledger {
                match self.file(hub, ledger, name, &head.version, answer).await {
                    Ok(filed) => reading.filed = filed,
                    Err(e) => reading.error = Some(e),
                }
            }
        }
        Some(reading)
    }

    fn forget(&self, name: &str) {
        self.firing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(name);
    }

    /// Run the sensor as root: what it reads is `granted ∩ ceiling`, decided by the library.
    /// The answer's row count (an ASK's `true` is one).
    async fn run(
        &self,
        hub: &Kernel,
        name: &str,
        source: &str,
        now_ms: u64,
    ) -> std::result::Result<usize, String> {
        let target = Iri::parse(format!("{}{name}:result", ikigai_script::name::PREFIX))
            .map_err(|e| e.to_string())?;
        let mut request = Request::new(Verb::Source, target).with_arg("as", inline(RESULTS_JSON));
        let declares_now = ikigai_script::sparql::analyze(source, &SparqlDoor::store())
            .map(|a| a.parameters.iter().any(|p| p.name == NOW))
            .unwrap_or(false);
        if declares_now {
            request = request.with_arg(NOW, inline(&date_time(now_ms)));
        }
        let answer = hub
            .issue(request, &Capability::root())
            .await
            .map_err(|e| e.to_string())?;
        rows(&answer.bytes)
    }

    fn record_firing(&self, name: &str, version: &str, rows: usize, now_ms: u64) {
        if let Some(record) = &self.record {
            record.entry(
                ikigai_log::Entry::new(
                    ikigai_log::Timestamp::from_millis(now_ms),
                    FIRED,
                    format!("{}{name}", ikigai_script::name::PREFIX),
                )
                .with("version", version)
                .with("rows", rows.to_string()),
            );
        }
    }

    /// File one item for this firing in `ledger`, unless an open item already carries the
    /// sensor's label. As root: the operator configured it, and the sensor's own run wrote
    /// nothing.
    async fn file(
        &self,
        hub: &Kernel,
        ledger: &str,
        name: &str,
        version: &str,
        rows: usize,
    ) -> std::result::Result<Option<String>, String> {
        let ledger = ikigai_ledger::Ledger::parse(ledger).map_err(|e| e.to_string())?;
        let iri = |action: &str| Iri::parse(ledger.resource(action)).map_err(|e| e.to_string());
        let open = hub
            .issue(
                Request::new(Verb::Source, iri("items")?)
                    .with_arg("labels", inline(name))
                    .with_arg("as", inline("application/json")),
                &Capability::root(),
            )
            .await
            .map_err(|e| e.to_string())?;
        if has_items(&open.bytes) {
            return Ok(None);
        }
        let control = format!("{}{name}", ikigai_script::name::PREFIX);
        let content = format!(
            "Sensor {name} fired\n\n\
             The query sensor <{control}> (version {version}) found {rows} row{s} on gonk's \
             tick. Run `{control}:result` to see them, and close this item once they are \
             answered.",
            s = if rows == 1 { "" } else { "s" },
        );
        let filed = hub
            .issue(
                Request::new(Verb::Sink, iri("append")?)
                    .with_arg("content", inline(&content))
                    .with_arg("labels", inline(&format!("{LABEL},{name}"))),
                &Capability::root(),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(Some(
            String::from_utf8_lossy(&filed.bytes).trim().to_string(),
        ))
    }

    /// Run [`Sensors::tick`] every `every` on a thread of its own, against the hub `hub` holds,
    /// on `pace`'s clock: the first tick [`first_delay`] from now, phased to the last one
    /// `pace` stamped, and each tick stamped there as it starts (see "The first tick").
    pub fn start(
        self: &Arc<Self>,
        hub: std::sync::Weak<Kernel>,
        every: Duration,
        pace: Pace,
    ) -> Option<std::thread::JoinHandle<()>> {
        let sensors = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("gonk-sensors".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        eprintln!("ikigai-gonk: THE SENSORS DO NOT RUN: no runtime: {e}");
                        return;
                    }
                };
                let first = first_delay(pace.now_ms(), pace.last_tick(), every);
                (pace.sleep)(first);
                loop {
                    let Some(hub) = hub.upgrade() else {
                        return;
                    };
                    let now = pace.now_ms();
                    pace.stamp_tick(now);
                    for reading in runtime.block_on(sensors.tick(&hub, now)) {
                        if let Some(e) = &reading.error {
                            eprintln!("ikigai-gonk: sensor {}: {e}", reading.name);
                        }
                    }
                    drop(hub);
                    (pace.sleep)(every);
                }
            });
        match spawned {
            Ok(handle) => Some(handle),
            Err(e) => {
                eprintln!("ikigai-gonk: THE SENSORS DO NOT RUN: no thread: {e}");
                None
            }
        }
    }
}

/// How long after a start the first tick runs: one `every` after the last tick
/// (`last_ms`), never later than one `every` from now (a stamp in the future, from a clock
/// stepped back, waits one cadence, no more), and at once when that is already past or gonk
/// stamped no tick.
///
/// ```
/// use std::time::Duration;
/// use ikigai_gonk::sensor::first_delay;
/// let hour = Duration::from_secs(3600);
/// let min = 60_000;
/// // A restart 30 minutes after a tick: the next is 30 minutes out, not an hour.
/// assert_eq!(first_delay(100 * min, Some(70 * min), hour), Duration::from_secs(30 * 60));
/// // Overdue, or never stamped: at once.
/// assert_eq!(first_delay(100 * min, Some(0), hour), Duration::ZERO);
/// assert_eq!(first_delay(100 * min, None, hour), Duration::ZERO);
/// // Stamped in the future: one cadence.
/// assert_eq!(first_delay(100 * min, Some(500 * min), hour), hour);
/// ```
pub fn first_delay(now_ms: u64, last_ms: Option<u64>, every: Duration) -> Duration {
    let Some(last) = last_ms else {
        return Duration::ZERO;
    };
    let every_ms = u64::try_from(every.as_millis()).unwrap_or(u64::MAX);
    let due = last.saturating_add(every_ms);
    Duration::from_millis(due.saturating_sub(now_ms)).min(every)
}

/// The tick's clock, how it waits, and the file it stamps each tick's time into, so that a
/// restart phases the first tick to the last one.
#[derive(Clone)]
pub struct Pace {
    /// What "now" is.
    pub clock: Arc<dyn ikigai_core::Clock>,
    /// How the tick waits for a duration.
    pub sleep: Arc<dyn Fn(Duration) + Send + Sync>,
    /// The last tick's time, in milliseconds since the epoch, as one decimal line; `None` to
    /// remember nothing across a restart.
    pub stamp: Option<std::path::PathBuf>,
}

impl Pace {
    /// The system clock and a real sleep, stamping into `stamp` — what the server runs on.
    pub fn system(stamp: Option<std::path::PathBuf>) -> Pace {
        Pace {
            clock: Arc::new(ikigai_core::SystemClock),
            sleep: Arc::new(std::thread::sleep),
            stamp,
        }
    }

    fn now_ms(&self) -> u64 {
        self.clock.now().as_millis()
    }

    /// The last tick's time, when the stamp holds one.
    pub fn last_tick(&self) -> Option<u64> {
        let text = std::fs::read_to_string(self.stamp.as_ref()?).ok()?;
        text.trim().parse().ok()
    }

    /// Stamp a tick at `now_ms`, through a rename so a crash never leaves half a stamp. A
    /// stamp that cannot be written is said, and the next restart ticks at once.
    fn stamp_tick(&self, now_ms: u64) {
        let Some(path) = &self.stamp else {
            return;
        };
        let partial = path.with_extension("partial");
        let written = std::fs::write(&partial, format!("{now_ms}\n"))
            .and_then(|()| std::fs::rename(&partial, path));
        if let Err(e) = written {
            eprintln!(
                "ikigai-gonk: the sensors' tick could not be stamped in {}: {e}",
                path.display()
            );
        }
    }
}

fn inline(text: &str) -> ArgRef {
    ArgRef::Inline(text.as_bytes().to_vec())
}

/// The row count of a SPARQL JSON result: an ASK's `true` is one, its `false` none.
fn rows(bytes: &[u8]) -> std::result::Result<usize, String> {
    let json: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("the sensor's answer is not SPARQL JSON results: {e}"))?;
    if let Some(answer) = json.get("boolean").and_then(Value::as_bool) {
        return Ok(usize::from(answer));
    }
    json.pointer("/results/bindings")
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| "the sensor's answer has neither a boolean nor bindings".to_string())
}

/// Whether a ledger's JSON item listing names any item.
fn has_items(bytes: &[u8]) -> bool {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Array(items)) => !items.is_empty(),
        Ok(Value::Object(o)) => o
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()),
        // Unreadable: say there is one, so a firing never files twice over a format change.
        _ => true,
    }
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for milliseconds since the epoch: the tick's time as the `now`
/// argument binds it.
///
/// ```
/// use ikigai_gonk::sensor::date_time;
/// assert_eq!(date_time(0), "1970-01-01T00:00:00.000Z");
/// assert_eq!(date_time(1_763_164_800_123), "2025-11-15T00:00:00.123Z");
/// ```
pub fn date_time(ms: u64) -> String {
    let (secs, ms) = ((ms / 1000) as i64, ms % 1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
