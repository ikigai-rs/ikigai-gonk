//! Findings summaries through the same kernel composition the server uses.
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::{browse, compose_with, doors};
use ikigai_store::DurableStore;
use std::sync::Arc;
use tempfile::TempDir;
const ROOT: &str = "demo";
fn issue(hub: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let answer = block_on(hub.issue(request, &Capability::root())).unwrap();
    String::from_utf8(answer.bytes.to_vec()).unwrap()
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
    plant_on(hub, id, ROOT, "src/lib.rs", severity, body);
}

fn plant_on(hub: &Kernel, id: &str, repo: &str, path: &str, severity: Option<&str>, body: &str) {
    let target_path = path
        .split('/')
        .map(ikigai_fn::html::url_segment)
        .collect::<Vec<_>>()
        .join("/");
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
    prov:wasGeneratedBy <urn:ikigai:browse:review:{repo}:{target_path}> ;
{rated}    ik:annotates <urn:repo:{repo}:file:{target_path}> ;
    ik:repo "{repo}" ;
    ik:path "{path}" ;
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

#[test]
fn a_file_summary_counts_pending_findings_by_severity() {
    let (hub, _dir) = hub();
    plant(
        &hub,
        "0a0a0a0a0a0a0a0a0a0a0a01",
        Some("major"),
        "A pending claim.",
    );
    let listing: serde_json::Value = serde_json::from_str(&issue(
        &hub,
        Verb::Source,
        "urn:repo:demo:findings:src/lib.rs",
        &[("as", "application/json")],
    ))
    .unwrap();
    assert_eq!(
        listing.as_array().unwrap().len(),
        1,
        "the existing listing supplies the evidence"
    );
    let summary: serde_json::Value = serde_json::from_str(&issue(
        &hub,
        Verb::Source,
        "urn:repo:demo:findings-summary:src/lib.rs",
        &[],
    ))
    .unwrap();
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["files"][0]["severities"]["major"], 1);
}

fn summary(hub: &Kernel, suffix: &str) -> serde_json::Value {
    serde_json::from_str(&issue(
        hub,
        Verb::Source,
        &format!("urn:repo:demo:findings-summary{suffix}"),
        &[],
    ))
    .unwrap()
}

#[test]
fn summaries_count_all_severities_and_unrated_per_file_and_ignore_other_roots() {
    let (hub, dir) = hub();
    let encoded = "src/a%20b%2B%C3%A9.rs";
    let path = "src/a b+é.rs";
    std::fs::write(dir.path().join(path), "the first version\n").unwrap();
    plant(
        &hub,
        "100000000000000000000001",
        Some("major"),
        "First claim.",
    );
    plant(
        &hub,
        "100000000000000000000002",
        Some("major"),
        "Second claim.",
    );
    plant(&hub, "100000000000000000000003", None, "Unrated claim.");
    plant_on(
        &hub,
        "100000000000000000000004",
        ROOT,
        path,
        Some("minor"),
        "Other file.",
    );
    plant_on(
        &hub,
        "100000000000000000000005",
        "elsewhere",
        "src/lib.rs",
        Some("critical"),
        "Other root.",
    );
    assert_eq!(
        summary(&hub, ""),
        serde_json::json!({
            "repo": ROOT, "state": "pending", "total": 4,
            "files": [
                {"path": path, "total": 1, "severities": {"minor": 1}, "unrated": 0},
                {"path": "src/lib.rs", "total": 3, "severities": {"major": 2}, "unrated": 1}
            ]
        })
    );
    let one = summary(&hub, &format!(":{encoded}"));
    assert_eq!(one["total"], 1);
    assert_eq!(one["files"].as_array().unwrap().len(), 1);
    assert_eq!(one["files"][0]["path"], path);
    assert_eq!(one["files"][0]["severities"]["minor"], 1);
}

#[test]
fn empty_files_and_empty_roots_have_explicit_zero_counts() {
    let (hub, _dir) = hub();
    assert_eq!(
        summary(&hub, ""),
        serde_json::json!({
            "repo": ROOT, "state": "pending", "total": 0, "files": []
        })
    );
    assert_eq!(
        summary(&hub, ":src/missing.rs"),
        serde_json::json!({
            "repo": ROOT, "state": "pending", "total": 0,
            "files": [{"path": "src/missing.rs", "total": 0, "severities": {}, "unrated": 0}]
        })
    );
}

#[test]
fn decisions_and_retractions_are_visible_on_the_next_read() {
    let (hub, _dir) = hub();
    let id = "200000000000000000000001";
    let finding = format!("urn:iki:finding:{id}");
    plant(&hub, id, Some("major"), "One claim.");
    assert_eq!(summary(&hub, ":src/lib.rs")["total"], 1);
    issue(&hub, Verb::Sink, &finding, &[("decision", "publish")]);
    assert_eq!(summary(&hub, ":src/lib.rs")["total"], 0);
    issue(&hub, Verb::Delete, &format!("urn:iki:annotation:{id}"), &[]);
    issue(
        &hub,
        Verb::Sink,
        &finding,
        &[
            ("decision", "retract"),
            ("revises", &format!("{finding}:decision")),
        ],
    );
    assert_eq!(summary(&hub, ":src/lib.rs")["total"], 1);
    issue(&hub, Verb::Sink, &finding, &[("decision", "decline")]);
    assert_eq!(summary(&hub, "")["total"], 0);
}

#[test]
fn computed_supersession_is_counted_the_same_way_as_the_findings_listing() {
    let (hub, _dir) = hub();
    let older = "300000000000000000000001";
    let current = "300000000000000000000002";
    plant(&hub, older, Some("major"), "Old claim.");
    plant(&hub, current, Some("minor"), "Current claim.");
    let graph = browse::Graph::chosen()
        .named()
        .unwrap()
        .as_str()
        .to_string();
    // Planted review passes use browse's stored vocabulary. Neither hash matches the
    // current file, so the most recently derived reading stands, as in the Queue.
    let update = format!(
        r#"
PREFIX ik: <https://ikigai-rs.dev/ns#>
PREFIX prov: <http://www.w3.org/ns/prov#>
INSERT DATA {{ GRAPH <{graph}> {{
  <urn:ikigai:browse:review:demo:old:test:src/lib.rs>
    a ik:Review; ik:repo "demo"; ik:path "src/lib.rs";
    ik:contentHash "old"; ik:versionTag "test";
    ik:derivedAt "2026-10-01T00:00:00Z";
    prov:generated <urn:iki:finding:{older}> .
  <urn:ikigai:browse:review:demo:new:test:src/lib.rs>
    a ik:Review; ik:repo "demo"; ik:path "src/lib.rs";
    ik:contentHash "new"; ik:versionTag "test";
    ik:derivedAt "2026-10-02T00:00:00Z";
    prov:generated <urn:iki:finding:{current}> .
}} }}"#
    );
    issue(
        &hub,
        Verb::Sink,
        "urn:iki:store:graph-update",
        &[("graph", &graph), ("content", &update)],
    );
    let superseded: serde_json::Value = serde_json::from_str(&issue(
        &hub,
        Verb::Source,
        "urn:repo:demo:findings:src/lib.rs",
        &[("as", "application/json"), ("state", "superseded")],
    ))
    .unwrap();
    assert_eq!(
        superseded.as_array().unwrap().len(),
        1,
        "the fixture must actually supersede a finding"
    );
    assert_eq!(
        summary(&hub, ":src/lib.rs")["files"][0],
        serde_json::json!({
            "path": "src/lib.rs", "total": 1, "severities": {"minor": 1}, "unrated": 0
        })
    );
}

#[test]
fn a_browse_reader_can_read_the_summary_through_a_door_but_other_grants_cannot() {
    let (hub, _dir) = hub();
    let door = doors::door_kernel(Arc::clone(&hub));
    let reader = Capability::scoped(["urn:cap:browse:read:demo"]);
    let request = Request::new(
        Verb::Source,
        Iri::parse("urn:repo:demo:findings-summary:src/lib.rs").unwrap(),
    );
    let answer = block_on(door.issue(request.clone(), &reader)).unwrap();
    assert_eq!(answer.repr_type.media_type, "application/json");
    for grant in [
        "urn:cap:browse:read:elsewhere",
        "urn:cap:store:read",
        "urn:cap:ledger:read:default",
    ] {
        assert!(
            matches!(
                block_on(door.issue(request.clone(), &Capability::scoped([grant]))),
                Err(Error::Denied(_))
            ),
            "{grant}"
        );
    }
    let description = hub
        .describe_pattern("urn:repo:demo:findings-summary:{path}")
        .unwrap();
    assert_eq!(description.id, "gonk-findings-summary");
    for path in ["%2Fetc/passwd", "src/%2E%2E/lib.rs", "%FF"] {
        let request = Request::new(
            Verb::Source,
            Iri::parse(format!("urn:repo:demo:findings-summary:{path}")).unwrap(),
        );
        assert!(
            matches!(
                block_on(door.issue(request, &reader)),
                Err(Error::InvalidArgument { .. })
            ),
            "{path}"
        );
    }
}
