//! Ledger [#4364](http://localhost:1062/l/default/item/4364): the sensors' first tick after a
//! start is phased to the last tick, not a whole `gonk.sensor.every` after the start, so a gonk
//! restarted more often than its cadence still runs its sensors.
//!
//! The tick runs on an injected clock and an injected sleep: each sleep moves the clock on by
//! what it waited, so an hour's cadence runs in milliseconds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Clock, Iri, Kernel, Request, Time, Verb};
use ikigai_gonk::access::Record;
use ikigai_gonk::compose_with;
use ikigai_gonk::grants::{grants_for, script_grants, Authority, ScriptAuthority};
use ikigai_gonk::script::Scripts;
use ikigai_gonk::sensor::{Pace, Sensors, EXAMPLES, FIRED};
use ikigai_store::DurableStore;

const PERSON: &str = "urn:iki:gonk:passkey:U0VOU09S";
const SENSOR: &str = "harness-closed-without-evidence";
const MINUTE_MS: u64 = 60_000;
const HOUR: Duration = Duration::from_secs(3600);

/// A clock the test moves.
struct Hand(AtomicU64);

impl Clock for Hand {
    fn now(&self) -> Time {
        Time::from_millis(self.0.load(Ordering::SeqCst))
    }
}

fn root(hub: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    block_on(hub.issue(request, &Capability::root())).unwrap_or_else(|e| panic!("{iri}: {e}"));
}

/// `every` = 1 h, a restart 30 minutes after the last tick: the sensor runs within the hour
/// of that tick (30 minutes after the restart), where a timer started at the restart would
/// first run it 90 minutes after the last tick.
#[test]
fn a_restart_thirty_minutes_after_a_tick_runs_the_sensor_within_the_hour() {
    // A hub with one sensor that fires: an item closed on a bare word.
    let scripts = Scripts::in_memory();
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().unwrap(),
        None,
        Vec::new(),
        Vec::new(),
        None,
        Some(&scripts),
        None,
    ));
    scripts.attach(&hub);
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.extend(script_grants(SENSOR, ScriptAuthority::Publish).unwrap());
    let publisher = Capability::scoped(scopes).with_principal(PERSON).unwrap();
    let text = EXAMPLES.iter().find(|e| e.name == SENSOR).unwrap().text;
    let request = Request::new(
        Verb::Sink,
        Iri::parse(format!("urn:script:{SENSOR}")).unwrap(),
    )
    .with_arg("language", ArgRef::Inline(b"sparql".to_vec()))
    .with_arg("content", ArgRef::Inline(text.as_bytes().to_vec()));
    block_on(hub.issue(request, &publisher)).unwrap();
    root(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "closed on a word")],
    );
    root(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:close",
        &[("item", "1"), ("reason", "done")],
    );

    let (fired_tx, fired_rx) = mpsc::channel::<String>();
    let fired_tx = Mutex::new(fired_tx);
    let record = Record::to(Arc::new(move |line: &str| {
        if line.contains(FIRED) {
            let _ = fired_tx.lock().unwrap().send(line.to_string());
        }
    }));
    let sensors = Arc::new(Sensors::new(scripts.backend(), Some(record), None));

    // The last tick at 12:00; gonk restarts at 12:30.
    let last_tick = 1_764_072_000_000; // 2025-11-25T12:00:00Z
    let restart = last_tick + 30 * MINUTE_MS;
    let dir = tempfile::tempdir().unwrap();
    let stamp = dir.path().join("last-tick");
    std::fs::write(&stamp, format!("{last_tick}\n")).unwrap();

    let clock = Arc::new(Hand(AtomicU64::new(restart)));
    let slept: Arc<Mutex<Vec<Duration>>> = Arc::default();
    // Every sleep moves the clock on; the second (the one after the first tick) parks the
    // thread for good, so the test sees exactly one tick.
    let (park_tx, park_rx) = mpsc::channel::<()>();
    let park_rx = Mutex::new(park_rx);
    let pace = Pace {
        clock: clock.clone(),
        sleep: {
            let clock = Arc::clone(&clock);
            let slept = Arc::clone(&slept);
            Arc::new(move |d: Duration| {
                let first = {
                    let mut slept = slept.lock().unwrap();
                    slept.push(d);
                    slept.len() == 1
                };
                clock.0.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
                if !first {
                    let _ = park_rx.lock().unwrap().recv();
                }
            })
        },
        stamp: Some(stamp.clone()),
    };
    sensors
        .start(Arc::downgrade(&hub), HOUR, pace)
        .expect("the tick's thread starts");

    let line = fired_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the sensor runs after the restart");
    let first_wait = slept.lock().unwrap()[0];
    assert_eq!(
        first_wait,
        Duration::from_secs(30 * 60),
        "the first tick waits out the cadence since the last tick, not a whole hour: {line}"
    );
    let ticked: u64 = std::fs::read_to_string(&stamp)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(ticked, last_tick + 60 * MINUTE_MS, "stamped at the tick");
    assert!(
        ticked - last_tick <= 60 * MINUTE_MS,
        "within the hour of the last tick"
    );
    drop(hub);
    drop(park_tx);
}

/// With no stamp (a first start, or a stamp that could not be written) the first tick runs at
/// once.
#[test]
fn with_no_stamp_the_first_tick_runs_at_once() {
    assert_eq!(
        ikigai_gonk::sensor::first_delay(1_764_072_000_000, None, HOUR),
        Duration::ZERO
    );
}
