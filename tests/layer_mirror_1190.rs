//! Ledger [#1190](http://localhost:1060/l/default/item/1190): a named ledger bound to a task
//! layer, the OFFER role, over a scripted layer host.
//!
//! The layer host is the published `ikigai-layers` in process, behind a door that mints each
//! participant's principal and holds that participant's grant, exactly what the layer door's
//! enrollment gives a certificate. Our gonk is `compose` over an in-memory store. The mirror
//! reaches the host through its door as `plasma-gonk`; the taker (Bobby's gonk) is driven by
//! hand through its own door as `bobby-gonk`.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Iri,
    Kernel, ReprType, Representation, Request, Space, SystemClock, Verb,
};
use ikigai_gonk::layer::{self, Binding, Door, Role};
use ikigai_gonk::mirror::{self, Mirror};
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;

const IK: &str = "https://ikigai-rs.dev/ns#";
const LAYER: &str = "tasks";
const LEDGER: &str = "crew";
const TUPLES: &str = "urn:layer:tasks:tuples";

fn participant(slug: &str) -> String {
    format!("urn:collab:participant:{slug}")
}

fn us() -> String {
    participant("plasma-gonk")
}

fn them() -> String {
    participant("bobby-gonk")
}

const TASK_SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

<urn:example:shape:offer> a sh:NodeShape ;
  sh:targetClass ik:Offer ;
  sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ; sh:maxLength 200 ] ,
              [ sh:path ik:brief ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path ik:repository ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .

<urn:example:shape:report> a sh:NodeShape ;
  sh:targetClass ik:Progress , ik:Release , ik:Handback ;
  sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path ik:offer ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path ik:addressedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path ik:note ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path ik:evidence ; sh:nodeKind sh:IRI ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .

<urn:example:shape:handback> a sh:NodeShape ;
  sh:targetClass ik:Handback ;
  sh:closed true ;
  sh:ignoredProperties ( rdf:type ik:offer ik:addressedTo ik:note prov:wasAttributedTo dcterms:created ik:seq ) ;
  sh:property [ sh:path ik:evidence ; sh:minCount 1 ; sh:nodeKind sh:IRI ] .
"#;

const TASK_CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#",
  "dcterms": "http://purl.org/dc/terms/",
  "prov": "http://www.w3.org/ns/prov#",
  "xsd": "http://www.w3.org/2001/XMLSchema#",
  "Offer": "ik:Offer", "Progress": "ik:Progress", "Release": "ik:Release", "Handback": "ik:Handback",
  "title": "dcterms:title",
  "brief": "ik:brief",
  "note": "ik:note",
  "repository": { "@id": "ik:repository", "@type": "@id" },
  "offer": { "@id": "ik:offer", "@type": "@id" },
  "addressedTo": { "@id": "ik:addressedTo", "@type": "@id" },
  "evidence": { "@id": "ik:evidence", "@type": "@id" },
  "attributedTo": { "@id": "prov:wasAttributedTo", "@type": "@id" },
  "created": { "@id": "dcterms:created", "@type": "xsd:dateTime" }
} }
"#;

const MSG_SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
<urn:example:shape:message> a sh:NodeShape ;
  sh:targetClass ik:Message ;
  sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .
"#;

const MSG_CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#",
  "Message": "ik:Message", "body": "ik:body"
} }
"#;

const PARTICIPANTS: &str = r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:collab:participant:brian>       a foaf:Person .
<urn:collab:participant:plasma-gonk> a ik:Machine ; prov:actedOnBehalfOf <urn:collab:participant:brian> .
<urn:collab:participant:bobby>       a foaf:Person .
<urn:collab:participant:bobby-gonk>  a ik:Machine ; prov:actedOnBehalfOf <urn:collab:participant:bobby> .
"#;

const CREW: &str = r#"[[mount]]
layer = "tasks"
plasma-gonk = ["read", "feed", "out", "take"]
bobby-gonk  = ["read", "feed", "out", "take"]
bobby       = ["read", "feed", "out", "take"]

[[mount]]
layer = "talk"
plasma-gonk = ["read", "feed", "say"]
bobby       = ["read", "feed", "say"]
"#;

/// A layer participant: the door's mint of `principal` with the grant its enrollment gives.
struct AsParticipant {
    kernel: Arc<Kernel>,
    capability: Capability,
}

impl Door for AsParticipant {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        block_on(Kernel::issue(&self.kernel, request, &self.capability))
    }
}

/// A door that lets `racer` take the tuple named in the FIRST take that passes through it,
/// just before that take runs: the take-racing-a-withdrawal case made deterministic.
struct Racing {
    inner: AsParticipant,
    racer: AsParticipant,
    armed: AtomicBool,
}

impl Door for Racing {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        if request.verb == Verb::Delete && self.armed.swap(false, Ordering::SeqCst) {
            self.racer
                .call(request.clone())
                .expect("the racer's take wins");
        }
        self.inner.call(request)
    }
}

struct World {
    _config: tempfile::TempDir,
    _state: tempfile::TempDir,
    layers: Arc<Kernel>,
    hub: Arc<Kernel>,
}

fn grant(principal: &str) -> Capability {
    let mut scopes: Vec<String> = ["read", "feed", "out", "take"]
        .iter()
        .map(|v| format!("urn:cap:layer:{v}:{LAYER}"))
        .collect();
    scopes.extend(
        ["read", "feed", "say"]
            .iter()
            .map(|v| format!("urn:cap:layer:{v}:talk")),
    );
    scopes.push("urn:cap:collab:read:crew".to_string());
    Capability::scoped(scopes)
        .with_principal(principal)
        .expect("a principal")
}

impl World {
    fn new() -> World {
        let config = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = config.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("layers/tasks.shapes.ttl", TASK_SHAPES);
        write("layers/tasks.context.jsonld", TASK_CONTEXT);
        write(
            "layers/tasks.toml",
            "model = \"tuplespace\"\nshapes = \"tasks.shapes.ttl\"\ncontext = \"tasks.context.jsonld\"\naddressed = true\n",
        );
        write("layers/msg.shapes.ttl", MSG_SHAPES);
        write("layers/msg.context.jsonld", MSG_CONTEXT);
        write(
            "layers/talk.toml",
            "model = \"channel\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\n",
        );
        write("participants.ttl", PARTICIPANTS);
        write("collabs/crew.toml", CREW);
        let state = tempfile::tempdir().unwrap();
        let host = ikigai_layers::Host::open(config.path(), state.path()).expect("the host");
        // `urn:collab:{name}:participants` is ikigai-layers 0.2.1's (merged, not published):
        // scripted here, in front, with the same graph it serves.
        let participants = FnEndpoint::new("collab-participants", |_inv| {
            Ok(Representation::new(
                ReprType::new("text/turtle"),
                PARTICIPANTS.as_bytes().to_vec(),
            ))
        })
        .with_description(Description::new("collab-participants").verb(Verb::Source));
        let spaces: Vec<Arc<dyn Space>> = vec![
            Arc::new(
                EndpointSpace::new().bind(Exact::new("urn:collab:crew:participants"), participants),
            ),
            Arc::new(ikigai_layers::space(&host)),
            Arc::new(ikigai_shacl::space()),
        ];
        let layers = Arc::new(
            Kernel::with_meta_renderer(
                Arc::new(Fallback::new(spaces)),
                Arc::new(ikigai_vocab::TurtleRenderer),
            )
            .with_clock(Arc::new(SystemClock)),
        );
        let hub = Arc::new(ikigai_gonk::compose(DurableStore::in_memory().unwrap()));
        World {
            _config: config,
            _state: state,
            layers,
            hub,
        }
    }

    fn door(&self, principal: &str) -> AsParticipant {
        AsParticipant {
            kernel: Arc::clone(&self.layers),
            capability: grant(principal),
        }
    }

    fn binding(&self, layer: &str) -> Binding {
        let certs = tempfile::tempdir().unwrap();
        let line = format!(
            "{layer} offer quic://127.0.0.1:1 {} ledger={LEDGER} collab=crew",
            certs.path().display()
        );
        let binding = layer::parse(&line, std::path::Path::new("/nowhere")).unwrap();
        assert_eq!(binding.role, Role::Offer);
        binding
    }

    fn mirror(&self) -> Mirror {
        self.mirror_through(Arc::new(self.door(&us())))
    }

    fn mirror_through(&self, door: Arc<dyn Door>) -> Mirror {
        Mirror::new(self.binding(LAYER), door, Arc::clone(&self.hub)).unwrap()
    }

    /// Issue on our gonk as its owner (root).
    fn gonk(&self, verb: Verb, resource: &str, args: &[(&str, &str)]) -> Result<String, Error> {
        let request = args.iter().fold(
            Request::new(
                verb,
                Iri::parse(format!("urn:iki:ledger:{LEDGER}:{resource}")).unwrap(),
            ),
            |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
        );
        Resolver::issue(&*self.hub, request)
            .map(|(r, _)| String::from_utf8_lossy(&r.bytes).into_owned())
    }

    fn file(&self, content: &str, args: &[(&str, &str)]) -> i64 {
        let mut all = vec![("content", content), ("as", "application/json")];
        all.extend_from_slice(args);
        let answer = self.gonk(Verb::Sink, "append", &all).unwrap();
        let json: serde_json::Value = serde_json::from_str(&answer).unwrap();
        json.pointer("/item/number").unwrap().as_i64().unwrap()
    }

    fn item(&self, number: i64) -> serde_json::Value {
        let answer = self
            .gonk(
                Verb::Source,
                &format!("item:{number}"),
                &[("as", "application/json")],
            )
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&answer).unwrap()["item"].clone()
    }

    fn comments(&self, number: i64) -> Vec<String> {
        self.item(number)["comments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["text"].as_str().unwrap().to_string())
            .collect()
    }

    /// The pending tuples' subjects, as anyone holding read sees them.
    fn pending(&self) -> Vec<String> {
        let answer = self
            .door(&them())
            .call(
                Request::new(Verb::Source, Iri::parse(TUPLES).unwrap())
                    .with_arg("limit", ArgRef::Inline(b"1000".to_vec())),
            )
            .unwrap();
        String::from_utf8(answer.bytes)
            .unwrap()
            .lines()
            .map(|l| l.split(' ').nth(2).unwrap().to_string())
            .collect()
    }

    fn tuple(&self, subject: &str) -> String {
        let answer = self
            .door(&them())
            .call(
                Request::new(Verb::Source, Iri::parse(TUPLES).unwrap())
                    .with_arg("tuple", ArgRef::Inline(subject.as_bytes().to_vec())),
            )
            .unwrap();
        String::from_utf8(answer.bytes).unwrap()
    }

    fn take(&self, who: &str, subject: &str) -> Result<Representation, Error> {
        self.door(who).call(
            Request::new(Verb::Delete, Iri::parse(TUPLES).unwrap())
                .with_arg("tuple", ArgRef::Inline(subject.as_bytes().to_vec())),
        )
    }

    /// Drop a report on `offer`, addressed to us, as `who`.
    fn report(&self, who: &str, class: &str, offer: &str, extra: &str) -> String {
        let subject = format!("urn:layer:{LAYER}:report:{}", mirror::mint_id());
        let content = format!(
            "@prefix ik: <{IK}> .\n<{subject}> a ik:{class} ; ik:offer <{offer}> ; \
             ik:addressedTo <{}> {extra} .\n",
            us()
        );
        self.door(who)
            .call(
                Request::new(Verb::Sink, Iri::parse(TUPLES).unwrap())
                    .with_arg("content", ArgRef::Inline(content.into_bytes())),
            )
            .unwrap_or_else(|e| panic!("dropping a {class}: {e}"));
        subject
    }
}

fn pass(mirror: &Mirror) -> mirror::Report {
    let report = mirror.pass().expect("a pass");
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    report
}

/// The pass after a pass that settled: nothing to do.
fn settled(mirror: &Mirror) {
    let report = pass(mirror);
    assert!(
        report.actions.is_empty(),
        "a second pass acted: {:?}",
        report.actions
    );
}

fn offered(report: &mirror::Report) -> String {
    let line = report
        .actions
        .iter()
        .find(|a| a.starts_with("offered "))
        .unwrap_or_else(|| panic!("no offer in {:?}", report.actions));
    line.rsplit('<')
        .next()
        .unwrap()
        .trim_end_matches('>')
        .to_string()
}

#[test]
fn a_take_becomes_a_claim_by_the_taker() {
    let w = World::new();
    let mirror = w.mirror();
    let n = w.file(
        "Port the importer\n\nThe importer reads the old format.",
        &[],
    );
    let offer = offered(&pass(&mirror));
    assert!(offer.starts_with("urn:layer:tasks:offer:"), "{offer}");
    assert_eq!(w.pending(), std::slice::from_ref(&offer));
    assert!(
        w.comments(n).iter().any(|c| c.contains(&offer)),
        "the offer is recorded on its item"
    );
    settled(&mirror);

    w.take(&them(), &offer).unwrap();
    let report = pass(&mirror);
    assert!(
        report.actions.iter().any(|a| a.starts_with("claimed ")),
        "{:?}",
        report.actions
    );
    let claim = &w.item(n)["claim"];
    assert_eq!(claim["holder"], them());
    assert_eq!(claim["kind"], "machine", "bobby-gonk is an ik:Machine");
    assert_eq!(claim["principal"], "urn:iki:gonk:layer:tasks");
    assert!(claim["expires"].is_string(), "on a lease: {claim}");
    assert!(w.pending().is_empty());
    settled(&mirror);
}

#[test]
fn a_person_who_takes_holds_a_persons_claim() {
    let w = World::new();
    let mirror = w.mirror();
    let n = w.file(
        "Review the chapter\n\nRead chapter 3 and say what is unclear.",
        &[],
    );
    let offer = offered(&pass(&mirror));
    w.take(&participant("bobby"), &offer).unwrap();
    pass(&mirror);
    let claim = &w.item(n)["claim"];
    assert_eq!(claim["holder"], participant("bobby"));
    assert_eq!(
        claim["kind"], "person",
        "the participants graph says foaf:Person"
    );
}

#[test]
fn a_replayed_hand_back_comments_once() {
    let w = World::new();
    let mirror = w.mirror();
    let n = w.file("Fix the parser\n\nIt drops the last line.", &[]);
    let offer = offered(&pass(&mirror));
    w.take(&them(), &offer).unwrap();
    pass(&mirror);
    let handback = w.report(
        &them(),
        "Handback",
        &offer,
        "; ik:evidence <https://github.com/ikigai-rs/ikigai-core/pull/41> ; ik:note \"CI green\"",
    );
    let report = pass(&mirror);
    assert!(
        report.actions.iter().any(|a| a.contains(&handback)),
        "took the hand-back: {:?}",
        report.actions
    );
    let handed = |w: &World| {
        w.comments(n)
            .iter()
            .filter(|c| c.starts_with("Handed back"))
            .count()
    };
    assert_eq!(handed(&w), 1);
    assert_eq!(w.item(n)["awaiting"], "dispatcher");
    assert!(w.pending().is_empty(), "the hand-back is consumed");

    // The dispatcher answers: the evidence did not hold. A replay of the hand-back must not
    // comment again, nor set the hand-back again over the dispatcher's answer.
    w.gonk(
        Verb::Sink,
        "comment",
        &[
            ("item", &n.to_string()),
            ("rejects", "true"),
            ("content", "CI is red on main"),
        ],
    )
    .unwrap();
    assert!(w.item(n)["awaiting"].is_null());
    settled(&mirror);
    settled(&mirror);
    assert_eq!(handed(&w), 1, "{:?}", w.comments(n));
    assert!(
        w.item(n)["awaiting"].is_null(),
        "a replay re-set the hand-back"
    );
    // And the claim stays the taker's: a hand-back is not a release.
    assert_eq!(w.item(n)["claim"]["holder"], them());
}

#[test]
fn a_take_racing_a_withdrawal_leaves_one_outcome_and_a_comment() {
    let w = World::new();
    let n = w.file("Retire the old door\n\nRemove the v1 door.", &[]);
    let offer = offered(&pass(&w.mirror()));
    // The item closes here while its offer is pending, so the next pass withdraws it, and the
    // taker's take lands between that pass's read and its withdrawal.
    w.gonk(
        Verb::Sink,
        "close",
        &[("item", &n.to_string()), ("reason", "superseded")],
    )
    .unwrap();
    let racing = Racing {
        inner: w.door(&us()),
        racer: w.door(&them()),
        armed: AtomicBool::new(true),
    };
    let mirror = w.mirror_through(Arc::new(racing));
    let report = pass(&mirror);
    assert!(
        !report.actions.iter().any(|a| a.starts_with("withdrew")),
        "the taker won, so nothing was withdrawn: {:?}",
        report.actions
    );
    let late: Vec<String> = w
        .comments(n)
        .into_iter()
        .filter(|c| c.contains("after this server tried to withdraw it"))
        .collect();
    assert_eq!(late.len(), 1, "{:?}", w.comments(n));
    assert!(late[0].contains(&them()), "{}", late[0]);
    // One outcome: the take stands (its receipt names the taker), and a closed item is not
    // claimed back open.
    let receipt = w
        .door(&us())
        .call(
            Request::new(Verb::Source, Iri::parse(TUPLES).unwrap())
                .with_arg("state", ArgRef::Inline(b"taken".to_vec())),
        )
        .unwrap();
    let receipts = String::from_utf8(receipt.bytes).unwrap();
    assert_eq!(receipts.lines().count(), 1, "{receipts}");
    assert!(receipts.contains(&them()), "{receipts}");
    assert!(receipts.contains(&offer), "{receipts}");
    assert!(w.item(n)["claim"].is_null());
    settled(&mirror);
    assert_eq!(
        w.comments(n)
            .iter()
            .filter(|c| c.contains("after this server tried"))
            .count(),
        1
    );
}

#[test]
fn a_withdrawal_that_wins_leaves_the_taker_refused_and_no_comment() {
    let w = World::new();
    let mirror = w.mirror();
    let n = w.file("Draft the release notes\n\nFor 0.2.", &[]);
    let offer = offered(&pass(&mirror));
    w.gonk(
        Verb::Sink,
        "claim",
        &[("item", &n.to_string()), ("content", "brian")],
    )
    .unwrap();
    let report = pass(&mirror);
    assert!(
        report.actions.iter().any(|a| a.starts_with("withdrew")),
        "{:?}",
        report.actions
    );
    match w.take(&them(), &offer) {
        Err(Error::Conflict(why)) => assert!(why.contains(&us()), "{why}"),
        other => panic!("the taker's late take: {other:?}"),
    }
    assert!(!w
        .comments(n)
        .iter()
        .any(|c| c.contains("after this server tried")));
    assert_eq!(w.item(n)["claim"]["holder"], "brian");
    settled(&mirror);
}

#[test]
fn nothing_outside_the_context_crosses() {
    let w = World::new();
    let mirror = w.mirror();
    let n = w.file(
        "Wire the store\n\nUse the durable store behind the door.",
        &[
            ("labels", "secret-label"),
            (
                "about",
                "https://github.com/ikigai-rs/ikigai-store urn:example:private:thing",
            ),
            ("priority", "1"),
        ],
    );
    w.gonk(
        Verb::Sink,
        "comment",
        &[("item", &n.to_string()), ("content", "an internal note")],
    )
    .unwrap();
    let offer = offered(&pass(&mirror));
    let tuple = w.tuple(&offer);
    for leak in [
        "secret-label",
        "urn:example:private",
        "an internal note",
        "urn:iki:ledger",
        "ledger#",
    ] {
        assert!(!tuple.contains(leak), "{leak} crossed:\n{tuple}");
    }
    let triples = oxigraph_triples(&tuple);
    let predicates: BTreeSet<&str> = triples.iter().map(|(_, p, _)| p.as_str()).collect();
    let allowed: BTreeSet<&str> = [
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://purl.org/dc/terms/title",
        "https://ikigai-rs.dev/ns#brief",
        "https://ikigai-rs.dev/ns#repository",
        // The host's stamps.
        "http://www.w3.org/ns/prov#wasAttributedTo",
        "http://purl.org/dc/terms/created",
        "https://ikigai-rs.dev/ns#seq",
    ]
    .into_iter()
    .collect();
    assert_eq!(predicates, allowed, "{tuple}");
    assert!(triples.iter().all(|(s, _, _)| *s == offer));
    assert!(tuple.contains("https://github.com/ikigai-rs/ikigai-store"));
    assert!(tuple.contains("Use the durable store behind the door."));
    assert!(!tuple.contains(&format!("#{n}")));

    // And the guard behind the CONSTRUCT: anything the context does not define is refused,
    // never pruned away in silence.
    let prune = Kernel::new(Arc::new(ikigai_jsonld::space()));
    let leaky = serde_json::json!([{
        "@id": offer,
        "@type": [format!("{IK}Offer")],
        "http://purl.org/dc/terms/title": [{"@value": "t"}],
        "https://ikigai-rs.dev/ns/ledger#label": [{"@value": "secret-label"}],
    }]);
    let refused =
        mirror::prune_strict(&prune, leaky.to_string().as_bytes(), TASK_CONTEXT).unwrap_err();
    assert!(refused.contains("refused to publish"), "{refused}");
}

#[test]
fn an_item_without_a_body_is_not_offered_and_says_why() {
    let w = World::new();
    let mirror = w.mirror();
    let n = w.file("Just a title", &[]);
    let report = mirror.pass().unwrap();
    assert!(report.actions.is_empty(), "{:?}", report.actions);
    assert!(
        report.problems.iter().any(|p| p.contains("no body")),
        "{:?}",
        report.problems
    );
    assert!(w.pending().is_empty());
    assert!(
        w.comments(n).is_empty(),
        "nothing recorded for an offer never made"
    );
}

#[test]
fn a_release_releases_the_claim_and_offers_again() {
    let w = World::new();
    let mirror = w.mirror();
    let n = w.file("Measure the cache\n\nBefore and after.", &[]);
    let first = offered(&pass(&mirror));
    w.take(&them(), &first).unwrap();
    pass(&mirror);
    assert_eq!(w.item(n)["claim"]["holder"], them());
    w.report(&them(), "Progress", &first, "; ik:note \"halfway\"");
    let report = pass(&mirror);
    assert!(
        report.actions.iter().any(|a| a.starts_with("renewed ")),
        "{:?}",
        report.actions
    );
    w.report(
        &them(),
        "Release",
        &first,
        "; ik:note \"no time this week\"",
    );
    let report = pass(&mirror);
    assert!(
        report.actions.iter().any(|a| a.starts_with("released ")),
        "{:?}",
        report.actions
    );
    let second = offered(&report);
    assert_ne!(first, second);
    assert!(w.item(n)["claim"].is_null());
    assert_eq!(w.pending(), [second]);
    settled(&mirror);
}

#[test]
fn closing_an_item_withdraws_its_pending_offer() {
    let w = World::new();
    let mirror = w.mirror();
    let n = w.file("Rename the crate\n\nTo its final name.", &[]);
    let offer = offered(&pass(&mirror));
    w.gonk(
        Verb::Sink,
        "close",
        &[("item", &n.to_string()), ("reason", "wontfix")],
    )
    .unwrap();
    let report = pass(&mirror);
    assert!(
        report
            .actions
            .iter()
            .any(|a| a.contains(&offer) && a.starts_with("withdrew")),
        "{:?}",
        report.actions
    );
    assert!(w.pending().is_empty());
    settled(&mirror);
}

#[test]
fn a_binding_to_a_layer_that_is_not_a_tuplespace_is_refused() {
    let w = World::new();
    w.file("Anything\n\nAt all.", &[]);
    let mirror = Mirror::new(
        w.binding("talk"),
        Arc::new(w.door(&us())),
        Arc::clone(&w.hub),
    )
    .unwrap();
    let refused = mirror.pass().unwrap_err();
    assert!(refused.contains("only a task layer"), "{refused}");
    assert!(refused.contains("`channel`"), "{refused}");
    // And for good: the next pass is refused without asking again.
    assert_eq!(mirror.pass().unwrap_err(), refused);
}

#[test]
fn the_mirror_cannot_close() {
    let w = World::new();
    let n = w.file("Hold this\n\nPlease.", &[]);
    let capability = mirror::capability(&w.binding(LAYER)).unwrap();
    let close = Request::new(
        Verb::Sink,
        Iri::parse(format!("urn:iki:ledger:{LEDGER}:close")).unwrap(),
    )
    .with_arg("item", ArgRef::Inline(n.to_string().into_bytes()))
    .with_arg("reason", ArgRef::Inline(b"done".to_vec()));
    match Resolver::issue_as(&*w.hub, close, &capability) {
        Err(Error::Denied(_)) => {}
        other => panic!("the mirror closed: {other:?}"),
    }
}

/// The triggers: a pass on start, and one on a write to the ledger (the cut listener), with
/// no sweep in between.
#[test]
fn a_ledger_write_offers_without_waiting_for_the_sweep() {
    let w = World::new();
    mirror::start(w.mirror());
    std::thread::sleep(Duration::from_millis(300));
    w.file("Arrives later\n\nOffered by the listener.", &[]);
    let deadline = Instant::now() + Duration::from_secs(20);
    while w.pending().is_empty() {
        assert!(
            Instant::now() < deadline,
            "no offer within 20 s of the write"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn oxigraph_triples(turtle: &str) -> Vec<(String, String, String)> {
    oxigraph::io::RdfParser::from_format(oxigraph::io::RdfFormat::Turtle)
        .for_slice(turtle.as_bytes())
        .map(|q| {
            let q = q.unwrap();
            let s = match &q.subject {
                oxigraph::model::NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
                other => other.to_string(),
            };
            (s, q.predicate.as_str().to_string(), q.object.to_string())
        })
        .collect()
}

// ---- the take role: their gonk, against our mirror ------------------------------------------

use ikigai_gonk::taker::{self, Taker, Took};

/// Their side: another gonk (its own store and ledger `ikigai`), taking from the same layer as
/// `bobby-gonk`.
struct Theirs {
    hub: Arc<Kernel>,
    taker: Taker,
}

impl World {
    fn theirs(&self) -> Theirs {
        self.theirs_through(Arc::new(self.door(&them())))
    }

    /// Their side through `door` instead of their own plain one.
    fn theirs_through(&self, door: Arc<dyn Door>) -> Theirs {
        let certs = tempfile::tempdir().unwrap();
        let line = format!(
            "{LAYER} take quic://127.0.0.1:1 {} ledger=ikigai",
            certs.path().display()
        );
        let binding = layer::parse(&line, std::path::Path::new("/nowhere")).unwrap();
        let hub = Arc::new(ikigai_gonk::compose(DurableStore::in_memory().unwrap()));
        let ledger = taker::hub_ledger(&binding, Arc::clone(&hub)).unwrap();
        let taker = Taker::new(binding, door, ledger).unwrap();
        Theirs { hub, taker }
    }
}

/// A layer host from before `urn:layers:me` (ikigai-layers 0.2.1 or older): the same door,
/// answering `NotFound` for that one resource.
struct WithoutMe(AsParticipant);

impl Door for WithoutMe {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        if request.target.as_str() == "urn:layers:me" {
            return Err(Error::NotFound("urn:layers:me".to_string()));
        }
        self.0.call(request)
    }
}

impl Theirs {
    fn gonk(&self, verb: Verb, resource: &str, args: &[(&str, &str)]) -> Result<String, Error> {
        let request = args.iter().fold(
            Request::new(
                verb,
                Iri::parse(format!("urn:iki:ledger:ikigai:{resource}")).unwrap(),
            ),
            |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
        );
        Resolver::issue(&*self.hub, request)
            .map(|(r, _)| String::from_utf8_lossy(&r.bytes).into_owned())
    }

    fn item(&self, number: i64) -> serde_json::Value {
        let answer = self
            .gonk(
                Verb::Source,
                &format!("item:{number}"),
                &[("as", "application/json")],
            )
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&answer).unwrap()["item"].clone()
    }

    fn pass(&self) -> mirror::Report {
        let report = self.taker.pass().expect("their pass");
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        report
    }

    fn reported(&self, class: &str) -> usize {
        self.pass()
            .actions
            .iter()
            .filter(|a| a.starts_with(&format!("reported {class}")))
            .count()
    }
}

#[test]
fn an_offer_taken_by_hand_is_filed_once_and_claimed_on_our_side() {
    let w = World::new();
    let mirror = w.mirror();
    let theirs = w.theirs();
    let n = w.file(
        "Port the importer\n\nThe importer reads the old format.",
        &[("about", "https://github.com/ikigai-rs/ikigai-core")],
    );
    let offer = offered(&pass(&mirror));
    let announced = theirs.pass();
    assert!(
        announced.actions.iter().any(|a| a.contains(&offer)),
        "their pass announces the open offer: {:?}",
        announced.actions
    );

    let Took::Filed(local) = theirs.taker.take(&offer).unwrap() else {
        panic!("not filed")
    };
    let item = theirs.item(local);
    assert_eq!(item["title"], "Port the importer");
    assert!(item["body"]
        .as_str()
        .unwrap()
        .starts_with("The importer reads the old format."));
    assert_eq!(item["key"], taker::filed_key(LAYER, &offer));
    let about: Vec<&str> = item["about"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(about.contains(&offer.as_str()), "{about:?}");
    assert!(about.contains(&"https://github.com/ikigai-rs/ikigai-core"));
    // Run twice, filed once.
    assert_eq!(theirs.taker.take(&offer).unwrap(), Took::Already(local));

    pass(&mirror);
    assert_eq!(w.item(n)["claim"]["holder"], them());
}

#[test]
fn their_close_hands_back_with_evidence_and_our_item_waits_for_its_dispatcher() {
    let w = World::new();
    let mirror = w.mirror();
    let theirs = w.theirs();
    let n = w.file("Fix the parser\n\nIt drops the last line.", &[]);
    let offer = offered(&pass(&mirror));
    let Took::Filed(local) = theirs.taker.take(&offer).unwrap() else {
        panic!("not filed")
    };
    pass(&mirror);
    // Open: a Progress a day, and only one.
    assert_eq!(theirs.reported("Progress"), 1);
    assert_eq!(theirs.reported("Progress"), 0);
    let report = pass(&mirror);
    assert!(
        report.actions.iter().any(|a| a.starts_with("renewed ")),
        "{:?}",
        report.actions
    );

    theirs
        .gonk(
            Verb::Sink,
            "comment",
            &[
                ("item", &local.to_string()),
                (
                    "content",
                    "Merged: https://github.com/ikigai-rs/ikigai-core/pull/41.",
                ),
            ],
        )
        .unwrap();
    theirs
        .gonk(
            Verb::Sink,
            "close",
            &[("item", &local.to_string()), ("reason", "done")],
        )
        .unwrap();
    assert_eq!(theirs.reported("Handback"), 1);
    assert_eq!(
        theirs.reported("Handback"),
        0,
        "a hand-back is dropped once"
    );

    pass(&mirror);
    let ours = w.item(n);
    assert_eq!(ours["awaiting"], "dispatcher");
    assert_eq!(ours["status"], "open", "nothing closes across");
    let handed: Vec<String> = w
        .comments(n)
        .into_iter()
        .filter(|c| c.starts_with("Handed back"))
        .collect();
    assert_eq!(handed.len(), 1);
    assert!(handed[0].contains("https://github.com/ikigai-rs/ikigai-core/pull/41"));
    settled(&mirror);
}

#[test]
fn their_close_without_done_releases_and_we_offer_again() {
    let w = World::new();
    let mirror = w.mirror();
    let theirs = w.theirs();
    let n = w.file("Measure the cache\n\nBefore and after.", &[]);
    let first = offered(&pass(&mirror));
    let Took::Filed(local) = theirs.taker.take(&first).unwrap() else {
        panic!("not filed")
    };
    pass(&mirror);
    theirs
        .gonk(
            Verb::Sink,
            "close",
            &[("item", &local.to_string()), ("reason", "wontfix")],
        )
        .unwrap();
    assert_eq!(theirs.reported("Release"), 1);
    let report = pass(&mirror);
    let second = offered(&report);
    assert_ne!(first, second);
    assert!(w.item(n)["claim"].is_null());
    assert_eq!(theirs.reported("Release"), 0);
}

#[test]
fn a_take_whose_reply_was_lost_is_found_by_its_receipt() {
    let w = World::new();
    let mirror = w.mirror();
    let theirs = w.theirs();
    w.file("First\n\nOne.", &[]);
    let offers: Vec<String> = pass(&mirror)
        .actions
        .iter()
        .filter(|a| a.starts_with("offered "))
        .map(|a| {
            a.rsplit('<')
                .next()
                .unwrap()
                .trim_end_matches('>')
                .to_string()
        })
        .collect();
    assert_eq!(offers.len(), 1);
    // ★ BEFORE this server has taken anything (ledger #1316): the door says who it is
    // (`urn:layers:me`), so the receipt is recognized as its own and the offer is filed. Through
    // gonk 0.x this was "cannot tell whether that was itself after a lost reply".
    w.take(&them(), &offers[0]).unwrap();
    let Took::Already(filed) = theirs
        .taker
        .take(&offers[0])
        .expect("the lost reply resolves")
    else {
        panic!("a take already made is Already")
    };
    assert_eq!(
        theirs.item(filed)["key"],
        taker::filed_key(LAYER, &offers[0])
    );
    // Someone else's take is not ours.
    w.file("Third\n\nThree.", &[]);
    let third = offered(&pass(&mirror));
    w.take(&participant("bobby"), &third).unwrap();
    let theirs_not = theirs.taker.take(&third).unwrap_err();
    assert!(theirs_not.contains(&participant("bobby")), "{theirs_not}");
}

/// From a host that does not serve `urn:layers:me` (before ikigai-layers 0.2.2) the taker infers
/// who it is from what its ledger holds, as before: nothing before the first take, and a lost
/// reply on another offer once it holds one.
#[test]
fn without_urn_layers_me_a_lost_reply_resolves_only_after_the_first_take() {
    let w = World::new();
    let mirror = w.mirror();
    let theirs = w.theirs_through(Arc::new(WithoutMe(w.door(&them()))));
    w.file("First\n\nOne.", &[]);
    w.file("Second\n\nTwo.", &[]);
    let offers: Vec<String> = pass(&mirror)
        .actions
        .iter()
        .filter(|a| a.starts_with("offered "))
        .map(|a| {
            a.rsplit('<')
                .next()
                .unwrap()
                .trim_end_matches('>')
                .to_string()
        })
        .collect();
    assert_eq!(offers.len(), 2);
    w.take(&them(), &offers[0]).unwrap();
    let unknown = theirs.taker.take(&offers[0]).unwrap_err();
    assert!(
        unknown.contains("cannot tell") && unknown.contains("urn:layers:me"),
        "{unknown}"
    );
    assert!(matches!(theirs.taker.take(&offers[1]), Ok(Took::Filed(_))));
    assert!(matches!(
        theirs.taker.take(&offers[0]),
        Ok(Took::Already(_))
    ));
}

/// ★ An offer whose item is DELETED is withdrawn, even when it was this ledger's only offer
/// (ledger #1316): the mirror knows itself from `urn:layers:me`. Inferred from the offers it had
/// recorded, as through gonk 0.x, it knew nobody once the only item carrying a record was gone,
/// and the orphan stayed pending in the layer.
#[test]
fn the_only_offer_of_a_deleted_item_is_withdrawn() {
    let w = World::new();
    let mirror = w.mirror();
    let n = w.file("Short-lived\n\nGone soon.", &[]);
    let offer = offered(&pass(&mirror));
    assert!(w.pending().contains(&offer));
    w.gonk(Verb::Delete, &format!("item:{n}"), &[])
        .expect("the owner deletes the item");
    let report = pass(&mirror);
    assert!(
        report
            .actions
            .iter()
            .any(|a| a.contains(&offer) && a.contains("its item is gone")),
        "{:?}",
        report.actions
    );
    assert!(!w.pending().contains(&offer), "withdrawn from the layer");
}
