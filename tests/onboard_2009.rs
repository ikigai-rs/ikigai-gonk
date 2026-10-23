//! Ledger [#2009](http://localhost:1062/l/default/item/2009), arc B: the onboarding controller
//! (invite, pending, approve, revoke) and gonk's own apply, end to end in process.
//!
//! The layer host is the published `ikigai-layers` 0.4.0 with its own onboarding pair
//! (`tests/fixtures/onboard/`, copied verbatim from the release), behind doors that mint what a
//! QUIC door would: the CONTROLLER (`brian`) as its manifest grants it, an UNENROLLED certificate
//! as `onboard::lobby_capability` (say on the lobby and nothing else), and an ENROLLED one from the
//! `clients.json` and `grants.json` the host itself wrote when it applied an entitlement. gonk is
//! `compose` over an in-memory store; its controller key is minted by ikigai-secret's own
//! `generate` into an in-memory keystore. Every certificate is a real one (`ikigai_quic::generate`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

mod layers_key;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, Fallback, Iri, Kernel, Representation, Request, Space, SystemClock,
    Verb,
};
use ikigai_gonk::layer::{self, Door};
use ikigai_gonk::mirror::{self, Mirror};
use ikigai_gonk::onboard::{self, Onboard, Parts, Verdict};
use ikigai_gonk::quic::{self, Layout};
use ikigai_gonk::taker::{self, Taker, Took};
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;

const ONB: &str = "https://ikigai-rs.dev/ns/onboard#";
const KINDS: &str = include_str!("../docs/onboard/kinds.ttl");

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

const ONBOARD_PAIR: &[(&str, &str)] = &[
    (
        "layers/enroll.toml",
        include_str!("fixtures/onboard/layers/enroll.toml"),
    ),
    (
        "layers/enroll.shapes.ttl",
        include_str!("fixtures/onboard/layers/enroll.shapes.ttl"),
    ),
    (
        "layers/enroll.context.jsonld",
        include_str!("fixtures/onboard/layers/enroll.context.jsonld"),
    ),
    (
        "layers/grants.toml",
        include_str!("fixtures/onboard/layers/grants.toml"),
    ),
    (
        "layers/grants.shapes.ttl",
        include_str!("fixtures/onboard/layers/grants.shapes.ttl"),
    ),
    (
        "layers/grants.context.jsonld",
        include_str!("fixtures/onboard/layers/grants.context.jsonld"),
    ),
];

fn participant(slug: &str) -> String {
    format!("urn:collab:participant:{slug}")
}

/// An in-memory keystore: the controller key and the open invites' codes.
#[derive(Default)]
struct Memory(Mutex<HashMap<String, Vec<u8>>>);

impl ikigai_secret::Backend for Memory {
    fn get(&self, name: &str) -> ikigai_core::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &[u8]) -> ikigai_core::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(name.to_string(), value.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> ikigai_core::Result<()> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

/// A keystore holding a freshly minted controller key, and its public half.
fn keystore() -> (Arc<Memory>, String) {
    let keys = Arc::new(Memory::default());
    let present: Arc<dyn ikigai_secret::PresenceGate> = Arc::new(|_: &str| Ok(()));
    let public = onboard::generate_key(
        Arc::clone(&keys) as Arc<dyn ikigai_secret::Backend>,
        Some(present),
        onboard::DEFAULT_KEY,
    )
    .unwrap();
    (keys, public)
}

/// A door's mint: `capability` (its principal included) at the host's kernel.
struct AsDoor {
    kernel: Arc<Kernel>,
    capability: Capability,
}

impl Door for AsDoor {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        block_on(Kernel::issue(&self.kernel, request, &self.capability))
    }
}

/// A layer host with the onboarding pair, a `tasks` tuplespace in collaboration `crew`, and
/// `controller` as `onboard.controller`, trusting `controller_key`.
struct Edge {
    config: tempfile::TempDir,
    _state: tempfile::TempDir,
    kernel: Arc<Kernel>,
    controller: String,
}

impl Edge {
    fn new(controller: &str, controller_key: &str) -> Edge {
        let config = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = config.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        for (rel, text) in ONBOARD_PAIR {
            write(rel, text);
        }
        write(
            "collabs/onboard.toml",
            &format!(
                "label = \"Onboarding\"\n\n[[mount]]\nlayer = \"enroll\"\n{controller} = [\"read\", \"feed\"]\n\n\
                 [[mount]]\nlayer = \"grants\"\n{controller} = [\"read\", \"feed\", \"say\"]\n"
            ),
        );
        write("layers/tasks.shapes.ttl", TASK_SHAPES);
        write("layers/tasks.context.jsonld", TASK_CONTEXT);
        write(
            "layers/tasks.toml",
            "model = \"tuplespace\"\nshapes = \"tasks.shapes.ttl\"\ncontext = \"tasks.context.jsonld\"\naddressed = true\n",
        );
        write(
            "participants.ttl",
            &format!(
                "@prefix foaf: <http://xmlns.com/foaf/0.1/> .\n@prefix prov: <http://www.w3.org/ns/prov#> .\n\
                 @prefix ik: <https://ikigai-rs.dev/ns#> .\n\
                 <urn:collab:participant:{controller}> a foaf:Person .\n\
                 <urn:collab:participant:{controller}-gonk> a ik:Machine ; prov:actedOnBehalfOf <urn:collab:participant:{controller}> .\n"
            ),
        );
        write(
            "collabs/crew.toml",
            &format!(
                "[[mount]]\nlayer = \"tasks\"\n{controller}-gonk = [\"read\", \"feed\", \"out\", \"take\"]\n\
                 {controller} = [\"read\", \"feed\", \"out\", \"take\"]\n"
            ),
        );
        write("controller.pub.pem", controller_key);
        write(
            "layers.toml",
            &format!(
                "[onboard]\ngrants = \"grants\"\ncontroller = \"{controller}\"\n\
                 controller_key = \"controller.pub.pem\"\ntrust_dir = \"trust\"\ntrust_reload = \"live\"\n"
            ),
        );
        let state = tempfile::tempdir().unwrap();
        let key = layers_key::state_key(config.path());
        let host = ikigai_layers::Host::open(config.path(), state.path(), &key).expect("the host");
        let spaces: Vec<Arc<dyn Space>> = vec![
            Arc::new(ikigai_layers::space(&host)),
            Arc::new(ikigai_shacl::space()),
        ];
        let kernel = Arc::new(
            Kernel::with_meta_renderer(
                Arc::new(Fallback::new(spaces)),
                Arc::new(ikigai_vocab::TurtleRenderer),
            )
            .with_clock(Arc::new(SystemClock)),
        );
        Edge {
            config,
            _state: state,
            kernel,
            controller: controller.to_string(),
        }
    }

    fn door(&self, capability: Capability) -> Arc<dyn Door> {
        Arc::new(AsDoor {
            kernel: Arc::clone(&self.kernel),
            capability,
        })
    }

    /// The controller, as its manifest grants it.
    fn controller_door(&self) -> Arc<dyn Door> {
        let scopes = [
            "urn:cap:layer:read:enroll",
            "urn:cap:layer:feed:enroll",
            "urn:cap:layer:read:grants",
            "urn:cap:layer:feed:grants",
            "urn:cap:layer:say:grants",
            "urn:cap:collab:read:onboard",
        ];
        self.door(
            Capability::scoped(scopes)
                .with_principal(&participant(&self.controller))
                .unwrap(),
        )
    }

    /// The controller's gonk, as the crew manifest grants it (the offer side of `tasks`).
    fn gonk_door(&self) -> Arc<dyn Door> {
        let scopes = [
            "urn:cap:layer:read:tasks",
            "urn:cap:layer:feed:tasks",
            "urn:cap:layer:out:tasks",
            "urn:cap:layer:take:tasks",
            "urn:cap:collab:read:crew",
        ];
        self.door(
            Capability::scoped(scopes)
                .with_principal(&participant(&format!("{}-gonk", self.controller)))
                .unwrap(),
        )
    }

    /// A certificate nobody enrolled: say on the lobby, nothing else.
    fn lobby_door(&self, fingerprint: &str) -> Arc<dyn Door> {
        self.door(ikigai_layers::onboard::lobby_capability("enroll", "edge", fingerprint).unwrap())
    }

    /// An ENROLLED certificate, as the door reads the files the host wrote: `None` when the
    /// host enrolls it under nothing.
    fn enrolled_door(&self, fingerprint: &str) -> Option<Arc<dyn Door>> {
        let clients: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(self.config.path().join("clients.json")).ok()?,
        )
        .unwrap();
        let grants: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(self.config.path().join("grants.json")).unwrap(),
        )
        .unwrap();
        let client = &clients["clients"][fingerprint];
        let grant = client["grant"].as_str()?;
        let scopes: Vec<String> = grants[grant]
            .as_array()?
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        Some(
            self.door(
                Capability::scoped(scopes)
                    .with_principal(client["principal"].as_str().unwrap())
                    .unwrap(),
            ),
        )
    }

    fn trusted(&self, fingerprint: &str) -> bool {
        self.config
            .path()
            .join("trust")
            .join(format!("{fingerprint}.crt"))
            .is_file()
    }
}

/// A gonk with an onboarding controller at `edge`, its key in `keys`.
struct Gonk {
    _home: tempfile::TempDir,
    hub: Arc<Kernel>,
    layout: Layout,
    onboard: Arc<Onboard>,
    clock: Arc<AtomicU64>,
}

const NOW: u64 = 1_800_000_000_000;

impl Gonk {
    fn new(edge: &Edge, keys: Arc<Memory>, public: &str) -> Gonk {
        Gonk::with(
            edge,
            keys,
            public,
            onboard::GonkDoor::Default { port: None },
            KINDS,
        )
    }

    /// A gonk whose `gonk.onboard.door` is `door`, over the kinds file `kinds_ttl`.
    fn with(
        edge: &Edge,
        keys: Arc<Memory>,
        public: &str,
        door: onboard::GonkDoor,
        kinds_ttl: &str,
    ) -> Gonk {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(home.path());
        let kinds = home.path().join("kinds.ttl");
        std::fs::write(&kinds, kinds_ttl).unwrap();
        let clock = Arc::new(AtomicU64::new(NOW));
        let ticks = Arc::clone(&clock);
        let onboard = Onboard::new(Parts {
            door: edge.controller_door(),
            door_authority: "edge.example:4434".to_string(),
            door_server_cert: ikigai_quic::generate().cert_pem,
            lobby: "enroll".to_string(),
            grants: "grants".to_string(),
            keystore: keys,
            key: onboard::DEFAULT_KEY.to_string(),
            controller_key: public.to_string(),
            kinds,
            gonk_door: door,
            layout: layout.clone(),
            clock: Arc::new(move || ticks.load(Ordering::SeqCst)),
        });
        let hub = Arc::new(ikigai_gonk::compose_with(
            DurableStore::in_memory().unwrap(),
            None,
            Vec::new(),
            vec![Arc::new(onboard.space())],
            None,
            None,
            None,
        ));
        onboard.attach(&hub);
        Gonk {
            _home: home,
            hub,
            layout,
            onboard,
            clock,
        }
    }

    fn enrolled_grant(&self, fingerprint: &str) -> Option<String> {
        quic::read_enrolment(&self.layout.clients_json())
            .unwrap()
            .and_then(|e| e.grant_for(fingerprint).map(str::to_string))
    }

    fn advance_hours(&self, hours: u64) {
        self.clock
            .fetch_add(hours * 60 * 60 * 1000, Ordering::SeqCst);
    }
}

/// A node: its own certificate, made here, the key never leaving.
struct Node {
    cert: String,
    fingerprint: String,
}

fn node() -> Node {
    let cert = ikigai_quic::generate().cert_pem;
    let fingerprint = ikigai_quic::fingerprint_of_pem(&cert).unwrap();
    Node { cert, fingerprint }
}

fn literal(value: &str) -> String {
    oxigraph::model::Literal::new_simple_literal(value).to_string()
}

impl Node {
    /// Post the request `worker.py onboard` posts, with the proof under `code`. Answers the
    /// lobby entry's id.
    fn request(&self, edge: &Edge, invite: &str, code: &str) -> String {
        let turtle = format!(
            "@prefix onb: <{ONB}> .\n<urn:iki:onboard:request:{fp}> a onb:EnrollmentRequest ;\n  \
             onb:invite <urn:iki:onboard:invite:{invite}> ;\n  onb:certificate {cert} ;\n  \
             onb:fingerprint {fpl} ;\n  onb:proof {proof} .\n",
            fp = self.fingerprint,
            cert = literal(&self.cert),
            fpl = literal(&self.fingerprint),
            proof = literal(&onboard::proof(code, &self.fingerprint)),
        );
        let answer = edge
            .lobby_door(&self.fingerprint)
            .call(
                Request::new(Verb::Sink, Iri::parse("urn:layer:enroll:say").unwrap())
                    .with_arg("content", ArgRef::Inline(turtle.into_bytes())),
            )
            .expect("the lobby takes a request");
        let text = String::from_utf8(answer.bytes).unwrap();
        let at = text.find("urn:layer:enroll:entry:").expect("an entry");
        text[at + "urn:layer:enroll:entry:".len()..]
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect()
    }
}

fn kinds(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A controller, a host trusting it, a gonk holding its key.
fn world(controller: &str) -> (Edge, Gonk) {
    let (keys, public) = keystore();
    let edge = Edge::new(controller, &public);
    let gonk = Gonk::new(&edge, keys, &public);
    (edge, gonk)
}

#[test]
fn invite_to_approval_to_apply_end_to_end() {
    let (edge, a) = world("brian");
    let minted = a
        .onboard
        .invite("mini", &kinds(&["worker", "task-member"]))
        .unwrap();
    assert_eq!(minted.code.len(), onboard::CODE_LEN);
    assert_eq!(
        minted.pin,
        ikigai_quic::fingerprint_of_pem(&minted.server_cert).unwrap()
    );
    let mini = node();
    let id = mini.request(&edge, &minted.invite.id, &minted.code);

    // Pending, already checked.
    let pending = a.onboard.pending().unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].id, id);
    assert_eq!(pending[0].verdict, Ok(()), "{pending:?}");
    assert_eq!(pending[0].name.as_deref(), Some("mini"));
    // The invite in the store carries no code.
    let invite = Resolver::issue(
        &*a.hub,
        Request::new(
            Verb::Source,
            Iri::parse(format!("urn:iki:onboard:invite:{}", minted.invite.id)).unwrap(),
        ),
    )
    .unwrap()
    .0;
    assert!(!String::from_utf8_lossy(&invite.bytes).contains(&minted.code));

    // Approve: a verified entitlement, applied on both sides.
    let (said, status) = a.onboard.approve(&id).unwrap();
    assert!(said.starts_with("urn:layer:grants:entry:"), "{said}");
    assert_eq!(status.error, None, "{status:?}");
    let record = status.records.iter().find(|r| r.entry == said).unwrap();
    assert_eq!(record.verdict, Verdict::InForce, "{status:?}");
    // gonk's half: the certificate the node made, under the worker kind's grant.
    assert_eq!(
        a.enrolled_grant(&mini.fingerprint).as_deref(),
        Some("onb-mini")
    );
    let grants = quic::read_grants(&a.layout.grants_json()).unwrap();
    assert_eq!(
        grants["onb-mini"],
        onboard::gonk_scopes("default=worker").unwrap()
    );
    let bundle = a.layout.clients_dir().join("onb-mini").join("client.crt");
    assert_eq!(std::fs::read_to_string(bundle).unwrap(), mini.cert);
    assert!(!a
        .layout
        .clients_dir()
        .join("onb-mini")
        .join("client.key")
        .exists());
    assert!(
        status.restart_pending,
        "a newly trusted certificate needs a restart"
    );
    // The host's half: trusted, and enrolled with the task-member kind's grants.
    assert!(edge.trusted(&mini.fingerprint));
    let door = edge.enrolled_door(&mini.fingerprint).expect("enrolled");
    door.call(Request::new(
        Verb::Source,
        Iri::parse("urn:layer:tasks:tuples").unwrap(),
    ))
    .expect("the node reads the task layer");
    let own = door
        .call(
            Request::new(
                Verb::Source,
                Iri::parse("urn:layer:grants:entries").unwrap(),
            )
            .with_arg(
                "type",
                ArgRef::Inline(format!("{ONB}Entitlement").into_bytes()),
            ),
        )
        .expect("the node reads its own entitlement");
    let own = String::from_utf8(own.bytes).unwrap();
    assert!(own.contains(&mini.fingerprint));
    assert!(own.contains("default=worker"), "both kinds in one: {own}");
    assert!(own.contains("urn:cap:layer:take:tasks"), "{own}");
    assert!(a.onboard.pending().unwrap().is_empty());

    // Idempotent by fingerprint.
    let again = a.onboard.apply();
    assert_eq!(again.enrolled[0].2, "in force, unchanged", "{again:?}");

    // Approving twice is refused.
    assert!(matches!(a.onboard.approve(&id), Err(Error::Conflict(_))));

    // A wrong proof, a reused invite and an expired invite are each refused in pending.
    let other = a
        .onboard
        .invite("node-b", &kinds(&["task-member"]))
        .unwrap();
    let wrong = node();
    let wrong_id = wrong.request(&edge, &other.invite.id, "AAAAAAAAAA");
    let reused = node();
    let reused_id = reused.request(&edge, &minted.invite.id, &minted.code);
    let late_invite = a
        .onboard
        .invite("node-c", &kinds(&["task-member"]))
        .unwrap();
    let late = node();
    let late_id = late.request(&edge, &late_invite.invite.id, &late_invite.code);
    a.advance_hours(onboard::INVITE_HOURS + 1);
    let pending = a.onboard.pending().unwrap();
    let why = |id: &str| {
        pending
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("{id} pending: {pending:?}"))
            .verdict
            .clone()
            .unwrap_err()
    };
    // The wrong proof's invite has expired by now too; the reason in force first is the expiry,
    // so the proof is checked here before the clock moves.
    assert!(
        why(&reused_id).contains("already used"),
        "{}",
        why(&reused_id)
    );
    assert!(why(&late_id).contains("expired"), "{}", why(&late_id));
    a.clock.store(NOW, Ordering::SeqCst);
    let checked = a.onboard.check(&wrong_id).unwrap();
    assert!(
        checked.verdict.as_ref().unwrap_err().contains("proof"),
        "{checked:?}"
    );
    assert!(matches!(
        a.onboard.approve(&wrong_id),
        Err(Error::Conflict(_))
    ));
    assert!(edge.enrolled_door(&wrong.fingerprint).is_none());
}

#[test]
fn every_write_needs_the_owners_socket_and_the_page_token_reads() {
    let (_edge, a) = world("brian");
    let page = Capability::scoped([onboard::CAP_READ]);
    let issue = |verb, target: &str, cap: &Capability| {
        block_on(Kernel::issue(
            &a.hub,
            Request::new(verb, Iri::parse(target).unwrap())
                .with_arg("name", ArgRef::Inline(b"mini".to_vec()))
                .with_arg("kind", ArgRef::Inline(b"worker".to_vec()))
                .with_arg("entry", ArgRef::Inline(b"x".to_vec()))
                .with_arg("who", ArgRef::Inline(b"mini".to_vec())),
            cap,
        ))
    };
    for target in [
        onboard::INVITE,
        onboard::APPROVE,
        onboard::REVOKE,
        onboard::APPLY,
    ] {
        assert!(
            matches!(issue(Verb::Sink, target, &page), Err(Error::Denied(_))),
            "{target} under the page's token"
        );
    }
    issue(Verb::Source, onboard::STATUS, &page).expect("the page reads the status");
    issue(Verb::Source, onboard::PENDING, &page).expect("the page reads pending");
    assert!(matches!(
        issue(
            Verb::Source,
            onboard::PENDING,
            &Capability::scoped(Vec::<String>::new())
        ),
        Err(Error::Denied(_))
    ));
    // And the admin token can never be a grant: certificate or passkey.
    assert!(quic::grant_refusal("ops", &[onboard::CAP_ADMIN.to_string()]).is_some());
    // The owner (root, the socket) mints an invite through the resource.
    let minted = issue(Verb::Sink, onboard::INVITE, &Capability::root()).expect("root invites");
    assert!(String::from_utf8_lossy(&minted.bytes).contains("code"));
}

/// Sign `body` (N-Triples about `<urn:layer:new>`) with the key in `keys`, as the layer host's own
/// tests do, and append the signature.
fn signed_by(body: &str, keys: Arc<Memory>) -> String {
    let bytes = ikigai_layers::onboard::signing_bytes_of(body, "urn:layer:new").unwrap();
    let pem = ikigai_secret::Backend::get(&*keys, onboard::DEFAULT_KEY)
        .unwrap()
        .unwrap();
    let key = ikigai_core::FnEndpoint::new("k", move |_inv| {
        Ok(Representation::new(
            ikigai_core::ReprType::new("application/x-pem-file"),
            pem.clone(),
        ))
    })
    .with_description(ikigai_core::Description::new("k").verb(Verb::Source));
    let spaces: Vec<Arc<dyn Space>> = vec![
        Arc::new(ikigai_sign::space()),
        Arc::new(
            ikigai_core::EndpointSpace::new().bind(ikigai_core::Exact::new("urn:test:key"), key),
        ),
    ];
    let signer = Kernel::new(Arc::new(Fallback::new(spaces)));
    let request = Request::new(Verb::Source, Iri::parse("urn:sign:sign").unwrap())
        .with_arg("in", ArgRef::Inline(bytes))
        .with_arg("key", ArgRef::Inline(b"urn:test:key".to_vec()));
    let sig = block_on(Kernel::issue(
        &signer,
        request,
        &Capability::scoped(["urn:cap:sign"]),
    ))
    .unwrap();
    format!(
        "{body}<urn:layer:new> <{ONB}signature> {} .\n",
        literal(&String::from_utf8(sig.bytes).unwrap())
    )
}

#[test]
fn only_the_controllers_signature_is_applied_and_revocation_removes_the_grant() {
    let (edge, a) = world("brian");
    let minted = a.onboard.invite("mini", &kinds(&["worker"])).unwrap();
    let mini = node();
    let id = mini.request(&edge, &minted.invite.id, &minted.code);
    a.onboard.approve(&id).unwrap();
    assert_eq!(
        a.enrolled_grant(&mini.fingerprint).as_deref(),
        Some("onb-mini")
    );
    // A worker-only kind still reads its own entitlement on the host (read on grants).
    assert!(edge.enrolled_door(&mini.fingerprint).is_some());

    // An entitlement under ANY other key, said on the grants layer by the controller's own
    // identity, is not applied.
    let (forger, _) = keystore();
    let evil = node();
    let body = format!(
        "<urn:layer:new> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{ONB}Entitlement> .\n\
         <urn:layer:new> <{ONB}fingerprint> {} .\n\
         <urn:layer:new> <{ONB}principal> <urn:collab:participant:evil> .\n\
         <urn:layer:new> <{ONB}gonkGrant> \"default=write\" .\n\
         <urn:layer:new> <{ONB}layerGrant> <urn:cap:layer:read:grants> .\n",
        literal(&evil.fingerprint)
    );
    edge.controller_door()
        .call(
            Request::new(Verb::Sink, Iri::parse("urn:layer:grants:say").unwrap()).with_arg(
                "content",
                ArgRef::Inline(signed_by(&body, forger).into_bytes()),
            ),
        )
        .expect("the host stores what the controller says, and judges it");
    let status = a.onboard.apply();
    let refused = status
        .records
        .iter()
        .find(|r| matches!(&r.verdict, Verdict::Refused { reason } if reason.contains("does not verify")))
        .unwrap_or_else(|| panic!("the forged entitlement is refused: {status:?}"));
    assert!(refused.fingerprint.is_none());
    assert_eq!(a.enrolled_grant(&evil.fingerprint), None);
    assert!(!a.layout.clients_dir().join("onb-evil").exists());

    // Revocation: a signed supersession with no grant removes gonk's grant and the bundle.
    let (said, status) = a.onboard.revoke("mini").unwrap();
    let record = status.records.iter().find(|r| r.entry == said).unwrap();
    assert_eq!(record.verdict, Verdict::Revoked, "{status:?}");
    assert_eq!(status.removed.len(), 1, "{status:?}");
    assert_eq!(a.enrolled_grant(&mini.fingerprint), None);
    assert!(!a.layout.clients_dir().join("onb-mini").exists());
    assert!(
        !edge.trusted(&mini.fingerprint),
        "the host revoked its half"
    );
    assert!(matches!(a.onboard.revoke("mini"), Err(Error::NotFound(_))));
}

/// Gonk B's side of the task layer: a taker into its own ledger `ikigai`, through `door`.
struct Theirs {
    hub: Arc<Kernel>,
    taker: Taker,
}

impl Theirs {
    fn new(door: Arc<dyn Door>) -> Theirs {
        let certs = tempfile::tempdir().unwrap();
        let line = format!(
            "tasks take quic://127.0.0.1:1 {} ledger=ikigai",
            certs.path().display()
        );
        let binding = layer::parse(&line, Path::new("/nowhere")).unwrap();
        let hub = Arc::new(ikigai_gonk::compose(DurableStore::in_memory().unwrap()));
        let ledger = taker::hub_ledger(&binding, Arc::clone(&hub)).unwrap();
        let taker = Taker::new(binding, door, ledger).unwrap();
        Theirs { hub, taker }
    }

    fn gonk(&self, verb: Verb, resource: &str, args: &[(&str, &str)]) -> String {
        ledger_call(&self.hub, "ikigai", verb, resource, args).unwrap()
    }

    fn reported(&self, class: &str) -> usize {
        let report = self.taker.pass().expect("their pass");
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        report
            .actions
            .iter()
            .filter(|a| a.starts_with(&format!("reported {class}")))
            .count()
    }
}

fn ledger_call(
    hub: &Kernel,
    ledger: &str,
    verb: Verb,
    resource: &str,
    args: &[(&str, &str)],
) -> Result<String, Error> {
    let request = args.iter().fold(
        Request::new(
            verb,
            Iri::parse(format!("urn:iki:ledger:{ledger}:{resource}")).unwrap(),
        ),
        |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
    );
    Resolver::issue(hub, request).map(|(r, _)| String::from_utf8_lossy(&r.bytes).into_owned())
}

fn pass(mirror: &Mirror) -> mirror::Report {
    let report = mirror.pass().expect("a pass");
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    report
}

#[test]
fn two_gonks_across_a_tasks_layer_one_enrolled_through_the_lobby() {
    let (edge, a) = world("brian");
    // Setup: gonk B enrolls through the lobby as a task-member, with its own certificate and its
    // own invite, approved by gonk A's controller.
    let minted = a
        .onboard
        .invite("bobby-gonk", &kinds(&["task-member"]))
        .unwrap();
    let b_cert = node();
    let id = b_cert.request(&edge, &minted.invite.id, &minted.code);
    a.onboard.approve(&id).unwrap();
    // task-member names no gonk grant: nothing is enrolled in gonk A.
    assert_eq!(a.enrolled_grant(&b_cert.fingerprint), None);
    let b_door = edge
        .enrolled_door(&b_cert.fingerprint)
        .expect("the host enrolled B");
    let theirs = Theirs::new(b_door);

    // A's offer side, its ledger `crew` bound to `tasks`.
    let certs = tempfile::tempdir().unwrap();
    let binding = layer::parse(
        &format!(
            "tasks offer quic://127.0.0.1:1 {} ledger=crew collab=crew",
            certs.path().display()
        ),
        Path::new("/nowhere"),
    )
    .unwrap();
    let mirror = Mirror::new(binding, edge.gonk_door(), Arc::clone(&a.hub)).unwrap();
    let filed = ledger_call(
        &a.hub,
        "crew",
        Verb::Sink,
        "append",
        &[
            (
                "content",
                "Port the importer\n\nThe importer reads the old format.",
            ),
            ("as", "application/json"),
        ],
    )
    .unwrap();
    let n = serde_json::from_str::<serde_json::Value>(&filed).unwrap()["item"]["number"]
        .as_i64()
        .unwrap();
    let report = pass(&mirror);
    let offer = report
        .actions
        .iter()
        .find_map(|a| a.strip_prefix("offered "))
        .and_then(|a| a.rsplit('<').next())
        .map(|a| a.trim_end_matches('>').to_string())
        .unwrap_or_else(|| panic!("an offer: {:?}", report.actions));

    // Leg 1: B takes A's offer into B's own ledger.
    let Took::Filed(local) = theirs.taker.take(&offer).unwrap() else {
        panic!("not filed")
    };
    let theirs_item: serde_json::Value = serde_json::from_str(&theirs.gonk(
        Verb::Source,
        &format!("item:{local}"),
        &[("as", "application/json")],
    ))
    .unwrap();
    assert_eq!(theirs_item["item"]["title"], "Port the importer");
    pass(&mirror);
    let ours = |w: &str| -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(
            &ledger_call(
                &a.hub,
                "crew",
                Verb::Source,
                &format!("item:{n}"),
                &[("as", "application/json")],
            )
            .unwrap(),
        )
        .unwrap()["item"][w]
            .clone()
    };
    assert_eq!(
        ours("claim")["holder"],
        participant("bobby-gonk"),
        "A sees the take as B's claim"
    );

    // Leg 2: B's report comes back over the layer.
    theirs.gonk(
        Verb::Sink,
        "comment",
        &[
            ("item", &local.to_string()),
            (
                "content",
                "Merged: https://github.com/ikigai-rs/ikigai-core/pull/41.",
            ),
        ],
    );
    theirs.gonk(
        Verb::Sink,
        "close",
        &[("item", &local.to_string()), ("reason", "done")],
    );
    assert_eq!(theirs.reported("Handback"), 1);

    // Leg 3: A sees it as DATA, and only A's own action closes A's item.
    pass(&mirror);
    assert_eq!(ours("awaiting"), "dispatcher");
    assert_eq!(ours("status"), "open", "nothing closes across");
    let comments: Vec<String> = ours("comments")
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["text"].as_str().unwrap().to_string())
        .collect();
    assert!(
        comments
            .iter()
            .any(|c| c.starts_with("Handed back") && c.contains("pull/41")),
        "{comments:?}"
    );
    ledger_call(
        &a.hub,
        "crew",
        Verb::Sink,
        "close",
        &[("item", &n.to_string()), ("reason", "done")],
    )
    .unwrap();
    assert_eq!(ours("status"), "closed");
}

#[test]
fn the_controller_is_per_gonk() {
    let (_edge_a, a) = world("brian");
    let (edge_b, b) = world("bobby");
    // B onboards ITS worker, with its own key, on its own host.
    let minted = b
        .onboard
        .invite("bobby-worker", &kinds(&["worker"]))
        .unwrap();
    let worker = node();
    let id = worker.request(&edge_b, &minted.invite.id, &minted.code);
    b.onboard.approve(&id).unwrap();
    assert_eq!(
        b.enrolled_grant(&worker.fingerprint).as_deref(),
        Some("onb-bobby-worker")
    );
    assert_eq!(a.enrolled_grant(&worker.fingerprint), None);
    assert!(
        a.onboard.apply().records.is_empty(),
        "A's grants layer is A's"
    );
    // A's invite is not B's: its code means nothing on B's lobby.
    let theirs = a.onboard.invite("mini", &kinds(&["worker"])).unwrap();
    let mini = node();
    let wrong = mini.request(&edge_b, &theirs.invite.id, &theirs.code);
    let checked = b.onboard.check(&wrong).unwrap();
    assert!(
        checked
            .verdict
            .as_ref()
            .unwrap_err()
            .contains("never minted here"),
        "{checked:?}"
    );
}

#[test]
fn the_page_shows_pending_with_the_command_and_offers_no_approval() {
    let (edge, a) = world("brian");
    let minted = a.onboard.invite("mini", &kinds(&["worker"])).unwrap();
    let mini = node();
    let id = mini.request(&edge, &minted.invite.id, &minted.code);
    let config = tempfile::tempdir().unwrap();
    let passkeys = Arc::new(ikigai_gonk::identity::Passkeys::new(
        Layout::in_config_home(config.path()),
        1062,
    ));
    let face = Arc::new(ikigai_gonk::web::Web {
        hub: Arc::clone(&a.hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new(),
        passkeys,
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        toolbox: false,
    });
    let pages = onboard::bind_page(ikigai_gonk::web::space(Arc::clone(&face)), &face);
    let http = ikigai_gonk::doors::http_kernel_serving(Arc::clone(&a.hub), pages, Vec::new(), None);
    let page = |cap: &Capability| {
        let answer = block_on(Kernel::issue(
            &http,
            Request::new(Verb::Source, Iri::parse(onboard::PAGE_IRI).unwrap()),
            cap,
        ))
        .unwrap();
        String::from_utf8(answer.bytes).unwrap()
    };
    let reader = page(&Capability::scoped([onboard::CAP_READ]));
    assert!(reader.contains(&id), "{reader}");
    assert!(
        reader.contains(&format!("ikigai-gonk onboard approve {id}")),
        "{reader}"
    );
    assert!(
        reader.contains("href='/onboard'"),
        "the header offers the page: {reader}"
    );
    assert!(
        !reader.contains("/iki/onboard"),
        "no form posts an approval"
    );
    let stranger = page(&Capability::scoped(Vec::<String>::new()));
    assert!(!stranger.contains(&id));
    assert!(stranger.contains(onboard::CAP_READ), "{stranger}");
    assert!(!stranger.contains("href='/onboard'"));
}

/// The door an approved worker's entitlement advertises, as the grants layer holds it, under
/// `door` and the kinds `kinds_ttl` (ledger #2123): `None` when it carries none.
fn advertised(door: onboard::GonkDoor, kinds_ttl: &str) -> Option<String> {
    let (keys, public) = keystore();
    let edge = Edge::new("brian", &public);
    let a = Gonk::with(&edge, keys, &public, door, kinds_ttl);
    let minted = a.onboard.invite("mini", &kinds(&["worker"])).unwrap();
    let mini = node();
    let id = mini.request(&edge, &minted.invite.id, &minted.code);
    let (said, _) = a.onboard.approve(&id).unwrap();
    let turtle = edge
        .controller_door()
        .call(
            Request::new(Verb::Source, Iri::parse(&said).unwrap())
                .with_arg("as", ArgRef::Inline(b"text/turtle".to_vec())),
        )
        .expect("the controller reads the entitlement");
    let turtle = String::from_utf8(turtle.bytes).unwrap();
    let store = oxigraph::store::Store::new().unwrap();
    store
        .load_from_reader(oxigraph::io::RdfFormat::Turtle, turtle.as_bytes())
        .unwrap();
    let door = oxigraph::model::NamedNode::new(format!("{ONB}gonkDoor")).unwrap();
    let found: Vec<String> = store
        .quads_for_pattern(None, Some(door.as_ref()), None, None)
        .map(|q| match q.unwrap().object {
            oxigraph::model::Term::Literal(l) => l.value().to_string(),
            other => panic!("{other}"),
        })
        .collect();
    assert!(found.len() <= 1, "{found:?}");
    found.into_iter().next()
}

#[test]
fn the_door_is_the_gonk_setting_and_wins_over_a_kinds_door() {
    let with_door = KINDS.replace(
        "onb:router \"edge\" .",
        "onb:router \"edge\" ;\n    onb:gonkDoor \"kind.example:1062\" .",
    );
    assert_ne!(with_door, KINDS, "the fixture gained a kind door");
    let set = || onboard::GonkDoor::Set("gonk.example:1062".to_string());
    // The setting, with no kind door and with one: the setting.
    assert_eq!(
        advertised(set(), KINDS).as_deref(),
        Some("gonk.example:1062")
    );
    assert_eq!(
        advertised(set(), &with_door).as_deref(),
        Some("gonk.example:1062")
    );
    // No setting: a kind's door still counts (older kinds files), else none here (no QUIC port).
    let none = || onboard::GonkDoor::Default { port: None };
    assert_eq!(
        advertised(none(), &with_door).as_deref(),
        Some("kind.example:1062")
    );
    assert_eq!(advertised(none(), KINDS), None);
    // Locally: no door at all, whatever the kind says.
    assert_eq!(advertised(onboard::GonkDoor::Socket, &with_door), None);
}
