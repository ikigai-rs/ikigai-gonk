//! Ledger [#3717](http://localhost:1062/l/default/item/3717): the lapse sweep labels a lapsed item
//! when its ledger has a `gonk.ledger.lapse.label` line, a worker's grant cannot run the sweep,
//! and the Dashboard lists every stalled claim with its holder, its epoch and how long ago its
//! lease ran out. The sweep's tick itself is `tests/lapse_3790.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

mod scratch_gonk;

use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Representation, Request, Verb};
use ikigai_gonk::grants::{grants_for, grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
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

    fn sink(&self, iri: &str, args: &[(&str, &str)]) {
        self.over_socket(request(Verb::Sink, iri, args))
            .unwrap_or_else(|e| panic!("sink {iri}: {e}"));
    }

    fn json(&self, iri: &str) -> Value {
        let answer = self
            .over_socket(request(Verb::Source, iri, &[("as", "application/json")]))
            .unwrap_or_else(|e| panic!("source {iri}: {e}"));
        serde_json::from_slice(&answer.bytes).unwrap_or_else(|e| panic!("{iri}: {e}"))
    }

    fn lapses(&self, n: u32) -> usize {
        let history = self.json(&format!("urn:iki:ledger:default:item:{n}:history"));
        history["events"]
            .as_array()
            .expect("a history's events")
            .iter()
            .filter(|event| event["kind"] == "lapse")
            .count()
    }

    fn labels(&self, n: u32) -> Value {
        self.json(&format!("urn:iki:ledger:default:item:{n}"))["item"]["labels"].clone()
    }
}

fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A tick records one lapse for the item whose lease ran out and none for a live one, and the
/// configured label lands on the lapsed item alone.
#[test]
fn a_tick_records_one_lapse_and_labels_it_when_the_ledger_has_a_lapse_label() {
    let gonk = Gonk::start(
        "gonk.ledger.lapse.every = \"1s\"\ngonk.ledger.lapse.label = \"default needs-person\"\n",
    );
    for (n, lease) in [(1, "2s"), (2, "10m")] {
        gonk.sink(
            "urn:iki:ledger:default:append",
            &[("content", "a title the sweep must never log")],
        );
        gonk.sink(
            "urn:iki:ledger:default:claim",
            &[
                ("item", &n.to_string()),
                ("content", &format!("worker-{n}")),
                ("lease", lease),
            ],
        );
    }
    until("the lapse of item 1", || gonk.lapses(1) > 0);
    // A few more ticks, and still one lapse.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(gonk.lapses(1), 1, "one lapse per lease end");
    assert_eq!(gonk.lapses(2), 0, "a live lease is not a lapse");
    assert_eq!(gonk.labels(1), serde_json::json!(["needs-person"]));
    assert_eq!(gonk.labels(2), serde_json::json!([]));
    let log = gonk.log();
    assert!(!log.contains("a title the sweep must never log"), "{log}");
}

/// With no label line a lapse is recorded and no label is added: the route is off by default.
#[test]
fn with_no_label_line_a_lapse_adds_no_label() {
    let gonk = Gonk::start("gonk.ledger.lapse.every = \"1s\"\n");
    gonk.sink("urn:iki:ledger:default:append", &[("content", "one")]);
    gonk.sink(
        "urn:iki:ledger:default:claim",
        &[("item", "1"), ("content", "worker-1"), ("lease", "2s")],
    );
    until("the lapse of item 1", || gonk.lapses(1) > 0);
    assert_eq!(gonk.labels(1), serde_json::json!([]));
}

/// A label line the ledger would refuse stops gonk at start, naming the line.
#[test]
fn a_bad_label_line_is_refused_at_start() {
    for bad in [
        "default",
        "default needs person",
        "default a\"",
        "a/b needs-person",
    ] {
        let line = format!(
            "gonk.ledger.lapse.label = \"{}\"\n",
            bad.replace('"', "\\\"")
        );
        let refused = Gonk::try_start(&line)
            .err()
            .unwrap_or_else(|| panic!("`{bad}` started a gonk"));
        assert!(
            refused.contains("gonk.ledger.lapse.label"),
            "`{bad}`: {refused}"
        );
    }
}

/// A worker's grant reads the stalled list but cannot run the sweep: `lapse` is the
/// dispatcher's (close or write).
#[test]
fn a_worker_identity_cannot_run_the_sweep() {
    let kernel = compose(ikigai_store::DurableStore::in_memory().unwrap());
    kernel
        .issue_as(
            request(
                Verb::Sink,
                "urn:iki:ledger:default:append",
                &[("content", "one")],
            ),
            &Capability::root(),
        )
        .unwrap();
    let worker = Capability::scoped(grants_for("default", Authority::Worker).unwrap())
        .with_principal("urn:iki:agent:worker-1")
        .unwrap();
    let refused = kernel
        .issue_as(
            request(Verb::Sink, "urn:iki:ledger:default:lapse", &[]),
            &worker,
        )
        .map(|_| ());
    assert!(
        matches!(refused, Err(Error::Denied(_))),
        "a worker ran the sweep: {refused:?}"
    );
    kernel
        .issue_as(
            request(
                Verb::Source,
                "urn:iki:ledger:default:stalled",
                &[("as", "application/json")],
            ),
            &worker,
        )
        .expect("a worker reads the stalled list");
    let writer = Capability::scoped(grants_for("default", Authority::Write).unwrap());
    kernel
        .issue_as(
            request(Verb::Sink, "urn:iki:ledger:default:lapse", &[]),
            &writer,
        )
        .expect("the dispatcher's write grant runs the sweep");
}

/// The hub and the HTTP door's kernel over one in-memory store, as `tests/dashboard_2542.rs`
/// builds them.
fn dashboard_kernels(configured: &[&str]) -> (Arc<Kernel>, Kernel, tempfile::TempDir) {
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let hub = Arc::new(compose(ikigai_store::DurableStore::in_memory().unwrap()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: configured.iter().map(|s| s.to_string()).collect(),
        browse_roots: Vec::new().into(),
        passkeys: Arc::new(Passkeys::new(layout, 1062)),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        display: Default::default(),
        page_assets: Default::default(),
        projects: Default::default(),
        toolbox: false,
        faces: Vec::new(),
        inquiry: None,
    });
    let http = doors::http_kernel(Arc::clone(&hub), web::space(face));
    (hub, http, config)
}

fn dashboard(http: &Kernel, reader: &Capability) -> String {
    let page = request(Verb::Source, ikigai_gonk::dashboard::PAGE_IRI, &[]);
    let html = futures::executor::block_on(Kernel::issue(http, page, reader))
        .expect("the Dashboard renders");
    String::from_utf8_lossy(&html.bytes).into_owned()
}

/// The Dashboard lists the stalled claim, with its holder, epoch, how long ago and whether its
/// lapse is recorded, and not the live one; a ledger the viewer may not read is left out.
#[test]
fn a_dashboard_with_no_stalled_claim_says_so() {
    let (_hub, http, _config) = dashboard_kernels(&["default"]);
    let reader =
        Capability::scoped(grants_for_all(&["default".to_string()], Authority::Read).unwrap());
    let page = dashboard(&http, &reader);
    assert!(
        page.contains("<h3 id=\"dashboard-stalled\">Stalled claims</h3>"),
        "{page}"
    );
    assert!(page.contains("No claim has stalled"), "{page}");
    assert!(
        !page.contains("aria-label=\"Stalled claims\""),
        "no empty table: {page}"
    );
}

#[test]
fn the_dashboard_lists_a_stalled_claim_with_its_holder_epoch_and_age() {
    let (hub, http, _config) = dashboard_kernels(&["default", "acme"]);
    let root = Capability::root();
    let issue = |iri: &str, args: &[(&str, &str)]| {
        futures::executor::block_on(Kernel::issue(&hub, request(Verb::Sink, iri, args), &root))
            .unwrap_or_else(|e| panic!("{iri}: {e}"));
    };
    for ledger in ["default", "acme"] {
        for (n, lease) in [(1, "1s"), (2, "10m")] {
            issue(
                &format!("urn:iki:ledger:{ledger}:append"),
                &[("content", &format!("<b>{ledger} item {n}</b>"))],
            );
            issue(
                &format!("urn:iki:ledger:{ledger}:claim"),
                &[
                    ("item", &n.to_string()),
                    ("content", &format!("worker-{n}")),
                    ("lease", lease),
                ],
            );
        }
    }
    let reader =
        Capability::scoped(grants_for_all(&["default".to_string()], Authority::Read).unwrap());

    std::thread::sleep(Duration::from_millis(1500));
    let page = dashboard(&http, &reader);
    let section = &page[page.find("dashboard-stalled").expect("the section")..];
    let section = &section[..section.find("</section>").expect("its end")];
    assert!(
        section.contains("1 open item is held on a lease that has run out"),
        "{section}"
    );
    assert!(
        section.contains("<a href=\"/l/default/item/1\">#1</a>"),
        "{section}"
    );
    assert!(
        section.contains("&lt;b&gt;default item 1&lt;/b&gt;"),
        "the title is text: {section}"
    );
    assert!(section.contains("<td>worker-1</td>"), "{section}");
    assert!(section.contains("<td>1</td>"), "the epoch: {section}");
    assert!(section.contains(" ago, <time datetime=\"20"), "{section}");
    assert!(section.contains("<td>Not yet</td>"), "{section}");
    assert!(
        !section.contains("worker-2"),
        "a live lease is not stalled: {section}"
    );
    assert!(
        !section.contains("acme"),
        "an unreadable ledger is left out: {section}"
    );
    assert!(
        section.contains("<th scope=\"col\">Lease ran out</th>"),
        "{section}"
    );

    // Once the sweep has recorded the lapse, the row says so.
    issue("urn:iki:ledger:default:lapse", &[]);
    let page = dashboard(&http, &reader);
    assert!(page.contains("<td>Yes</td>"), "{page}");
}
