//! Ledger [#3726](http://localhost:1062/l/default/item/3726): query sensors, published SPARQL
//! ASK or SELECT scripts named `harness-*`, that gonk runs on its tick and that record a
//! firing, and file an item, when they fire.
//!
//! Each example sensor ([`ikigai_gonk::sensor::EXAMPLES`]) fires on its example data and not on
//! its counter-example; a firing is one log line naming the control and one filed item; and a
//! sensor whose script exceeds its publisher's grant, or the host's ceiling, is refused at
//! publish. Every test seeds its own in-memory hub and calls the tick directly.

use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::access::Record;
use ikigai_gonk::compose_with;
use ikigai_gonk::grants::{grants_for, script_grants, Authority, ScriptAuthority};
use ikigai_gonk::script::Scripts;
use ikigai_gonk::sensor::{Reading, Sensors, EXAMPLES, FIRED};
use ikigai_store::DurableStore;

/// A passkey's principal, as the HTTP door mints it.
const PERSON: &str = "urn:iki:gonk:passkey:U0VOU09S";

/// A day, in milliseconds.
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

fn hub() -> (Arc<Kernel>, Scripts) {
    let scripts = Scripts::in_memory();
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        Vec::new(),
        Vec::new(),
        None,
        Some(&scripts),
        None,
    ));
    scripts.attach(&hub);
    (hub, scripts)
}

fn issue(
    kernel: &Kernel,
    capability: &Capability,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
) -> Result<String, Error> {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    block_on(kernel.issue(request, capability))
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
}

fn root(hub: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
    issue(hub, &Capability::root(), verb, iri, args)
        .unwrap_or_else(|e| panic!("{verb:?} {iri}: {e}"))
}

/// A person who reads `ledger` and publishes the script `name`.
fn publisher(ledger: &str, name: &str) -> Capability {
    let mut scopes = grants_for(ledger, Authority::Read).unwrap();
    scopes.extend(script_grants(name, ScriptAuthority::Publish).unwrap());
    Capability::scoped(scopes).with_principal(PERSON).unwrap()
}

fn publish(hub: &Kernel, who: &Capability, name: &str, text: &str) -> Result<String, Error> {
    issue(
        hub,
        who,
        Verb::Sink,
        &format!("urn:script:{name}"),
        &[("language", "sparql"), ("content", text)],
    )
}

fn example(name: &str) -> &'static str {
    EXAMPLES
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no example named {name}"))
        .text
}

/// Publish the example `name`, as a person who reads the default ledger.
fn publish_example(hub: &Kernel, name: &str) {
    publish(hub, &publisher("default", name), name, example(name))
        .unwrap_or_else(|e| panic!("publishing {name}: {e}"));
}

fn append(hub: &Kernel, title: &str) {
    root(
        hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", title)],
    );
}

fn close(hub: &Kernel, item: &str, extra: &[(&str, &str)]) {
    let mut args = vec![("item", item), ("reason", "done")];
    args.extend_from_slice(extra);
    root(hub, Verb::Sink, "urn:iki:ledger:close", &args);
}

type Lines = Arc<Mutex<Vec<String>>>;

fn sensors(scripts: &Scripts, ledger: Option<&str>) -> (Sensors, Lines) {
    let lines: Lines = Arc::default();
    let sink = Arc::clone(&lines);
    let record = Record::to(Arc::new(move |line: &str| {
        sink.lock().unwrap().push(line.to_string())
    }));
    (
        Sensors::new(scripts.backend(), Some(record), ledger.map(str::to_string)),
        lines,
    )
}

fn now_ms() -> u64 {
    ikigai_gonk::access::now().as_millis()
}

fn reading<'a>(readings: &'a [Reading], name: &str) -> &'a Reading {
    readings
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no reading for {name}: {readings:?}"))
}

/// Tick once and say whether `name` is firing.
fn firing(hub: &Kernel, sensors: &Sensors, name: &str, at: u64) -> bool {
    let readings = block_on(sensors.tick(hub, at));
    let reading = reading(&readings, name);
    assert_eq!(reading.error, None, "{reading:?}");
    reading.firing
}

// ------------------------------------------------------------- the examples, each both ways

#[test]
fn closed_without_evidence_fires_on_a_bare_done_close_and_not_on_one_with_evidence() {
    let name = "harness-closed-without-evidence";
    let (hub, scripts) = hub();
    publish_example(&hub, name);
    let (sensors, _) = sensors(&scripts, None);
    append(&hub, "fixed with a commit");
    close(
        &hub,
        "1",
        &[("evidence", "https://github.com/x/y/commit/abc")],
    );
    append(&hub, "still open");
    assert!(
        !firing(&hub, &sensors, name, now_ms()),
        "the counter-example: a done close that names its evidence, and an open item"
    );
    append(&hub, "closed on a word");
    close(&hub, "3", &[("content", "done, trust me")]);
    assert!(
        firing(&hub, &sensors, name, now_ms()),
        "the example: a done close with no evidence"
    );
}

#[test]
fn merged_without_verdict_fires_on_a_pull_request_close_with_no_verdict_line() {
    let name = "harness-merged-without-verdict";
    let (hub, scripts) = hub();
    publish_example(&hub, name);
    let (sensors, _) = sensors(&scripts, None);
    append(&hub, "merged with a verdict");
    close(
        &hub,
        "1",
        &[
            ("evidence", "https://github.com/x/y/pull/3"),
            (
                "content",
                "Merged https://github.com/x/y/pull/3\nVerdict: confirmed, CI green",
            ),
        ],
    );
    append(&hub, "closed with no pull request");
    close(&hub, "2", &[("content", "nothing to merge")]);
    assert!(
        !firing(&hub, &sensors, name, now_ms()),
        "the counter-example: a verdict line beside the pull request, and a close naming none"
    );
    append(&hub, "merged on a word");
    close(
        &hub,
        "3",
        &[(
            "content",
            "Merged https://github.com/x/y/pull/4, the verdict is fine",
        )],
    );
    assert!(
        firing(&hub, &sensors, name, now_ms()),
        "the example: a note naming a pull request, and no line that starts `verdict:`"
    );
}

#[test]
fn merged_without_verdict_also_fires_on_pull_request_evidence_with_no_note() {
    let name = "harness-merged-without-verdict";
    let (hub, scripts) = hub();
    publish_example(&hub, name);
    let (sensors, _) = sensors(&scripts, None);
    append(&hub, "merged, said nothing");
    close(&hub, "1", &[("evidence", "https://github.com/x/y/pull/5")]);
    assert!(firing(&hub, &sensors, name, now_ms()));
}

#[test]
fn lapsed_claim_fires_a_day_after_the_lease_and_not_before() {
    let name = "harness-lapsed-claim";
    let (hub, scripts) = hub();
    publish_example(&hub, name);
    let (sensors, _) = sensors(&scripts, None);
    append(&hub, "claimed and dropped");
    root(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:claim",
        &[("item", "1"), ("content", "worker"), ("lease", "PT1S")],
    );
    let claimed = now_ms();
    assert!(
        !firing(&hub, &sensors, name, claimed + 60_000),
        "the counter-example: a lease that lapsed a minute ago"
    );
    assert!(
        firing(&hub, &sensors, name, claimed + DAY_MS + 60_000),
        "the example: a lease that lapsed over a day ago, nobody taking it over"
    );
    // A takeover replaces the lapsed claim, and the sensor goes quiet. Only an expired lease
    // can be taken over, and this one runs a second.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    root(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:claim",
        &[
            ("item", "1"),
            ("content", "another"),
            ("takeover", "true"),
            ("from", "worker"),
        ],
    );
    assert!(!firing(&hub, &sensors, name, claimed + DAY_MS + 60_000));
}

// ------------------------------------------------------------- what a firing does

#[test]
fn a_firing_is_one_log_line_naming_the_control_and_one_filed_item() {
    let name = "harness-closed-without-evidence";
    let (hub, scripts) = hub();
    publish_example(&hub, name);
    let (sensors, lines) = sensors(&scripts, Some("default"));
    append(&hub, "closed on a word");
    close(&hub, "1", &[]);

    let first = block_on(sensors.tick(&hub, now_ms()));
    let first = reading(&first, name);
    assert!(first.firing && first.fired, "{first:?}");
    assert_eq!(first.rows, 1);
    let filed = first.filed.clone().expect("a firing files an item");
    assert!(filed.starts_with("#2 "), "{filed}");

    let logged = lines.lock().unwrap().clone();
    let firings: Vec<&String> = logged.iter().filter(|l| l.contains(FIRED)).collect();
    assert_eq!(firings.len(), 1, "{logged:?}");
    let line = firings[0];
    // gonk's own classes are written as the log writes every class it has no prefix for.
    assert!(line.contains(FIRED), "{line}");
    assert!(line.contains(&format!(" urn:script:{name} ")), "{line}");
    assert!(line.contains("version=sha256:"), "{line}");
    assert!(line.contains("rows=1"), "{line}");
    // Counts only: no title of the item it found.
    assert!(!line.contains("closed on a word"), "{line}");

    let item = root(
        &hub,
        Verb::Source,
        "urn:iki:ledger:items",
        &[
            ("labels", &format!("sensor,{name}")),
            ("as", "application/json"),
        ],
    );
    assert!(item.contains("\"count\":1"), "{item}");
    assert!(item.contains(&format!("Sensor {name} fired")), "{item}");

    // Still firing on the next tick: no second line, no second item.
    let second = block_on(sensors.tick(&hub, now_ms()));
    let second = reading(&second, name);
    assert!(
        second.firing && !second.fired && second.filed.is_none(),
        "{second:?}"
    );
    assert_eq!(
        lines
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(FIRED))
            .count(),
        1
    );

    // A restart forgets what was firing, and the open item keeps it from filing twice.
    let (restarted, again) = self::sensors(&scripts, Some("default"));
    let third = block_on(restarted.tick(&hub, now_ms()));
    let third = reading(&third, name);
    assert!(third.fired && third.filed.is_none(), "{third:?}");
    assert_eq!(
        again
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(FIRED))
            .count(),
        1
    );
}

#[test]
fn without_a_ledger_a_firing_is_recorded_and_nothing_is_filed() {
    let name = "harness-closed-without-evidence";
    let (hub, scripts) = hub();
    publish_example(&hub, name);
    let (sensors, lines) = sensors(&scripts, None);
    append(&hub, "closed on a word");
    close(&hub, "1", &[]);
    let readings = block_on(sensors.tick(&hub, now_ms()));
    assert!(reading(&readings, name).fired);
    assert_eq!(
        lines
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(FIRED))
            .count(),
        1
    );
    let open = root(
        &hub,
        Verb::Source,
        "urn:iki:ledger:items",
        &[("as", "application/json")],
    );
    assert!(open.contains("\"count\":0"), "{open}");
}

#[test]
fn only_published_harness_scripts_are_sensors() {
    let (hub, scripts) = hub();
    let ask = "ASK FROM <urn:iki:ledger:graph:default> WHERE { ?s ?p ?o }";
    publish(
        &hub,
        &publisher("default", "open-by-anything"),
        "open-by-anything",
        ask,
    )
    .expect("a script that is no sensor");
    let draft = publisher("default", "harness-draft");
    issue(
        &hub,
        &draft,
        Verb::Sink,
        "urn:script:harness-draft",
        &[("language", "sparql"), ("content", ask), ("state", "draft")],
    )
    .expect("a draft");
    publish(
        &hub,
        &publisher("default", "harness-live"),
        "harness-live",
        ask,
    )
    .unwrap();
    let (sensors, _) = sensors(&scripts, None);
    let names: Vec<String> = block_on(sensors.tick(&hub, now_ms()))
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, ["harness-live"]);
}

// ------------------------------------------------------------- refused at publish

#[test]
fn a_sensor_whose_script_exceeds_its_publishers_grant_is_refused_at_publish() {
    let name = "harness-closed-without-evidence";
    let (hub, scripts) = hub();
    // This publisher reads `acme`, not `default`, which the sensor reads.
    let refused = publish(&hub, &publisher("acme", name), name, example(name))
        .expect_err("a sensor reading more than its publisher may is refused");
    assert!(matches!(refused, Error::Denied(_)), "{refused:?}");
    assert!(
        refused.to_string().contains("urn:iki:ledger:graph:default"),
        "the refusal names what the publisher lacks: {refused}"
    );
    let (sensors, _) = sensors(&scripts, None);
    assert!(
        block_on(sensors.tick(&hub, now_ms())).is_empty(),
        "nothing was published"
    );
}

#[test]
fn a_sensor_the_ceiling_withholds_or_that_is_no_ask_or_select_is_refused_at_publish() {
    let (hub, _) = hub();
    let name = "harness-elsewhere";
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.push("urn:cap:store:read:graph:urn:iki:browse:graph:default".to_string());
    scopes.extend(script_grants(name, ScriptAuthority::Publish).unwrap());
    let who = Capability::scoped(scopes).with_principal(PERSON).unwrap();
    let browse = "ASK FROM <urn:iki:browse:graph:default> WHERE { ?s ?p ?o }";
    let refused = publish(&hub, &who, name, browse).expect_err("beyond the ceiling");
    assert!(refused.to_string().contains("ceiling"), "{refused}");
    let construct = "CONSTRUCT { ?s ?p ?o } FROM <urn:iki:ledger:graph:default> WHERE { ?s ?p ?o }";
    let refused = publish(&hub, &who, name, construct).expect_err("not an ASK or SELECT");
    assert!(refused.to_string().contains("ASK or a SELECT"), "{refused}");
    // The same CONSTRUCT under a name that is no sensor's is the library's to take.
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.extend(script_grants("a-graph", ScriptAuthority::Publish).unwrap());
    let other = Capability::scoped(scopes).with_principal(PERSON).unwrap();
    publish(&hub, &other, "a-graph", construct).expect("not a sensor");
}
