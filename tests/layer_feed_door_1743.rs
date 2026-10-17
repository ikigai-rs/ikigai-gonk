//! Ledger [#1743](http://localhost:1060/l/default/item/1743): a binding's passes are never held
//! behind its own feed's long poll.
//!
//! Seen on plasma, 2026-10-16: a `gonk.layer` offer binding read the layer's definition and
//! then timed out on `urn:layer:{layer}:context` at the 15 s deadline, pass after pass, while
//! a fresh process with the same certificates read the same names in 0.3 s. The difference
//! was concurrency inside gonk. The binding's feed thread long-polls the door
//! (`urn:layer:{layer}:feed … wait=50`), and it did so on the SAME [`LazyMount`] the passes
//! use: one QUIC connection. ikigai-quic's door (through 0.1.63) answers one connection's
//! calls one at a time, so a pass call queued behind the 50-second poll and missed its
//! 15-second deadline. The mirror and taker tests use doors with no feed, which hid it.
//!
//! The host here is the published `ikigai-layers` behind a real QUIC door (`ikigai-quic`'s
//! `serve_shared_on`, the door the edge runs), with a scripted feed in front that holds every
//! read for its `wait` and says when one is in progress, so the test knows the long poll is
//! being served before it writes. The bindings are started by [`layer::start`], the function
//! `ikigai-gonk serve` calls, in both roles: the offer role (plasma's gonk) and the take role
//! (Bobby's gonk, which showed the same symptom). In process, on `127.0.0.1:0` bound by the server itself; nothing
//! here touches a live service.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Exact, Fallback, FnEndpoint, Iri, Kernel,
    ReprType, Representation, Request, Space, SystemClock, Verb,
};
use ikigai_gonk::layer::{self, Binding, Door};
use ikigai_gonk::taker;
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;

const LAYER: &str = "tasks";
const LEDGER: &str = "crew";
const TUPLES: &str = "urn:layer:tasks:tuples";
const FEED: &str = "urn:layer:tasks:feed";
/// The offerer (plasma's gonk) and the taker (Bobby's).
const OFFERER: &str = "urn:collab:participant:plasma-gonk";
const TAKER: &str = "urn:collab:participant:bobby-gonk";

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
"#;

/// What the enrollment of `principal` gives its certificate at the door.
fn grant(principal: &str) -> Capability {
    let mut scopes: Vec<String> = ["read", "feed", "out", "take"]
        .iter()
        .map(|v| format!("urn:cap:layer:{v}:{LAYER}"))
        .collect();
    scopes.push("urn:cap:collab:read:crew".to_string());
    Capability::scoped(scopes)
        .with_principal(principal)
        .expect("a principal")
}

/// The layer host, served over QUIC to the certificate enrolled as `us`, and what the test
/// reads of it.
struct Host {
    us: &'static str,
    addr: SocketAddr,
    certs: tempfile::TempDir,
    /// Feed reads the door is serving right now.
    polling: Arc<AtomicUsize>,
    /// The host's kernel, for reading the layer in process (not through the door under test).
    layers: Arc<Kernel>,
    _config: tempfile::TempDir,
    _state: tempfile::TempDir,
}

impl Host {
    fn start(us: &'static str) -> Host {
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
        write("participants.ttl", PARTICIPANTS);
        write("collabs/crew.toml", CREW);
        let state = tempfile::tempdir().unwrap();
        let host = ikigai_layers::Host::open(config.path(), state.path()).expect("the host");

        // The feed, scripted: every read is held for its `wait` (the long poll an idle layer
        // gives), counted while it is held, and answers no events.
        let polling = Arc::new(AtomicUsize::new(0));
        let held = Arc::clone(&polling);
        let feed = FnEndpoint::new("feed", move |inv| {
            let wait = inv
                .inline_str("wait")
                .ok()
                .and_then(|w| w.trim().parse::<u64>().ok())
                .unwrap_or(0);
            held.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_secs(wait));
            held.fetch_sub(1, Ordering::SeqCst);
            Ok(Representation::new(
                ReprType::new("application/json"),
                br#"{"events":[],"last":0}"#.to_vec(),
            ))
        })
        .with_description(Description::new("feed").verb(Verb::Source));
        let participants = FnEndpoint::new("collab-participants", |_inv| {
            Ok(Representation::new(
                ReprType::new("text/turtle"),
                PARTICIPANTS.as_bytes().to_vec(),
            ))
        })
        .with_description(Description::new("collab-participants").verb(Verb::Source));
        let spaces: Vec<Arc<dyn Space>> = vec![
            Arc::new(
                EndpointSpace::new()
                    .bind(Exact::new(FEED), feed)
                    .bind(Exact::new("urn:collab:crew:participants"), participants),
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

        let certs = tempfile::tempdir().unwrap();
        let (server, client) = (ikigai_quic::generate(), ikigai_quic::generate());
        for (file, pem) in [
            ("server.crt", &server.cert_pem),
            ("client.crt", &client.cert_pem),
            ("client.key", &client.key_pem),
        ] {
            std::fs::write(certs.path().join(file), pem).unwrap();
        }
        let minter: ikigai_quic::Minter = Arc::new(move |_| {
            Some(ikigai_quic::Session {
                capability: grant(us),
                file_segment: String::new(),
                principal: Some(us.to_string()),
            })
        });
        // Bound by the server itself, never bind-and-release (field guide 9h).
        let udp = ikigai_quic::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = udp.local_addr().unwrap();
        let trusted = vec![client.cert_pem];
        let kernel = Arc::clone(&layers);
        std::thread::spawn(move || {
            ikigai_quic::serve_shared_on(
                kernel,
                udp,
                &server,
                &trusted,
                minter,
                ikigai_quic::DEFAULT_IDLE_TIMEOUT,
                None,
            )
        });
        Host {
            us,
            addr,
            certs,
            polling,
            layers,
            _config: config,
            _state: state,
        }
    }

    fn binding(&self, role: &str) -> Binding {
        let line = format!(
            "{LAYER} {role} quic://{} {} ledger={LEDGER} collab=crew",
            self.addr,
            self.certs.path().display()
        );
        layer::parse(&line, std::path::Path::new("/nowhere")).unwrap()
    }

    /// `principal`'s door to the host in process, not through the QUIC door under test.
    fn in_process(&self, principal: &'static str) -> Arc<dyn Door> {
        Arc::new(InProcess {
            kernel: Arc::clone(&self.layers),
            principal,
        })
    }

    /// The lines of the pending tuples' listing that contain `needle`.
    fn pending(&self, needle: &str) -> usize {
        let request = Request::new(Verb::Source, Iri::parse(TUPLES).unwrap())
            .with_arg("limit", ArgRef::Inline(b"1000".to_vec()));
        let answer = self.in_process(self.us).call(request).unwrap();
        String::from_utf8_lossy(&answer.bytes)
            .lines()
            .filter(|line| line.contains(needle))
            .count()
    }

    /// Wait until the door is serving a feed read (the long poll).
    fn polling(&self) {
        until(
            Duration::from_secs(20),
            "the door is serving the feed's long poll",
            || self.polling.load(Ordering::SeqCst) > 0,
        );
    }
}

/// A participant at the host, in process: the door's mint of `principal`.
struct InProcess {
    kernel: Arc<Kernel>,
    principal: &'static str,
}

impl Door for InProcess {
    fn call(&self, request: Request) -> Result<Representation, ikigai_core::Error> {
        block_on(Kernel::issue(&self.kernel, request, &grant(self.principal)))
    }
}

/// File an item on our gonk, as its owner.
fn file(hub: &Kernel, content: &str) {
    let request = Request::new(
        Verb::Sink,
        Iri::parse(format!("urn:iki:ledger:{LEDGER}:append")).unwrap(),
    )
    .with_arg("content", ArgRef::Inline(content.as_bytes().to_vec()));
    Resolver::issue(hub, request).expect("filing an item");
}

fn until(within: Duration, what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + within;
    while !check() {
        assert!(Instant::now() < deadline, "{what} within {within:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The deadline a pass's effect must show within: under the layer door's call deadline, since a
/// pass call queued behind the poll fails AT it.
fn within() -> Duration {
    layer::CALL_TIMEOUT - Duration::from_secs(5)
}

/// ★ The reproduction, OFFER role. The bindings are started the way `serve` starts them; once
/// the door is serving the feed's long poll, an item is filed. Before: every pass call queued
/// behind the poll on the one connection and failed at the 15 s deadline (`reading
/// urn:layer:tasks:context: timeout`), so no offer appeared. After: the feed has a connection
/// of its own and the offer appears at once.
#[test]
fn an_offer_pass_is_not_held_behind_the_feeds_long_poll() {
    let host = Host::start(OFFERER);
    let bindings = vec![host.binding("offer")];
    let mounted = layer::mount(&bindings);
    let hub = Arc::new(ikigai_gonk::compose(DurableStore::in_memory().unwrap()));
    layer::start(&bindings, &mounted, &hub).expect("the bindings start");

    host.polling();
    file(&hub, "Offer me\n\nFiled while the feed waits.");
    until(within(), "an offer in the layer", || {
        host.pending(":offer:") > 0
    });
    assert!(
        host.polling.load(Ordering::SeqCst) > 0,
        "the offer came while the feed's long poll was still held"
    );
}

/// ★ The same, TAKE role (seen on Bobby's gonk, 2026-10-16: the definition read, the context
/// timed out, nothing since). An offer is dropped by the offerer and taken by hand; once the
/// door is serving the taker's feed, the take files the item, and the taker's pass owes the
/// offerer a `Progress`. Before: that pass queued behind the poll and failed at the deadline.
#[test]
fn a_take_pass_is_not_held_behind_the_feeds_long_poll() {
    let host = Host::start(TAKER);
    let offer = format!("urn:layer:{LAYER}:offer:held-behind-the-feed");
    host.in_process(OFFERER)
        .call(
            Request::new(Verb::Sink, Iri::parse(TUPLES).unwrap()).with_arg(
                "content",
                ArgRef::Inline(
                    format!(
                        "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
                         @prefix dcterms: <http://purl.org/dc/terms/> .\n\
                         <{offer}> a ik:Offer ; dcterms:title \"Take me\" ; \
                         ik:brief \"Taken while the feed waits.\" .\n"
                    )
                    .into_bytes(),
                ),
            ),
        )
        .expect("the offerer drops an offer");
    let binding = host.binding("take");
    let bindings = vec![binding.clone()];
    let mounted = layer::mount(&bindings);
    let hub = Arc::new(ikigai_gonk::compose(DurableStore::in_memory().unwrap()));
    layer::start(&bindings, &mounted, &hub).expect("the bindings start");

    host.polling();
    // The take by hand, in process (the command goes through the running gonk's socket): it
    // writes the ledger, and the write kicks the taker's pass.
    let ledger = taker::hub_ledger(&binding, Arc::clone(&hub)).unwrap();
    let took = taker::take(&binding, host.in_process(TAKER), &ledger, &offer).expect("the take");
    assert!(matches!(took, taker::Took::Filed(_)), "{took:?}");
    until(within(), "the taker's Progress in the layer", || {
        host.pending(":progress:") > 0
    });
    assert!(
        host.polling.load(Ordering::SeqCst) > 0,
        "the report came while the feed's long poll was still held"
    );
}
