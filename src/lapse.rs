//! The lapse sweep (ledger [#3790](http://localhost:1062/l/default/item/3790)): on a timer,
//! `sink urn:iki:ledger:{ledger}:lapse` for every ledger in the store, so a lease that ran out
//! is RECORDED in its item's history with no request from anyone.
//!
//! ikigai-ledger 0.23.0 added the Sink and said who sends it: "gonk's tick already runs on a
//! schedule under the dispatcher's grant, so it sends `sink …:lapse` each tick". Until this,
//! nothing in gonk did, and a lapse was recorded only when a person asked.
//!
//! - **Recording only.** The Sink writes one `ledger:LapseEvent` per lapse and nothing else:
//!   no claim is released, reaped or relabeled (this server sets no lapse route).
//! - **Off unless configured.** With no [`EVERY_KEY`] line nothing is scheduled, and
//!   [`STATUS`] says so.
//! - **The backup's timer, not a second one.** The registry here is built over
//!   [`crate::backup::BackupTimer`] and [`crate::backup::NextRun`]: the first run after a stated
//!   first delay ([`first_delay`]), each next run recorded for [`STATUS`].
//! - **Its own authority, per ledger.** Each ledger's sweep is sent under [`capability`]: exactly
//!   the tokens the Sink requires ([`scopes`]) and gonk's own recorder principal
//!   ([`PRINCIPAL`]), which is the committer on every `LapseEvent` it writes. The tick itself
//!   fires under [`job_capability`], which holds no scope at all, and its one target is resolved
//!   only by the registry's own resolver (`Sweeper`), which no door reaches.
//! - **Failures are contained.** A ledger whose sweep fails is logged in words and recorded in
//!   [`STATUS`]; the other ledgers and the next run go on. Nothing logged names an item's title:
//!   ledger names, item numbers and counts only.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation,
    Iri, ReprType, Representation, Request, Result, SpaceEntry, Verb,
};
use ikigai_ledger::Ledger;
use ikigai_resolve::{CacheStatus, Resolver};
use ikigai_time::JobRegistry;
use serde_json::{json, Value};

use crate::backup::{humanize, stamp_iso, BackupTimer, NextRun, CATCH_UP};

/// The config line: how often to sweep, in `ikigai-time`'s grammar (`5m`, `30s`, `1h`). Absent
/// (or `off`) means no sweep.
pub const EVERY_KEY: &str = "gonk.ledger.lapse.every";

/// The job's one target, resolved only by the registry's own resolver (`Sweeper`): bound in no
/// space, so no door reaches it.
pub const SWEEP: &str = "urn:iki:gonk:lapse";

/// What the sweep did, per ledger, and when it runs next: `Source urn:iki:gonk:lapse:status`.
pub const STATUS: &str = "urn:iki:gonk:lapse:status";

/// The principal every sweep is sent as, and so the committer the ledger stamps on each
/// `ledger:LapseEvent` it records: this server, as the recorder, never a person.
pub const PRINCIPAL: &str = "urn:iki:gonk:recorder:lapse";

/// The shortest cadence accepted. A sweep reads every ledger, so a sub-second one is a typo.
pub const MIN_EVERY: Duration = Duration::from_secs(1);

const JSON: &str = "application/json";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The cadence [`EVERY_KEY`] spells: `None` when absent or `off`, refused when malformed, zero
/// or under [`MIN_EVERY`].
///
/// ```
/// use std::time::Duration;
/// use ikigai_gonk::lapse::every;
/// assert_eq!(every(None), Ok(None));
/// assert_eq!(every(Some("off")), Ok(None));
/// assert_eq!(every(Some("5m")), Ok(Some(Duration::from_secs(300))));
/// assert!(every(Some("0s")).is_err());
/// assert!(every(Some("500ms")).is_err());
/// assert!(every(Some("often")).is_err());
/// ```
pub fn every(spelled: Option<&str>) -> std::result::Result<Option<Duration>, String> {
    let Some(spelled) = spelled.map(str::trim) else {
        return Ok(None);
    };
    if spelled.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    let interval = ikigai_time::parse_schedule(spelled)
        .map_err(|e| {
            format!(
                "{EVERY_KEY}: `{spelled}` is not a cadence: {e} — a duration like `5m`, `30s` or \
                 `1h`, or remove the line to record no lapses on a timer"
            )
        })?
        .interval();
    if interval < MIN_EVERY {
        return Err(format!(
            "{EVERY_KEY}: `{spelled}` is under a second; every sweep reads every ledger"
        ));
    }
    Ok(Some(interval))
}

/// How long after startup the first sweep runs: one cadence, but never more than
/// [`CATCH_UP`], so a 1-hour sweep still records the lapses a restart left within a minute.
///
/// ```
/// use std::time::Duration;
/// use ikigai_gonk::lapse::first_delay;
/// assert_eq!(first_delay(Duration::from_secs(2)), Duration::from_secs(2));
/// assert_eq!(first_delay(Duration::from_secs(3600)), Duration::from_secs(60));
/// ```
pub fn first_delay(every: Duration) -> Duration {
    every.min(CATCH_UP)
}

/// The tokens `sink urn:iki:ledger:{ledger}:lapse` requires, and nothing more: the ledger's
/// `close` (the Sink takes `close` or `write`, and `close` is the narrower), and the read and
/// write grants of the ledger's graph at the store, which its read and its update go through.
///
/// ```
/// use ikigai_ledger::Ledger;
/// assert_eq!(
///     ikigai_gonk::lapse::scopes(&Ledger::parse("acme").unwrap()),
///     [
///         "urn:cap:ledger:close:acme",
///         "urn:cap:store:read:graph:urn:iki:ledger:graph:acme",
///         "urn:cap:store:write:graph:urn:iki:ledger:graph:acme",
///     ]
/// );
/// ```
pub fn scopes(ledger: &Ledger) -> [String; 3] {
    let graph = ledger.graph();
    [
        ledger.cap_close(),
        ikigai_store::cap_read_graph(&graph),
        ikigai_store::cap_write_graph(&graph),
    ]
}

/// What one ledger's sweep is sent under: [`scopes`], as [`PRINCIPAL`].
pub fn capability(ledger: &Ledger) -> Capability {
    Capability::scoped(scopes(ledger))
        .with_principal(PRINCIPAL)
        .expect("a constant principal IRI")
}

/// What the tick fires under, and the registry's ceiling: no scope at all, as [`PRINCIPAL`].
/// A tick reaches the registry's own resolver (`Sweeper`) and nothing else; were it ever
/// forwarded to the hub, it could read and write nothing.
pub fn job_capability() -> Capability {
    Capability::scoped(Vec::<String>::new())
        .with_principal(PRINCIPAL)
        .expect("a constant principal IRI")
}

/// What one ledger's last sweep found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swept {
    /// When it ran, in milliseconds since the epoch.
    pub at_ms: u64,
    /// The lapses it recorded.
    pub recorded: u64,
    /// The stalled items whose lapse an earlier sweep had recorded.
    pub already_recorded: u64,
    /// Every lapse recorded in this ledger since this server started.
    pub recorded_since_start: u64,
    /// Why the sweep failed, in words; `None` when it did not.
    pub error: Option<String>,
}

/// The sweep as configured and as it went: shared by the timer's resolver (`Sweeper`) and
/// [`STATUS`].
#[derive(Clone)]
pub struct Lapses {
    /// The cadence, or `None` when no [`EVERY_KEY`] line is set.
    pub every: Option<Duration>,
    /// The registry the sweep fires from; `None` when nothing is scheduled.
    pub jobs: Option<JobRegistry>,
    /// When the next sweep is due, as the timer records it.
    pub next: NextRun,
    swept: Arc<Mutex<BTreeMap<String, Swept>>>,
}

impl Lapses {
    /// The sweep every `every`, or none. With a cadence, the registry is built here, over the
    /// backup's timer, its first tick [`first_delay`] out; nothing fires until [`Lapses::start`].
    pub fn new(every: Option<Duration>, next: NextRun) -> Lapses {
        let jobs = every.map(|every| {
            JobRegistry::new(
                Arc::new(BackupTimer::new(first_delay(every), next.clone())),
                next.clock(),
            )
            .with_capability(job_capability())
        });
        Lapses {
            every,
            jobs,
            next,
            swept: Arc::default(),
        }
    }

    /// Schedule the sweep against `hub`, finding the ledgers in `store`. Returns the job's id,
    /// or `None` when no cadence is configured.
    pub fn start(
        &self,
        hub: Arc<dyn Resolver>,
        store: Arc<ikigai_store::Store>,
    ) -> std::result::Result<Option<u64>, String> {
        let (Some(every), Some(jobs)) = (self.every, &self.jobs) else {
            return Ok(None);
        };
        jobs.set_resolver(Arc::new(Sweeper {
            hub,
            store,
            lapses: self.clone(),
        }));
        jobs.schedule_persistent(
            SWEEP.to_string(),
            Verb::Sink,
            ikigai_time::Schedule::Every(every),
            true,
            job_capability(),
        )
        .map(Some)
    }

    /// Each ledger's last sweep, by name.
    pub fn swept(&self) -> BTreeMap<String, Swept> {
        self.swept.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn note(&self, ledger: &str, at_ms: u64, outcome: std::result::Result<(u64, u64), String>) {
        let mut swept = self.swept.lock().unwrap_or_else(|e| e.into_inner());
        let before = swept.get(ledger).map_or(0, |s| s.recorded_since_start);
        let entry = match outcome {
            Ok((recorded, already_recorded)) => Swept {
                at_ms,
                recorded,
                already_recorded,
                recorded_since_start: before + recorded,
                error: None,
            },
            Err(error) => Swept {
                at_ms,
                recorded: 0,
                already_recorded: 0,
                recorded_since_start: before,
                error: Some(error),
            },
        };
        swept.insert(ledger.to_string(), entry);
    }

    /// One sweep of every ledger in `store`, through `hub`: what the timer's tick runs.
    /// Returns one line for the job's health; `Err` when any ledger failed (the others are
    /// swept all the same).
    pub fn sweep(
        &self,
        hub: &dyn Resolver,
        store: &ikigai_store::Store,
    ) -> std::result::Result<String, String> {
        let ledgers = ledgers(store)?;
        let (mut recorded, mut already, mut failed) = (0, 0, 0);
        for name in &ledgers {
            let at_ms = self.next.now_ms();
            let outcome = sweep_one(hub, name);
            match &outcome {
                Ok((r, a, numbers)) => {
                    recorded += r;
                    already += a;
                    if *r > 0 {
                        eprintln!(
                            "ikigai-gonk: lapse sweep: recorded {r} lapse(s) in ledger `{name}` \
                             (item(s) {})",
                            numbers.join(", ")
                        );
                    }
                }
                Err(e) => {
                    failed += 1;
                    eprintln!(
                        "ikigai-gonk: lapse sweep: ledger `{name}` was not swept: {e}. The other \
                         ledgers and the next run go on"
                    );
                }
            }
            self.note(name, at_ms, outcome.map(|(r, a, _)| (r, a)));
        }
        let line = format!(
            "swept {} ledger(s): recorded {recorded} lapse(s), {already} already recorded, \
             {failed} ledger(s) failed",
            ledgers.len()
        );
        if failed > 0 {
            Err(line)
        } else {
            Ok(line)
        }
    }

    fn space(self) -> EndpointSpace {
        EndpointSpace::new().bind(Exact::new(STATUS), Status { lapses: self })
    }
}

/// [`Lapses`]' one resource, [`STATUS`], as a space for the hub.
pub fn space(lapses: Lapses) -> EndpointSpace {
    lapses.space()
}

/// The ledgers in `store`: each named graph `urn:iki:ledger:graph:{name}` that is a ledger's
/// ([`crate::script::ledgers_in`]). Read from the store's graph list, so a ledger filed in since
/// the last sweep is swept on the next.
fn ledgers(store: &ikigai_store::Store) -> std::result::Result<Vec<String>, String> {
    let mut graphs = String::new();
    for graph in store.named_graphs() {
        let graph = graph.map_err(|e| format!("the store's graphs could not be listed: {e}"))?;
        if let oxigraph::model::NamedOrBlankNode::NamedNode(node) = graph {
            graphs.push_str(node.as_str());
            graphs.push('\n');
        }
    }
    Ok(crate::script::ledgers_in(&graphs).into_iter().collect())
}

/// `sink urn:iki:ledger:{name}:lapse as=application/json` under [`capability`]: how many it
/// recorded, how many were recorded already, and the recorded items' numbers.
fn sweep_one(
    hub: &dyn Resolver,
    name: &str,
) -> std::result::Result<(u64, u64, Vec<String>), String> {
    let ledger = Ledger::parse(name).map_err(|e| format!("not a ledger name: {e}"))?;
    let target = Iri::parse(format!("urn:iki:ledger:{name}:lapse"))
        .map_err(|e| format!("no lapse resource: {e}"))?;
    let request =
        Request::new(Verb::Sink, target).with_arg("as", ArgRef::Inline(JSON.as_bytes().to_vec()));
    let (answer, _) = hub
        .issue_as(request, &capability(&ledger))
        .map_err(|e| format!("the ledger refused the sweep: {e}"))?;
    let document: Value = serde_json::from_slice(&answer.bytes)
        .map_err(|e| format!("the ledger's answer is not the lapse document: {e}"))?;
    let recorded = document
        .get("recorded")
        .and_then(Value::as_array)
        .ok_or("the ledger's answer has no `recorded` list")?;
    let already = document
        .get("already_recorded")
        .and_then(Value::as_u64)
        .ok_or("the ledger's answer has no `already_recorded` count")?;
    // Numbers only: a row's title is never read out of it.
    let numbers = recorded
        .iter()
        .map(
            |row| match row.pointer("/item/number").and_then(Value::as_i64) {
                Some(n) => format!("#{n}"),
                None => "#?".to_string(),
            },
        )
        .collect();
    Ok((recorded.len() as u64, already, numbers))
}

/// The registry's resolver: it answers [`SWEEP`] by running [`Lapses::sweep`], and nothing else.
/// Held only by the registry, so no caller reaches it.
struct Sweeper {
    hub: Arc<dyn Resolver>,
    store: Arc<ikigai_store::Store>,
    lapses: Lapses,
}

impl Resolver for Sweeper {
    fn issue(&self, request: Request) -> std::result::Result<(Representation, CacheStatus), Error> {
        self.issue_as(request, &job_capability())
    }

    fn issue_as(
        &self,
        request: Request,
        _capability: &Capability,
    ) -> std::result::Result<(Representation, CacheStatus), Error> {
        if request.target.as_str() != SWEEP || request.verb != Verb::Sink {
            return Err(Error::Unresolved(request.target));
        }
        let line = self
            .lapses
            .sweep(self.hub.as_ref(), &self.store)
            .map_err(Error::Endpoint)?;
        Ok((plain(line), CacheStatus::Miss))
    }

    fn is_cached(&self, _: &Request, _: &Capability) -> bool {
        false
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        None
    }
}

struct Status {
    lapses: Lapses,
}

#[async_trait]
impl Endpoint for Status {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "{STATUS} answers Source, not {:?}",
                inv.request.verb
            )));
        }
        if !inv.capability.allows(crate::system::CAP_READ) {
            return Err(Error::Denied(format!(
                "{STATUS} names every ledger and what was recorded in it, so it needs `{}`",
                crate::system::CAP_READ
            )));
        }
        if matches!(inv.inline_str("as"), Ok(JSON)) {
            let mut bytes = serde_json::to_vec(&self.json()).unwrap_or_default();
            bytes.push(b'\n');
            Ok(Representation::new(
                ReprType::new(JSON).with_param("charset", "utf-8"),
                bytes,
            ))
        } else {
            Ok(plain(self.text()))
        }
    }

    fn name(&self) -> &str {
        "gonk-lapse-status"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-lapse-status")
            .title("When the lapse sweep last ran, and what it recorded")
            .summary(
                "Whether this server records lapsed leases on a timer, and if it does: when \
                 the next sweep is due, and for each ledger when it was last swept, how many \
                 lapses that sweep recorded and how many were recorded already, or why it \
                 failed. The sweep records each lapse once in its item's history and never \
                 releases a claim.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(crate::system::CAP_READ)
            .input(
                ArgSpec::new("as")
                    .summary("the representation to answer in")
                    .class(XSD_STRING)
                    .one_of(["text/plain", JSON])
                    .default_value("text/plain")
                    .optional(),
            )
            .output("text/plain")
            .output(JSON)
    }
}

impl Status {
    fn job(&self) -> Option<ikigai_time::JobHealth> {
        self.lapses
            .jobs
            .as_ref()?
            .health()
            .into_iter()
            .find(|job| job.target == SWEEP)
    }

    fn text(&self) -> String {
        let Some(every) = self.lapses.every else {
            return format!(
                "Lapse sweep: off. There is no {EVERY_KEY} line, so a lapsed lease is recorded \
                 only when someone sends `sink urn:iki:ledger:<ledger>:lapse`.\n"
            );
        };
        let mut out = format!(
            "Lapse sweep: every {}. Each sweep records every lapsed lease once in its item's \
             history, as {PRINCIPAL}. It never releases a claim.\n",
            humanize(every)
        );
        match self.job() {
            None => out.push_str("Warning: the sweep is configured but no job is scheduled.\n"),
            Some(job) => {
                out.push_str(&format!("Runs so far: {}.\n", job.runs));
                match self.lapses.next.due_ms() {
                    Some(due) => {
                        let now = self.lapses.next.now_ms();
                        let when = if due >= now {
                            format!("in {}", humanize(Duration::from_millis(due - now)))
                        } else {
                            format!("overdue by {}", humanize(Duration::from_millis(now - due)))
                        };
                        out.push_str(&format!("Next run: {} ({when}).\n", stamp_iso(due)));
                    }
                    None => out.push_str("Next run: a sweep is running now.\n"),
                }
                if job.failures_in_a_row > 0 {
                    out.push_str(&format!(
                        "Warning: the last {} run(s) failed on at least one ledger.\n",
                        job.failures_in_a_row
                    ));
                }
            }
        }
        let swept = self.lapses.swept();
        if swept.is_empty() {
            out.push_str("No ledger has been swept yet.\n");
        }
        for (ledger, s) in swept {
            match &s.error {
                None => out.push_str(&format!(
                    "Ledger {ledger}: last swept {}. It recorded {} lapse(s); {} more were \
                     already recorded. {} recorded since this server started.\n",
                    stamp_iso(s.at_ms),
                    s.recorded,
                    s.already_recorded,
                    s.recorded_since_start
                )),
                Some(e) => out.push_str(&format!(
                    "Ledger {ledger}: the sweep at {} failed: {e}\n",
                    stamp_iso(s.at_ms)
                )),
            }
        }
        out
    }

    fn json(&self) -> Value {
        let timer = self.job().map(|job| {
            json!({
                "id": job.id,
                "runs": job.runs,
                "interval_seconds": job.interval.as_secs(),
                "next_run_due": self.lapses.next.due_ms().map(stamp_iso),
                "seconds_since_last_run": job.since_last.map(|d| d.as_secs()),
                "failures_in_a_row": job.failures_in_a_row,
            })
        });
        let ledgers: Vec<Value> = self
            .lapses
            .swept()
            .into_iter()
            .map(|(ledger, s)| {
                json!({
                    "ledger": ledger,
                    "last_swept": stamp_iso(s.at_ms),
                    "recorded": s.recorded,
                    "already_recorded": s.already_recorded,
                    "recorded_since_start": s.recorded_since_start,
                    "error": s.error,
                })
            })
            .collect();
        json!({
            "schedule": self.lapses.every.map(humanize),
            "principal": PRINCIPAL,
            "timer": timer,
            "ledgers": ledgers,
        })
    }
}

fn plain(text: String) -> Representation {
    Representation::new(
        ReprType::new("text/plain").with_param("charset", "utf-8"),
        text.into_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sweep's capability holds the lapse Sink's tokens for its own ledger and nothing a
    /// worker, a filer or another ledger needs.
    #[test]
    fn a_ledgers_capability_is_the_lapse_tokens_alone() {
        let acme = Ledger::parse("acme").unwrap();
        let cap = capability(&acme);
        assert_eq!(cap.principal(), Some(PRINCIPAL));
        for held in scopes(&acme) {
            assert!(cap.allows(&held), "{held}");
        }
        for not in [
            acme.cap_write(),
            acme.cap_comment(),
            acme.cap_claim(),
            acme.cap_read(),
            acme.cap_delete(),
            acme.cap_about(),
            Ledger::parse("other").unwrap().cap_close(),
            ikigai_store::CAP_READ.to_string(),
            crate::system::CAP_READ.to_string(),
        ] {
            assert!(!cap.allows(&not), "{not}");
        }
        assert!(!job_capability().allows(&acme.cap_close()));
    }
}
