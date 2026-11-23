//! Ledger [#4104](http://localhost:1062/l/default/item/4104): the activity graph as a resource.
//! A real `ikigai-log` segment, written through gonk's own record and rotated so it is closed,
//! read back as `urn:iki:gonk:activity:segment:{stamp}` and `urn:iki:gonk:activity`.

use std::sync::Arc;

use ikigai_core::{Capability, EndpointSpace, Error, Fallback, Iri, Kernel, Request, Verb};
use ikigai_gonk::access::{self, Door, Record};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{NamedNode, Term, Triple};

const ACT: &str = "urn:iki:gonk:activity:vocab#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const PROV_USED: &str = "http://www.w3.org/ns/prov#used";

struct Scene {
    _dir: tempfile::TempDir,
    record: Arc<Record>,
    kernel: Kernel,
}

/// Three owner polls of `next`, one owner claim, two anonymous page loads; then a rotation, so
/// that segment is closed and a new live one is open.
fn scene() -> (Scene, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let record = Record::open(&config, &dir.path().join("log"))
        .unwrap()
        .expect("a file destination");
    let at = 1_795_000_000_000u64; // 2026-11-18T...
    let line = |door, verb, iri: &str, cap: &Capability, ms| {
        let request = Request::new(verb, Iri::parse(iri).unwrap());
        record.access(&access::access(at + ms, door, &request, cap, Ok(10), 5));
    };
    let root = Capability::root();
    let anon = Capability::scoped(Vec::<String>::new());
    for ms in 0..3 {
        line(
            Door::Socket,
            Verb::Source,
            "urn:iki:ledger:default:next",
            &root,
            ms,
        );
    }
    line(
        Door::Socket,
        Verb::Sink,
        "urn:iki:ledger:default:item:7:claim",
        &root,
        10,
    );
    line(
        Door::Http,
        Verb::Source,
        "urn:iki:gonk:page:dashboard",
        &anon,
        20,
    );
    line(
        Door::Http,
        Verb::Source,
        "urn:iki:gonk:page:ledger",
        &anon,
        30,
    );
    let closed = record.handle().open_segment().unwrap().0;
    record.handle().rotate(access::now()).unwrap();
    let live = record.handle().open_segment().unwrap().0;
    assert_ne!(closed, live);
    let spaces: Vec<Arc<dyn ikigai_core::Space>> = vec![
        Arc::new(ikigai_log::endpoints::space(record.handle())),
        Arc::new(ikigai_gonk::activity::space()),
        Arc::new(EndpointSpace::new()),
    ];
    let kernel = Kernel::new(Arc::new(Fallback::new(spaces)));
    (
        Scene {
            _dir: dir,
            record,
            kernel,
        },
        closed,
        live,
    )
}

fn source(scene: &Scene, iri: &str, cap: &Capability) -> Result<Vec<Triple>, Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let read = runtime.block_on(
        scene
            .kernel
            .issue(Request::new(Verb::Source, Iri::parse(iri).unwrap()), cap),
    )?;
    Ok(RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(&read.bytes)
        .map(|q| Triple::from(q.unwrap()))
        .collect())
}

fn of_type<'a>(triples: &'a [Triple], class: &str) -> Vec<&'a Triple> {
    let class = Term::from(NamedNode::new(format!("{ACT}{class}")).unwrap());
    triples
        .iter()
        .filter(|t| t.predicate.as_str() == RDF_TYPE && t.object == class)
        .collect()
}

fn value(triples: &[Triple], subject: &Triple, predicate: &str) -> Vec<String> {
    triples
        .iter()
        .filter(|t| t.subject == subject.subject && t.predicate.as_str() == predicate)
        .map(|t| match &t.object {
            Term::Literal(l) => l.value().to_string(),
            other => other.to_string(),
        })
        .collect()
}

fn stamp(segment: &str) -> &str {
    segment.strip_prefix("urn:log:").unwrap()
}

#[test]
fn a_closed_segment_answers_its_rollup_writes_and_anonymous_counts() {
    let (scene, closed, _) = scene();
    let iri = format!("urn:iki:gonk:activity:segment:{}", stamp(&closed));
    let graph = source(&scene, &iri, &Capability::root()).unwrap();

    let rollups = of_type(&graph, "Rollup");
    let next: Vec<_> = rollups
        .iter()
        .filter(|r| value(&graph, r, &format!("{ACT}kind")) == ["ledger-next"])
        .collect();
    assert_eq!(next.len(), 1, "three polls in one hour are one node");
    assert_eq!(value(&graph, next[0], &format!("{ACT}count")), ["3"]);

    let writes = of_type(&graph, "Write");
    assert_eq!(writes.len(), 1, "the claim is the one write");
    assert_eq!(value(&graph, writes[0], &format!("{ACT}itemKey")), ["7"]);
    assert_eq!(
        value(&graph, writes[0], &format!("{ACT}ledger")),
        ["default"]
    );

    let anonymous = of_type(&graph, "AnonymousLoad");
    assert_eq!(anonymous.len(), 1, "two anonymous page loads, one bucket");
    assert_eq!(value(&graph, anonymous[0], &format!("{ACT}count")), ["2"]);
    assert_eq!(value(&graph, anonymous[0], &format!("{ACT}kind")), ["page"]);
    assert!(
        value(&graph, anonymous[0], PROV_USED).is_empty(),
        "an anonymous bucket names no resource"
    );
    assert!(
        !graph
            .iter()
            .any(|t| t.object.to_string().contains("urn:iki:gonk:page:")),
        "no anonymous resource anywhere in the graph"
    );
    drop(scene.record);
}

#[test]
fn the_live_segment_is_refused_and_the_union_leaves_it_out() {
    let (scene, closed, live) = scene();
    let live_iri = format!("urn:iki:gonk:activity:segment:{}", stamp(&live));
    assert!(source(&scene, &live_iri, &Capability::root()).is_err());

    let union = source(&scene, "urn:iki:gonk:activity", &Capability::root()).unwrap();
    let one = source(
        &scene,
        &format!("urn:iki:gonk:activity:segment:{}", stamp(&closed)),
        &Capability::root(),
    )
    .unwrap();
    assert_eq!(
        of_type(&union, "Rollup").len(),
        of_type(&one, "Rollup").len()
    );
    assert_eq!(
        union.len(),
        one.len(),
        "the union is the closed segment's graph"
    );
}

#[test]
fn both_are_the_owners_alone() {
    let (scene, closed, _) = scene();
    let reader = Capability::scoped(["urn:cap:store:read:graph:*"]);
    for iri in [
        "urn:iki:gonk:activity".to_string(),
        format!("urn:iki:gonk:activity:segment:{}", stamp(&closed)),
    ] {
        match source(&scene, &iri, &reader) {
            Err(Error::Denied(_)) => {}
            other => panic!("{iri} without urn:cap:log:read: {other:?}"),
        }
    }
}
