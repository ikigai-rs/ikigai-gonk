//! `ikigai-gonk findings file` (ledger #1453), end to end: the real binary, over the owner-only
//! socket door exactly as `main` builds it, against a hub that composes the browse family over a
//! scratch root and an in-memory store.
//!
//! No model is called, live or stubbed: the findings are PLANTED in browse's graph with the
//! quads `ikigai-browse` writes for a pending finding (the fixture `tests/queue.rs` uses), and a
//! person's decisions are made through the finding Sink itself, as the Queue makes them. So what
//! this tests is gonk's half: which rows it files, how, and that a re-run files nothing.
//!
//! - [`published_findings_are_filed_once_and_pending_ones_never`] — a dry run files nothing;
//!   the run files the published serious findings (one rated serious by the person who published
//!   it, the model having proposed nothing) keyed by the finding's
//!   IRI, about the file, the finding and the annotation; it never files a pending or declined
//!   one, nor a published one below the serious set; a second run answers `existing` for each.
//! - [`severity_all_and_reproduced_change_what_is_filed`] — `--severity all` files the published
//!   one below the set too; `--reproduced` files only the one marked reproduced.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

mod ready;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::config::QueuePolicy;
use ikigai_gonk::{browse, compose_with, doors, queue};
use ikigai_store::DurableStore;
use tempfile::TempDir;

const ROOT: &str = "demo";

/// Five findings, each in a different place in the pipeline.
const SERIOUS: &str = "0a0a0a0a0a0a0a0a0a0a0a01";
const RATED_AT_PUBLISH: &str = "0a0a0a0a0a0a0a0a0a0a0a02";
const MILD: &str = "0a0a0a0a0a0a0a0a0a0a0a03";
const PENDING: &str = "0a0a0a0a0a0a0a0a0a0a0a04";
const DECLINED: &str = "0a0a0a0a0a0a0a0a0a0a0a05";

fn issue(hub: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("an IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let answer = block_on(Kernel::issue(hub, request, &Capability::root()))
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

/// A scratch root, and a hub composing browse over it and an in-memory store, the way `main`
/// composes them.
fn hub() -> (Arc<Kernel>, TempDir) {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir(dir.path().join("src")).expect("src/");
    std::fs::write(dir.path().join("src/lib.rs"), "the first version\n").expect("lib.rs");
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store that declares where its sharer writes");
    let wired = browse::wire(
        vec![(ROOT.to_string(), dir.path().to_path_buf())],
        store
            .shared_writer()
            .expect("a shared store hands out its writer"),
        None,
        None,
        &graph,
        None,
    );
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
    ));
    (hub, dir)
}

/// A pending finding, as `ikigai-browse` stores one (`tests/queue.rs::plant_finding_on`).
fn plant(hub: &Kernel, id: &str, severity: Option<&str>, body: &str) {
    let graph = browse::Graph::chosen()
        .named()
        .expect("this server writes browse's quads in a NAMED graph")
        .as_str()
        .to_string();
    let rated = match severity {
        Some(word) => format!("    sh:resultSeverity <urn:iki:severity:{word}> ;\n"),
        None => String::new(),
    };
    let update = format!(
        r#"PREFIX oa: <http://www.w3.org/ns/oa#>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX sh: <http://www.w3.org/ns/shacl#>
PREFIX ik: <https://ikigai-rs.dev/ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
INSERT DATA {{ GRAPH <{graph}> {{
  <urn:iki:finding:{id}> a prov:Entity ;
    dcterms:description "{body}" ;
    dcterms:creator "a-test-reviewer" ;
    dcterms:created "2026-10-13T12:00:00Z"^^xsd:dateTime ;
    prov:wasGeneratedBy <urn:ikigai:browse:review:{ROOT}:src/lib.rs> ;
{rated}    ik:annotates <urn:repo:{ROOT}:file:src/lib.rs> ;
    ik:repo "{ROOT}" ;
    ik:path "src/lib.rs" ;
    ik:contentHash "sha256:planted" ;
    oa:hasSelector <urn:iki:finding:{id}:selector:quote> ,
                   <urn:iki:finding:{id}:selector:position> .
  <urn:iki:finding:{id}:selector:quote> a oa:TextQuoteSelector ;
    oa:exact "first version" .
  <urn:iki:finding:{id}:selector:position> a oa:TextPositionSelector ;
    oa:start "4"^^xsd:nonNegativeInteger ;
    oa:end "17"^^xsd:nonNegativeInteger .
}} }}"#
    );
    issue(
        hub,
        Verb::Sink,
        "urn:iki:store:graph-update",
        &[("graph", &graph), ("content", &update)],
    );
}

/// The contract's word for one argument of the finding Sink — never spelled here.
fn word(hub: &Kernel, argument: &str, nth: usize) -> String {
    queue::one_of(
        hub,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        argument,
    )
    .unwrap_or_else(|| panic!("the finding Sink declares `{argument}`"))
    .into_iter()
    .nth(nth)
    .unwrap_or_else(|| panic!("`{argument}` has a word {nth}"))
}

/// The pipeline: one of each, decided through the finding Sink as the Queue decides.
/// Returns the severity words used: (serious, mild).
fn the_pipeline(hub: &Kernel) -> (String, String) {
    let declared = queue::check_serious(hub, &QueuePolicy::default()).expect("the contract");
    let policy = QueuePolicy::default();
    let serious = policy.serious[0].clone();
    let mild = queue::other_severities(&declared, &policy)
        .into_iter()
        .next()
        .expect("a severity outside the serious set");
    plant(
        hub,
        SERIOUS,
        Some(&serious),
        "Slicing the first byte panics.",
    );
    plant(
        hub,
        RATED_AT_PUBLISH,
        None,
        "The model rated this one nothing.",
    );
    plant(hub, MILD, Some(&mild), "A remark below the serious set.");
    plant(hub, PENDING, Some(&serious), "Nobody has looked at this.");
    plant(hub, DECLINED, Some(&serious), "A person said no.");
    let reproduced = word(hub, queue::REPRODUCED_ARG, 0);
    let sink = |id: &str, args: &[(&str, &str)]| {
        issue(hub, Verb::Sink, &format!("urn:iki:finding:{id}"), args);
    };
    sink(
        SERIOUS,
        &[
            ("decision", "publish"),
            ("reproduced", &reproduced),
            ("content", "a failing test"),
        ],
    );
    // The model proposed no severity, so the person states one: the finding Sink refuses a
    // decision on an unrated finding that does not.
    sink(
        RATED_AT_PUBLISH,
        &[("decision", "publish"), ("severity", &serious)],
    );
    sink(MILD, &[("decision", "publish")]);
    sink(DECLINED, &[("decision", "decline")]);
    (serious, mild)
}

/// The socket door over `hub`, served in-process; its path.
fn serve(hub: &Arc<Kernel>, dir: &Path) -> PathBuf {
    let socket = dir.join("g.sock");
    let door = doors::door_kernel(Arc::clone(hub));
    let path = socket.clone();
    std::thread::spawn(move || ikigai_ipc::serve(door, &path));
    ready::socket(&socket);
    socket
}

/// The binary, with every home a scratch one, so nothing of this machine's gonk is read.
fn file(socket: &Path, homes: &Path, extra: &[&str]) -> Output {
    let config = homes.join("config.toml");
    if !config.exists() {
        std::fs::write(&config, "").expect("an empty config");
    }
    std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["findings", "file", ROOT, "--socket"])
        .arg(socket)
        .arg("--config")
        .arg(&config)
        .arg("--config-home")
        .arg(homes)
        .arg("--data-home")
        .arg(homes)
        .args(extra)
        .output()
        .expect("run ikigai-gonk")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The ledger item holding `key`, in the ledger's JSON face, or `None`.
fn keyed(hub: &Kernel, key: &str) -> Option<serde_json::Value> {
    let listing = issue(
        hub,
        Verb::Source,
        "urn:iki:ledger:items",
        &[("status", "all"), ("key", key), ("as", "application/json")],
    );
    let json: serde_json::Value = serde_json::from_str(&listing).expect("the items' JSON face");
    let items = json["items"].as_array().expect("an items array").clone();
    assert!(items.len() <= 1, "two items hold {key}: {listing}");
    items.into_iter().next()
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn published_findings_are_filed_once_and_pending_ones_never() {
    let (hub, _root) = hub();
    let (serious, mild) = the_pipeline(&hub);
    let homes = tempfile::tempdir().expect("scratch homes");
    let socket = serve(&hub, homes.path());

    // A dry run plans and files nothing.
    let dry = stdout(&file(&socket, homes.path(), &["--dry-run"]));
    assert!(dry.contains(&format!("would file  {SERIOUS}")), "{dry}");
    assert!(
        dry.contains(&format!("would file  {RATED_AT_PUBLISH}")),
        "{dry}"
    );
    assert!(
        dry.contains("dry run, nothing filed: 3 published finding(s) on demo: 2 would be filed"),
        "{dry}"
    );
    assert!(keyed(&hub, &format!("urn:iki:finding:{SERIOUS}")).is_none());

    // The run files the two published serious findings, and nothing else.
    let run = stdout(&file(&socket, homes.path(), &[]));
    assert!(
        run.contains("filed    #1  Slicing the first byte panics."),
        "{run}"
    );
    assert!(
        run.contains("filed    #2  The model rated this one nothing."),
        "{run}"
    );
    assert!(
        run.contains(&format!(
            "skipped  {MILD}  {mild} is not in gonk.queue.serious"
        )),
        "{run}"
    );
    assert!(
        run.contains("3 published finding(s) on demo: 2 filed, 0 already filed, 1 skipped"),
        "{run}"
    );
    for never in [PENDING, DECLINED, MILD] {
        assert!(
            keyed(&hub, &format!("urn:iki:finding:{never}")).is_none(),
            "{never} was filed:\n{run}"
        );
    }

    let item = keyed(&hub, &format!("urn:iki:finding:{SERIOUS}")).expect("the serious item");
    assert_eq!(item["key"], format!("urn:iki:finding:{SERIOUS}"));
    // The ledger holds `about` as a set; it answers it sorted.
    let mut about = strings(&item["about"]);
    about.sort();
    let mut want_about = vec![
        format!("urn:repo:{ROOT}:file:src/lib.rs"),
        format!("urn:iki:finding:{SERIOUS}"),
        format!("urn:iki:annotation:{SERIOUS}"),
    ];
    want_about.sort();
    assert_eq!(about, want_about, "{item}");
    let mut labels = strings(&item["labels"]);
    labels.sort();
    let mut want = vec![
        "finding".to_string(),
        format!("repo:{ROOT}"),
        serious.clone(),
        "reproduced".to_string(),
    ];
    want.sort();
    assert_eq!(labels, want, "{item}");
    // The person's rating is the one used: the model proposed none.
    let rated = keyed(&hub, &format!("urn:iki:finding:{RATED_AT_PUBLISH}"))
        .expect("the item rated at publish");
    let labels = strings(&rated["labels"]);
    assert!(labels.contains(&serious), "{rated}");
    assert!(!labels.contains(&"reproduced".to_string()), "{rated}");

    // A second run files nothing: each key answers the item that holds it.
    let again = stdout(&file(&socket, homes.path(), &[]));
    assert!(
        again.contains(&format!("already  #1 (open)  urn:iki:finding:{SERIOUS}")),
        "{again}"
    );
    assert!(
        again.contains("3 published finding(s) on demo: 0 filed, 2 already filed, 1 skipped"),
        "{again}"
    );
}

#[test]
fn severity_all_and_reproduced_change_what_is_filed() {
    let (hub, _root) = hub();
    the_pipeline(&hub);
    let homes = tempfile::tempdir().expect("scratch homes");
    let socket = serve(&hub, homes.path());

    let reproduced = stdout(&file(&socket, homes.path(), &["--reproduced"]));
    assert!(
        reproduced.contains("3 published finding(s) on demo: 1 filed, 0 already filed, 2 skipped"),
        "{reproduced}"
    );
    assert!(keyed(&hub, &format!("urn:iki:finding:{SERIOUS}")).is_some());
    assert!(keyed(&hub, &format!("urn:iki:finding:{RATED_AT_PUBLISH}")).is_none());

    let all = stdout(&file(&socket, homes.path(), &["--severity", "all"]));
    assert!(
        all.contains("3 published finding(s) on demo: 2 filed, 1 already filed, 0 skipped"),
        "{all}"
    );
    assert!(keyed(&hub, &format!("urn:iki:finding:{MILD}")).is_some());
    for never in [PENDING, DECLINED] {
        assert!(keyed(&hub, &format!("urn:iki:finding:{never}")).is_none());
    }
}
