//! Findings summaries through the same kernel composition the server uses.
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::{browse, compose_with};
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
