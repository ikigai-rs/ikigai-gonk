//! `urn:sparql:*` — the query face over the store, scoped per graph by the caller's grant
//! (ledger [#836](http://localhost:1060/l/default/item/836)).
//!
//! Two graphs with different grants, and a caller that may read one of them:
//!
//! - [`a_one_graph_caller_reads_only_its_graph_by_every_route`] — the union default, the graph
//!   named, and `GRAPH ?g`, each confined to the graph the grant names;
//! - [`a_named_graph_the_caller_cannot_read_is_denied_whether_or_not_it_exists`] — the face
//!   is not an existence oracle;
//! - [`root_reads_the_union_of_every_named_graph`] — the dev server's union semantics, which
//!   the existing clients rely on (ledger [#378](http://localhost:1060/l/default/item/378));
//! - [`no_write_is_reachable_under_urn_sparql`] — sealed;
//! - [`the_default_dataset_is_stated_in_the_meta`] — the sentence #378 asked for, read through
//!   the JSON Meta face a mounting client reads;
//! - [`a_class_query_runs_end_to_end_through_a_mounted_socket`] — `review-queries.md` §3 over a
//!   real Unix socket, through the `MountedRemote` a `web.mount` line builds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::{compose, doors, sparql};
use ikigai_store::{cap_read_graph, DurableStore};

const BROWSE: &str = "urn:iki:browse:graph:default";
const LEDGER: &str = "urn:iki:ledger:graph:default";
/// A graph nothing has written and nobody holds a grant for.
const NOWHERE: &str = "urn:iki:test:graph:nowhere";

/// One human note and two machine findings, in the shape `ikigai-browse` writes.
const ANNOTATIONS: &str = r#"
@prefix oa: <http://www.w3.org/ns/oa#> .
@prefix dct: <http://purl.org/dc/terms/> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:iki:annotation:a1> a oa:Annotation ; ik:annotates <urn:repo:demo:file:a.rs> ;
    oa:bodyValue "a finding" ; dct:creator "qwen3-coder:30b" .
<urn:iki:annotation:a2> a oa:Annotation ; ik:annotates <urn:repo:demo:file:b.rs> ;
    oa:bodyValue "another finding" ; dct:creator "qwen3-coder:30b" .
<urn:iki:annotation:a3> a oa:Annotation ; ik:annotates <urn:repo:demo:file:a.rs> ;
    oa:bodyValue "a human note" .
"#;

const LEDGER_TTL: &str = r#"
<urn:iki:ledger:default:item:x> <http://purl.org/dc/terms/title> "a ledger item" .
"#;

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// A hub over an in-memory store holding the two graphs.
fn hub() -> Arc<Kernel> {
    let hub = Arc::new(compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ));
    for (graph, ttl) in [(BROWSE, ANNOTATIONS), (LEDGER, LEDGER_TTL)] {
        block_on(Kernel::issue(
            &hub,
            request(
                Verb::Sink,
                "urn:iki:store:load",
                &[("content", ttl), ("format", "text/turtle"), ("graph", graph)],
            ),
            &Capability::root(),
        ))
        .unwrap_or_else(|e| panic!("loading {graph}: {e}"));
    }
    hub
}

fn ask(
    kernel: &Kernel,
    cap: &Capability,
    form: &str,
    args: &[(&str, &str)],
) -> Result<String, Error> {
    block_on(Kernel::issue(
        kernel,
        request(Verb::Source, &format!("urn:sparql:{form}"), args),
        cap,
    ))
    .map(|repr| String::from_utf8_lossy(&repr.bytes).into_owned())
}

/// The CSV rows of a SELECT, header dropped, sorted.
fn rows(csv: &str) -> Vec<String> {
    let mut rows: Vec<String> = csv
        .lines()
        .skip(1)
        .map(|line| line.trim_end_matches('\r').to_string())
        .filter(|line| !line.is_empty())
        .collect();
    rows.sort();
    rows
}

fn browse_only() -> Capability {
    Capability::scoped([cap_read_graph(BROWSE)])
}

const SUBJECTS: &str = "SELECT DISTINCT ?s WHERE { ?s ?p ?o }";

#[test]
fn a_one_graph_caller_reads_only_its_graph_by_every_route() {
    let hub = hub();
    let cap = browse_only();
    let annotations = vec![
        "urn:iki:annotation:a1".to_string(),
        "urn:iki:annotation:a2".to_string(),
        "urn:iki:annotation:a3".to_string(),
    ];

    // The union default: the readable set is the browse graph alone.
    let union = ask(
        &hub,
        &cap,
        "select",
        &[("query", SUBJECTS), ("as", "text/csv")],
    )
    .unwrap();
    assert_eq!(rows(&union), annotations, "{union}");

    // The graph named.
    let named = ask(
        &hub,
        &cap,
        "select",
        &[("query", SUBJECTS), ("graph", BROWSE), ("as", "text/csv")],
    )
    .unwrap();
    assert_eq!(rows(&named), annotations, "{named}");

    // `GRAPH ?g` binds only the graphs in the dataset.
    let graphs = ask(
        &hub,
        &cap,
        "select",
        &[
            ("query", "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } }"),
            ("as", "text/csv"),
        ],
    )
    .unwrap();
    assert_eq!(rows(&graphs), [BROWSE], "{graphs}");

    // And `GRAPH <ledger>` matches nothing rather than reaching around the grant.
    let around = ask(
        &hub,
        &cap,
        "ask",
        &[(
            "query",
            &format!("ASK {{ GRAPH <{LEDGER}> {{ ?s ?p ?o }} }}"),
        )],
    )
    .unwrap();
    assert!(around.contains("false"), "{around}");
}

#[test]
fn a_named_graph_the_caller_cannot_read_is_denied_whether_or_not_it_exists() {
    let hub = hub();
    let cap = browse_only();
    for graph in [LEDGER, NOWHERE, &format!("{BROWSE} {LEDGER}")] {
        match ask(&hub, &cap, "select", &[("query", SUBJECTS), ("graph", graph)]) {
            Err(Error::Denied(why)) => assert!(why.contains("urn:cap:store:read:graph:"), "{why}"),
            other => panic!("graph={graph}: expected Denied, got {other:?}"),
        }
    }
    // The existing graph and the absent one are refused in the same words, so the refusal
    // says nothing about which of them is there.
    let words = |graph: &str| match ask(&hub, &cap, "select", &[("query", SUBJECTS), ("graph", graph)])
    {
        Err(Error::Denied(why)) => why.replace(graph, "<G>"),
        other => panic!("{other:?}"),
    };
    assert_eq!(words(LEDGER), words(NOWHERE));
}

#[test]
fn root_reads_the_union_of_every_named_graph() {
    let hub = hub();
    let root = Capability::root();
    let all = ask(
        &hub,
        &root,
        "select",
        &[("query", SUBJECTS), ("as", "text/csv")],
    )
    .unwrap();
    let all = rows(&all);
    assert!(all.contains(&"urn:iki:ledger:default:item:x".to_string()), "{all:?}");
    assert!(all.contains(&"urn:iki:annotation:a1".to_string()), "{all:?}");

    // Comma-separated, as `urn:sparql:*` has always taken it.
    let both = ask(
        &hub,
        &root,
        "select",
        &[
            ("query", SUBJECTS),
            ("graph", &format!("{LEDGER},{BROWSE}")),
            ("as", "text/csv"),
        ],
    )
    .unwrap();
    assert_eq!(rows(&both), all);

    // CONSTRUCT answers Turtle when no `as` is given — ikigai-sparql's default, not the store's.
    let graph = block_on(Kernel::issue(
        &hub,
        request(
            Verb::Source,
            "urn:sparql:construct",
            &[("query", "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }")],
        ),
        &root,
    ))
    .unwrap();
    assert_eq!(graph.repr_type.media_type, "text/turtle");
}

#[test]
fn a_caller_that_reads_no_graph_is_refused_not_answered_empty() {
    let hub = hub();
    // A grant under the family that names a graph nothing has written.
    let cap = Capability::scoped([cap_read_graph(NOWHERE)]);
    match ask(&hub, &cap, "select", &[("query", SUBJECTS)]) {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "graph");
            assert!(detail.contains("urn:iki:store:graphs"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    // A caller with no store grant at all is stopped by the declared floor.
    match ask(&hub, &Capability::scoped(Vec::<String>::new()), "select", &[("query", SUBJECTS)]) {
        Err(Error::Denied(_)) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn malformed_and_crossed_queries_are_invalid_arguments() {
    let hub = hub();
    let root = Capability::root();
    for (form, query) in [
        ("select", "SELECT WHERE {"),
        ("select", "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }"),
        ("construct", "SELECT * WHERE { ?s ?p ?o }"),
        ("select", "SELECT * FROM <urn:x> WHERE { ?s ?p ?o }"),
    ] {
        match ask(&hub, &root, form, &[("query", query)]) {
            Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "query", "{query}"),
            other => panic!("{form} {query}: {other:?}"),
        }
    }
}

#[test]
fn no_write_is_reachable_under_urn_sparql() {
    let hub = hub();
    let root = Capability::root();
    let count = || {
        rows(
            &ask(
                &hub,
                &root,
                "select",
                &[
                    ("query", "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }"),
                    ("as", "text/csv"),
                ],
            )
            .unwrap(),
        )
    };
    // Eleven annotation triples and one ledger triple.
    assert_eq!(count(), ["12"]);
    // No update IRI is bound.
    match block_on(Kernel::issue(
        &hub,
        request(
            Verb::Sink,
            "urn:sparql:update",
            &[("content", "DROP ALL")],
        ),
        &root,
    )) {
        Err(Error::Unresolved(_)) => {}
        other => panic!("urn:sparql:update must be unbound: {other:?}"),
    }
    // A Sink on a query form is refused.
    assert!(block_on(Kernel::issue(
        &hub,
        request(
            Verb::Sink,
            "urn:sparql:select",
            &[("content", "DROP ALL")],
        ),
        &root,
    ))
    .is_err());
    // An update passed as the query is not a query.
    for update in [
        "DROP ALL",
        "INSERT DATA { GRAPH <urn:iki:browse:graph:default> { <urn:x> <urn:y> <urn:z> } }",
    ] {
        match ask(&hub, &root, "select", &[("query", update)]) {
            Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "query"),
            other => panic!("{update}: {other:?}"),
        }
    }
    assert_eq!(count(), ["12"], "nothing was written");
}

#[test]
fn the_default_dataset_is_stated_in_the_meta() {
    let door = doors::door_kernel(hub());
    for form in ["select", "ask", "construct", "describe"] {
        let meta = block_on(Kernel::issue(
            &door,
            request(
                Verb::Meta,
                &format!("urn:sparql:{form}"),
                &[("as", "application/json")],
            ),
            &Capability::root(),
        ))
        .unwrap_or_else(|e| panic!("{form}: {e}"));
        let json: serde_json::Value = serde_json::from_slice(&meta.bytes).unwrap();
        let text = json.to_string();
        // The JSON escapes nothing in the sentence but its quotes, which it has none of.
        assert!(
            text.contains(sparql::DEFAULT_DATASET),
            "{form}: the default dataset is not stated: {text}"
        );
        assert!(text.contains("urn:cap:store:read:graph:*"), "{form}: {text}");
    }
}

/// `review-queries.md` §3, "Machine vs human", as a `web.mount` line runs it: an ordinary
/// client kernel whose `urn:sparql:` is gonk's socket, mounted the way the cli composes a
/// `prefer` line, and the query issued under root — which is what `ikigai-web` sends.
#[test]
fn a_class_query_runs_end_to_end_through_a_mounted_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("gonk.sock");
    let (door, path) = (doors::door_kernel(hub()), socket.clone());
    std::thread::spawn(move || ikigai_ipc::serve(door, &path));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "the socket never appeared");
        std::thread::sleep(Duration::from_millis(50));
    }
    let resolver = ikigai_ipc::connect(&socket).expect("connect");
    let client = Kernel::with_meta_renderer(
        Arc::new(ikigai_resolve::MountedRemote::overriding(
            Arc::new(resolver),
            "urn:sparql:",
            socket.display().to_string(),
        )),
        Arc::new(ikigai_vocab::TurtleRenderer),
    );
    let query = "PREFIX oa: <http://www.w3.org/ns/oa#>
PREFIX dct: <http://purl.org/dc/terms/>
SELECT ?creator (COUNT(?a) AS ?notes) WHERE {
  ?a a oa:Annotation .
  OPTIONAL { ?a dct:creator ?creator }
} GROUP BY ?creator";
    let answer = block_on(Kernel::issue(
        &client,
        request(Verb::Source, "urn:sparql:select", &[("query", query)]),
        &Capability::root(),
    ))
    .expect("the class query over the mount");
    assert_eq!(
        answer.repr_type.media_type,
        "application/sparql-results+json"
    );
    let json: serde_json::Value = serde_json::from_slice(&answer.bytes).unwrap();
    let mut counts: Vec<(String, String)> = json["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["creator"]["value"].as_str().unwrap_or("").to_string(),
                row["notes"]["value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    counts.sort();
    assert_eq!(
        counts,
        [
            (String::new(), "1".to_string()),
            ("qwen3-coder:30b".to_string(), "2".to_string())
        ],
        "{json}"
    );
    // Nothing the client holds can be cut by a write in gonk, so it must not have cached it.
    assert!(!Kernel::is_cached(
        &client,
        &request(Verb::Source, "urn:sparql:select", &[("query", query)]),
        &Capability::root()
    ));
}
