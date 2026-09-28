//! The arrangement as a resource, through gonk's own doors — core 0.1.78's
//! `urn:kernel:topology` over the spaces this server composes (ledger
//! [#546](http://localhost:1060/l/default/item/546)).
//!
//! Three shapes are served, and each is sourced here exactly as a caller holding
//! `urn:cap:kernel:inspect` would source it:
//!
//! - the **hub** ([`ikigai_gonk::compose`]) — `urn:iki:gonk:space:hub`, an `ik:Fallback`
//!   over the store, the ledger and the render transform, every layer structured;
//! - a **socket/QUIC door** ([`doors::door_kernel`]) — the SAME graph, byte for byte: the
//!   door's space forwards the hub's identity and structure rather than standing in front
//!   of it as an `ik:OpaqueSpace`;
//! - the **HTTP door** ([`doors::http_kernel`]) — `urn:iki:gonk:space:door:http`, whose
//!   layers are the pages and the hub, in that order.
//!
//! The last test is the paper's §12.5 check ported from core's `tests/topology.rs` and
//! walked over gonk's arrangement. gonk composes **no limited family** — nothing here is a
//! `Limit` over a prefix — so the walk cannot show the NO-with-a-limiter half; what it
//! shows is that every family this server serves is reachable from the entry, and that an
//! unserved family is unreachable through every door — by absence, the end of the chain.

use std::collections::BTreeSet;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::{compose, doors, spaces};
use ikigai_store::DurableStore;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{NamedOrBlankNode, Quad, Term};

const IK: &str = "https://ikigai-rs.dev/ns#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
/// The resource.
const TOPOLOGY: &str = "urn:kernel:topology";
/// The graph's entry node: the empty chain, named by its fingerprint (`root`) — stated in
/// core's `tests/topology.rs` rather than in the resource's own summary.
const ENTRY: &str = "urn:ikigai:chain:root";

fn hub() -> Arc<Kernel> {
    Arc::new(compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ))
}

/// The HTTP door as `main` builds it, over a scratch config home for the passkey layout.
fn http_door(hub: Arc<Kernel>, config: &std::path::Path) -> Kernel {
    let passkeys = Arc::new(ikigai_gonk::identity::Passkeys::new(
        ikigai_gonk::quic::Layout::in_config_home(config),
        1060,
    ));
    let face = Arc::new(ikigai_gonk::web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new(),
        passkeys,
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
    });
    doors::http_kernel(hub, ikigai_gonk::web::space(face))
}

/// `Source urn:kernel:topology` under exactly the capability it declares.
fn topology_turtle(kernel: &Kernel) -> String {
    let request = Request::new(
        Verb::Source,
        Iri::parse(TOPOLOGY).expect("the resource IRI"),
    );
    let inspect = Capability::scoped(["urn:cap:kernel:inspect"]);
    let repr = block_on(kernel.issue(request, &inspect)).expect("the topology renders");
    assert_eq!(repr.repr_type.media_type, "text/turtle");
    String::from_utf8(repr.bytes.to_vec()).expect("Turtle is UTF-8")
}

fn topology(kernel: &Kernel) -> Graph {
    Graph::parse(&topology_turtle(kernel))
}

/// The rendered graph, parsed — a handful of accessors over the triples, mirroring the
/// helper core's own topology test uses so the two walks read alike.
struct Graph(Vec<Quad>);

impl Graph {
    fn parse(turtle: &str) -> Graph {
        let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
            .for_slice(turtle.as_bytes())
            .map(|q| q.unwrap_or_else(|e| panic!("{e}\n{turtle}")))
            .collect();
        for q in &quads {
            assert!(
                matches!(q.subject, NamedOrBlankNode::NamedNode(_))
                    && !matches!(q.object, Term::BlankNode(_)),
                "a blank node in the topology: {q}"
            );
        }
        Graph(quads)
    }

    fn objects(&self, s: &str, p: &str) -> Vec<&Term> {
        self.0
            .iter()
            .filter(|q| matches!(&q.subject, NamedOrBlankNode::NamedNode(n) if n.as_str() == s))
            .filter(|q| q.predicate.as_str() == p)
            .map(|q| &q.object)
            .collect()
    }

    fn iri(&self, s: &str, p: &str) -> Option<String> {
        self.objects(s, p).first().map(|o| match o {
            Term::NamedNode(n) => n.as_str().to_string(),
            other => panic!("{s} {p} is not an IRI: {other}"),
        })
    }

    fn strs(&self, s: &str, p: &str) -> Vec<String> {
        self.objects(s, p)
            .into_iter()
            .map(|o| match o {
                Term::Literal(l) => l.value().to_string(),
                other => panic!("{s} {p} is not a literal: {other}"),
            })
            .collect()
    }

    fn kind(&self, s: &str) -> String {
        self.iri(s, &format!("{RDF}type"))
            .unwrap_or_else(|| panic!("{s} has no type"))
            .strip_prefix(IK)
            .expect("an ik: kind")
            .to_string()
    }

    /// The members of `s`'s `ik:layers`, in list order.
    fn layers(&self, s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cell = self
            .iri(s, &format!("{IK}layers"))
            .unwrap_or_else(|| panic!("{s} has no layers"));
        while cell != format!("{RDF}nil") {
            out.push(self.iri(&cell, &format!("{RDF}first")).expect("rdf:first"));
            cell = self.iri(&cell, &format!("{RDF}rest")).expect("rdf:rest");
        }
        out
    }

    fn space(&self, s: &str) -> String {
        self.iri(s, &format!("{IK}space"))
            .unwrap_or_else(|| panic!("{s} encloses nothing"))
    }

    /// Every node of a given kind.
    fn of_kind(&self, kind: &str) -> BTreeSet<String> {
        self.0
            .iter()
            .filter(|q| q.predicate.as_str() == format!("{RDF}type"))
            .filter(
                |q| matches!(&q.object, Term::NamedNode(n) if n.as_str() == format!("{IK}{kind}")),
            )
            .map(|q| match &q.subject {
                NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
                _ => unreachable!("checked at parse"),
            })
            .collect()
    }
}

/// ★ The hub names itself, and everything it composes is structured: three layers, each
/// an `ik:EndpointSpace` with its patterns, and not one `ik:OpaqueSpace` in the graph.
#[test]
fn the_hub_is_a_named_fallback_over_structured_layers_and_nothing_is_opaque() {
    let g = topology(&hub());

    assert_eq!(g.kind(ENTRY), "Chain");
    assert_eq!(g.strs(ENTRY, &format!("{IK}severed")), ["false"]);
    assert_eq!(g.layers(ENTRY), [spaces::HUB]);

    assert_eq!(g.kind(spaces::HUB), "Fallback");
    let layers = g.layers(spaces::HUB);
    assert_eq!(layers.len(), 3, "store, ledger, render: {layers:?}");
    for layer in &layers {
        assert_eq!(g.kind(layer), "EndpointSpace", "{layer}");
    }
    // The linked crates' spaces are anonymous today (neither `ikigai-store` 0.2.5 nor
    // `ikigai-ledger` 0.2.1 names its space), so those two are skolemized; gonk's own is
    // named, and its one pattern is the transform it binds.
    let pattern = format!("{IK}pattern");
    assert!(
        layers[0].starts_with("urn:ikigai:space:_:"),
        "{}",
        layers[0]
    );
    assert!(g
        .strs(&layers[0], &pattern)
        .iter()
        .any(|p| p.starts_with("urn:iki:store:")));
    assert!(
        layers[1].starts_with("urn:ikigai:space:_:"),
        "{}",
        layers[1]
    );
    assert!(g
        .strs(&layers[1], &pattern)
        .iter()
        .any(|p| p.starts_with("urn:iki:ledger:")));
    assert_eq!(layers[2], spaces::RENDER);
    assert_eq!(g.strs(spaces::RENDER, &pattern), ["urn:iki:gonk:render"]);

    assert!(
        g.of_kind("OpaqueSpace").is_empty(),
        "{:?}",
        g.of_kind("OpaqueSpace")
    );
}

/// ★ A socket or QUIC door renders THE SAME GRAPH the hub renders — byte for byte. The
/// door's space holds exactly the hub's doors, so it forwards the hub's identity and
/// structure; a node of its own would be a lie about where the arrangement continues, and
/// an opaque one would be the honest version of the same lie.
#[test]
fn a_door_kernel_renders_the_hubs_arrangement_under_the_hubs_name() {
    let hub = hub();
    let from_hub = topology_turtle(&hub);
    let from_door = topology_turtle(&doors::door_kernel(Arc::clone(&hub)));
    assert_eq!(from_door, from_hub);
    assert!(from_door.contains(&format!("<{}> a ik:Fallback", spaces::HUB)));
    assert!(!from_door.contains("ik:OpaqueSpace"));
}

/// ★ The HTTP door is its own named `ik:Fallback`: the pages, then the hub (one node, the
/// same IRI as everywhere else), and nothing after it. It used to end in a floor — a
/// catch-all rendered as an `ik:Limit` over the empty family — that existed only because
/// `ikigai-web` once answered `Unresolved` with a 500; that library maps it to a 404 now, so
/// the catch-all is gone (see `doors::http_kernel`), and this pins that it stays gone.
#[test]
fn the_http_door_is_pages_then_the_hub() {
    let config = tempfile::tempdir().expect("a scratch config home");
    let g = topology(&http_door(hub(), config.path()));

    assert_eq!(g.layers(ENTRY), [spaces::HTTP_DOOR]);
    assert_eq!(g.kind(spaces::HTTP_DOOR), "Fallback");
    assert_eq!(g.layers(spaces::HTTP_DOOR), [spaces::PAGES, spaces::HUB]);

    assert_eq!(g.kind(spaces::PAGES), "EndpointSpace");
    let pages = g.strs(spaces::PAGES, &format!("{IK}pattern"));
    for expected in [
        "urn:iki:gonk:page:home",
        "urn:iki:gonk:act",
        "urn:iki:gonk:sparql",
    ] {
        assert!(
            pages.iter().any(|p| p == expected),
            "{expected} not in {pages:?}"
        );
    }

    // The hub inside the door is the hub: same node, same layers as it reports for itself.
    assert_eq!(g.kind(spaces::HUB), "Fallback");
    assert_eq!(g.layers(spaces::HUB).len(), 3);

    assert!(
        g.of_kind("Limit").is_empty(),
        "no floor: a name nothing binds is the library's 404 — {:?}",
        g.of_kind("Limit")
    );

    assert!(
        g.of_kind("OpaqueSpace").is_empty(),
        "{:?}",
        g.of_kind("OpaqueSpace")
    );
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Reach {
    Reachable,
    Unreachable,
    Unknown,
}

/// The paper's §12.5 check as core's `tests/topology.rs` writes it: is any door of `family`
/// reachable from `node` without a limiter over the family standing ahead of it?
fn reach(g: &Graph, node: &str, family: &str) -> Reach {
    let p = |name: &str| format!("{IK}{name}");
    match g.kind(node).as_str() {
        "Chain" | "Fallback" => {
            let mut unknown = false;
            for layer in g.layers(node) {
                if g.kind(&layer) == "Limit"
                    && g.strs(&layer, &p("family"))
                        .iter()
                        .any(|limited| family.starts_with(limited.as_str()))
                {
                    return Reach::Unreachable;
                }
                match reach(g, &layer, family) {
                    Reach::Reachable => return Reach::Reachable,
                    Reach::Unknown => unknown = true,
                    Reach::Unreachable => {}
                }
            }
            if unknown {
                Reach::Unknown
            } else {
                Reach::Unreachable
            }
        }
        "Mount" => {
            let prefix = g.strs(node, &p("prefix")).remove(0);
            if family.starts_with(&prefix) || prefix.starts_with(family) {
                reach(g, &g.space(node), family)
            } else {
                Reach::Unreachable
            }
        }
        "EndpointSpace" => {
            if g.strs(node, &p("pattern"))
                .iter()
                .any(|pattern| pattern.starts_with(family))
            {
                Reach::Reachable
            } else {
                Reach::Unreachable
            }
        }
        "Limit" => Reach::Unreachable,
        "Rewrite" | "Alias" | "Confine" => reach(g, &g.space(node), family),
        "OpaqueSpace" => Reach::Unknown,
        other => panic!("unknown node kind {other}"),
    }
}

/// The §12.5 walk over gonk's arrangement. gonk composes no limited family, so there is no
/// "NO with the limiter" case to show and none is invented; what the walk establishes is
/// that every family this server serves is reachable from each door's entry, and that
/// every family it does not serve is `Unreachable` through both doors, by absence.
#[test]
fn the_12_5_walk_reaches_every_served_family_and_closes_the_rest() {
    let hub = hub();
    let config = tempfile::tempdir().expect("a scratch config home");
    let socket = topology(&doors::door_kernel(Arc::clone(&hub)));
    let http = topology(&http_door(hub, config.path()));

    for family in ["urn:iki:ledger:", "urn:iki:store:", "urn:iki:gonk:render"] {
        assert_eq!(reach(&socket, ENTRY, family), Reach::Reachable, "{family}");
        assert_eq!(reach(&http, ENTRY, family), Reach::Reachable, "{family}");
    }
    // The pages are the HTTP door's alone.
    assert_eq!(reach(&http, ENTRY, "urn:iki:gonk:page:"), Reach::Reachable);
    assert_eq!(
        reach(&socket, ENTRY, "urn:iki:gonk:page:"),
        Reach::Unreachable
    );
    // An unserved family: closed either way, and never `Unknown` — nothing in either
    // arrangement is opaque.
    assert_eq!(reach(&http, ENTRY, "urn:personal:"), Reach::Unreachable);
    assert_eq!(reach(&socket, ENTRY, "urn:personal:"), Reach::Unreachable);
    // The HTTP door once ended in a catch-all floor, and removing it changed nothing here —
    // which is what the old version of this test said a floor was: a change to what an
    // unbound name ANSWERS (404, not 500), never to what is reachable. The library answers
    // that 404 itself now; `tests/web.rs::a_browser_gets_html_pages_and_a_readable_404`
    // is where the status is pinned.
    assert_eq!(http.layers(spaces::HTTP_DOOR), [spaces::PAGES, spaces::HUB]);
}
