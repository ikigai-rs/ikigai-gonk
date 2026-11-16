//! Ledger [#3790](http://localhost:1062/l/default/item/3790): gonk records lapsed claims on a
//! timer. With `gonk.ledger.lapse.every` set, every ledger in the store is swept with
//! `sink urn:iki:ledger:{ledger}:lapse`, under that Sink's own tokens and gonk's recorder
//! principal, with no request from anyone. With the line absent nothing is recorded and no job
//! is scheduled. Recording only: no claim is released.
//!
//! Each test but the last runs a REAL gonk (the binary, in a child process, over a scratch config
//! home, data home and socket) and talks to it over its owner socket, as an operator would.

use std::time::{Duration, Instant};

mod scratch_gonk;

use ikigai_core::{ArgRef, Capability, Error, Iri, Representation, Request, Verb};
use ikigai_gonk::lapse;
use ikigai_ledger::Ledger;
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;
use serde_json::Value;

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

impl Gonk {
    fn over_socket(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }

    fn sink(&self, iri: &str, args: &[(&str, &str)]) -> String {
        let answer = self
            .over_socket(request(Verb::Sink, iri, args))
            .unwrap_or_else(|e| panic!("sink {iri}: {e}"));
        String::from_utf8_lossy(&answer.bytes).into_owned()
    }

    fn json(&self, iri: &str) -> Value {
        let answer = self
            .over_socket(request(Verb::Source, iri, &[("as", "application/json")]))
            .unwrap_or_else(|e| panic!("source {iri}: {e}"));
        serde_json::from_slice(&answer.bytes).unwrap_or_else(|e| panic!("{iri}: {e}"))
    }

    /// File an item in `ledger` and claim it as `holder` on a lease of `lease`.
    fn claimed(&self, ledger: &str, n: u32, holder: &str, lease: &str) {
        self.sink(
            &format!("urn:iki:ledger:{ledger}:append"),
            &[("content", "a title the sweep must never log")],
        );
        self.sink(
            &format!("urn:iki:ledger:{ledger}:claim"),
            &[
                ("item", &n.to_string()),
                ("content", holder),
                ("lease", lease),
            ],
        );
    }

    /// The `lapse` events in the history of item `n` of `ledger`.
    fn lapses(&self, ledger: &str, n: u32) -> Vec<Value> {
        let history = self.json(&format!("urn:iki:ledger:{ledger}:item:{n}:history"));
        history["events"]
            .as_array()
            .expect("a history's events")
            .iter()
            .filter(|event| event["kind"] == "lapse")
            .cloned()
            .collect()
    }

    fn status(&self) -> Value {
        self.json(lapse::STATUS)
    }

    fn status_text(&self) -> String {
        let answer = self
            .over_socket(request(Verb::Source, lapse::STATUS, &[]))
            .expect("the lapse status");
        String::from_utf8_lossy(&answer.bytes).into_owned()
    }

    /// The timer's run count, from the status.
    fn runs(&self) -> u64 {
        self.status()["timer"]["runs"].as_u64().unwrap_or(0)
    }
}

/// Wait up to 30 s for `done`.
fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// ★ THE REPRODUCTION. A claim on a 2-second lease runs out and nobody asks: the job records it,
/// once across several sweeps, as gonk's recorder; a renewed lease is not recorded; a second
/// ledger is swept too; and the status says what each sweep found.
#[test]
fn a_lapsed_lease_is_recorded_once_by_the_job_in_every_ledger() {
    let gonk = Gonk::start("gonk.ledger.lapse.every = \"1s\"\n");
    assert!(
        gonk.log().contains("lapses  every 1s"),
        "the banner says the sweep is on:\n{}",
        gonk.log()
    );

    gonk.claimed("default", 1, "worker-1", "2s");
    gonk.claimed("default", 2, "worker-2", "3s");
    gonk.claimed("acme", 1, "worker-3", "2s");

    // Item 2's holder renews every half second for six seconds: its lease never runs out.
    let renewed = Instant::now() + Duration::from_secs(6);
    while Instant::now() < renewed {
        gonk.sink(
            "urn:iki:ledger:default:claim",
            &[
                ("item", "2"),
                ("content", "worker-2"),
                ("lease", "3s"),
                ("renew", "true"),
            ],
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    // And then for long enough that it cannot run out before this test ends.
    gonk.sink(
        "urn:iki:ledger:default:claim",
        &[
            ("item", "2"),
            ("content", "worker-2"),
            ("lease", "10m"),
            ("renew", "true"),
        ],
    );

    until("the lapse in default", || {
        !gonk.lapses("default", 1).is_empty()
    });
    until("the lapse in acme", || !gonk.lapses("acme", 1).is_empty());
    assert!(
        gonk.lapses("default", 2).is_empty(),
        "a renewed lease is not a lapse: {:?}",
        gonk.lapses("default", 2)
    );

    // Several more sweeps, and each lapse is still recorded once.
    let runs = gonk.runs();
    until("three more sweeps", || gonk.runs() >= runs + 3);
    for (ledger, holder) in [("default", "worker-1"), ("acme", "worker-3")] {
        let events = gonk.lapses(ledger, 1);
        assert_eq!(events.len(), 1, "{ledger}: recorded once: {events:?}");
        assert_eq!(events[0]["holder"], holder, "{events:?}");
        assert_eq!(
            events[0]["committer"],
            lapse::PRINCIPAL,
            "{ledger}: the recorder is gonk's own principal, not a person: {events:?}"
        );
    }
    assert!(
        gonk.lapses("default", 2).is_empty(),
        "renewed, never lapsed"
    );
    // Recording only: the claim still stands.
    let item = gonk.json("urn:iki:ledger:default:item:1");
    assert_eq!(item["item"]["claim"]["holder"], "worker-1", "{item}");

    let status = gonk.status();
    assert_eq!(status["schedule"], "1s", "{status}");
    assert_eq!(status["principal"], lapse::PRINCIPAL, "{status}");
    assert!(status["timer"]["next_run_due"].is_string(), "{status}");
    let ledgers = status["ledgers"].as_array().expect("ledgers");
    let row = |name: &str| {
        ledgers
            .iter()
            .find(|row| row["ledger"] == name)
            .unwrap_or_else(|| panic!("{name} is swept: {status}"))
            .clone()
    };
    let default = row("default");
    assert_eq!(default["recorded_since_start"], 1, "{default}");
    assert_eq!(
        default["recorded"], 0,
        "the last sweep found it recorded: {default}"
    );
    assert_eq!(default["already_recorded"], 1, "{default}");
    assert!(default["error"].is_null(), "{default}");
    assert!(default["last_swept"].is_string(), "{default}");
    let acme = row("acme");
    assert_eq!(acme["recorded_since_start"], 1, "{acme}");
    assert_eq!(acme["already_recorded"], 1, "{acme}");

    let text = gonk.status_text();
    assert!(text.contains("Lapse sweep: every 1s."), "{text}");
    assert!(text.contains("It never releases a claim."), "{text}");
    assert!(text.contains("Next run: "), "{text}");
    assert!(
        text.contains("It recorded 0 lapse(s); 1 more were already recorded."),
        "{text}"
    );

    // The log says what was recorded in numbers, and never a title.
    let log = gonk.log();
    assert!(
        log.contains("recorded 1 lapse(s) in ledger `default` (item(s) #1)"),
        "{log}"
    );
    assert!(!log.contains("a title the sweep must never log"), "{log}");
}

/// With the line absent, gonk behaves as before: no job, and a lapse stays unrecorded.
#[test]
fn with_no_line_nothing_is_recorded_and_no_job_is_listed() {
    let gonk = Gonk::start("");
    assert!(gonk.log().contains("lapses  off"), "{}", gonk.log());
    gonk.claimed("default", 1, "worker-1", "2s");
    std::thread::sleep(Duration::from_secs(5));
    assert!(gonk.lapses("default", 1).is_empty());
    let stalled = gonk.json("urn:iki:ledger:default:stalled");
    assert_eq!(stalled["stalled"][0]["recorded"], false, "{stalled}");

    let status = gonk.status();
    assert!(status["schedule"].is_null(), "{status}");
    assert!(status["timer"].is_null(), "no job: {status}");
    assert_eq!(status["ledgers"], serde_json::json!([]), "{status}");
    assert!(gonk.status_text().starts_with("Lapse sweep: off."));
}

/// A malformed or zero interval stops gonk at start, naming the line.
#[test]
fn a_bad_interval_is_refused_at_start() {
    for bad in ["0s", "0", "often", "5 minutes", "500ms"] {
        let refused = Gonk::try_start(&format!("gonk.ledger.lapse.every = \"{bad}\"\n"))
            .err()
            .unwrap_or_else(|| panic!("`{bad}` started a gonk"));
        assert!(
            refused.contains("gonk.ledger.lapse.every"),
            "`{bad}`: {refused}"
        );
    }
}

/// What a ledger's sweep is sent under reaches that Sink and nothing else: no comment, claim,
/// filing or label, and nothing in another ledger. The tick itself fires under no scope at all.
#[test]
fn the_sweeps_capability_holds_the_lapse_tokens_and_nothing_else() {
    let kernel = ikigai_gonk::compose(ikigai_store::DurableStore::in_memory().unwrap());
    let root = Capability::root();
    for ledger in ["default", "acme"] {
        kernel
            .issue_as(
                request(
                    Verb::Sink,
                    &format!("urn:iki:ledger:{ledger}:append"),
                    &[("content", "one")],
                ),
                &root,
            )
            .unwrap();
    }
    let sweep = lapse::capability(&Ledger::parse("default").unwrap());
    let issue = |cap: &Capability, verb, iri: &str, args: &[(&str, &str)]| {
        kernel.issue_as(request(verb, iri, args), cap).map(|_| ())
    };

    issue(&sweep, Verb::Sink, "urn:iki:ledger:default:lapse", &[]).expect("the lapse Sink");
    for (iri, args) in [
        (
            "urn:iki:ledger:default:comment",
            &[("item", "1"), ("content", "hi")][..],
        ),
        (
            "urn:iki:ledger:default:claim",
            &[("item", "1"), ("content", "me")][..],
        ),
        ("urn:iki:ledger:default:append", &[("content", "two")][..]),
        (
            "urn:iki:ledger:default:label",
            &[("item", "1"), ("content", "x")][..],
        ),
        ("urn:iki:ledger:acme:lapse", &[][..]),
        ("urn:iki:ledger:acme:close", &[("item", "1")][..]),
    ] {
        let refused = issue(&sweep, Verb::Sink, iri, args);
        assert!(
            matches!(refused, Err(Error::Denied(_))),
            "{iri} under the sweep's capability: {refused:?}"
        );
    }
    assert!(issue(&sweep, Verb::Source, "urn:iki:ledger:default:items", &[]).is_err());

    let tick = lapse::job_capability();
    for (iri, args) in [
        ("urn:iki:ledger:default:lapse", &[][..]),
        ("urn:iki:ledger:default:close", &[("item", "1")][..]),
        (
            "urn:iki:ledger:default:comment",
            &[("item", "1"), ("content", "hi")][..],
        ),
    ] {
        let refused = issue(&tick, Verb::Sink, iri, args);
        assert!(
            matches!(refused, Err(Error::Denied(_))),
            "{iri} under the tick's capability: {refused:?}"
        );
    }
}
