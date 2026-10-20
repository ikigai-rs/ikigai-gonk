//! `cargo run --example layers-preview [-- <port>]` — the Layers page over a SCRATCH layer host,
//! served on loopback so a person (or a browser check) can look at it and use it.
//!
//! # ★ Why this exists
//!
//! The Layers page answers only a signed-in passkey holding `urn:cap:gonk:layers:read`, and its
//! layers are a real layer host's. Neither is something a browser check should set up: a
//! passkey enrollment raises a real Touch ID prompt, and the edge door is live. So this serves
//! the same pages, through the same HTTP door, over a layer host in this process (three layers:
//! a channel `talk`, a channel `notes` the identity only reads, a task layer `tasks`), with the
//! identity `urn:collab:participant:brian` minted by an in-process door.
//!
//! ⚠ **It proves how the page looks and behaves, never who may use it.** The ANONYMOUS loopback
//! caller is handed the page's two tokens here, which the real server never does; the tests in
//! `tests/layers_page_1675.rs` hold the real rule. Every 30 seconds `bobby` says something on
//! `talk`, so an open page shows a live update.

use std::sync::Arc;
use std::time::Duration;

// The tests' state-key helper (ikigai-layers 0.3.0 seals a host's state at rest).
#[path = "../tests/layers_key/mod.rs"]
mod layers_key;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, Fallback, Iri, Kernel, Representation, Request, Space, SystemClock,
    Verb,
};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::layer::Door;
use ikigai_gonk::layers_view::{self, LayersView, Waits, CAP_READ, CAP_SAY};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;
use sha2::{Digest, Sha256};

const SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:p:message> a sh:NodeShape ; sh:targetClass ik:Message , ik:Question ; sh:closed true ;
  sh:ignoredProperties ( rdf:type ik:inReplyTo prov:wasAttributedTo dcterms:created ik:seq ) ;
  sh:property [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] .
<urn:p:brief> a sh:NodeShape ; sh:targetClass ik:Brief , ik:Issue ; sh:closed true ;
  sh:ignoredProperties ( rdf:type ik:inReplyTo ik:repository ik:severity prov:wasAttributedTo dcterms:created ik:seq ) ;
  sh:property [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] .
<urn:p:file> a sh:NodeShape ; sh:targetClass ik:File ; sh:closed true ;
  sh:ignoredProperties ( rdf:type ik:fileName ik:mediaType ik:contentHash ik:byteSize ik:encoding ik:inReplyTo prov:wasAttributedTo dcterms:created ik:seq ) ;
  sh:property [ sh:path ik:content ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] .
<urn:p:offer> a sh:NodeShape ; sh:targetClass ik:Offer ; sh:closed true ;
  sh:ignoredProperties ( rdf:type ik:brief prov:wasAttributedTo dcterms:created ik:seq ) ;
  sh:property [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] .
"#;

/// The prefixes every scratch entry is written with.
const IK: &str =
    "@prefix ik: <https://ikigai-rs.dev/ns#> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n";

const CONTEXT: &str =
    r#"{ "@context": { "ik": "https://ikigai-rs.dev/ns#", "Message": "ik:Message" } }"#;

const PARTICIPANTS: &str = r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
<urn:collab:participant:brian> a foaf:Person .
<urn:collab:participant:bobby> a foaf:Person .
"#;

const CREW: &str = r#"[[mount]]
layer = "talk"
brian = ["read", "feed", "say"]
bobby = ["read", "feed", "say"]

[[mount]]
layer = "notes"
brian = ["read", "feed"]
bobby = ["read", "feed", "say"]

[[mount]]
layer = "tasks"
brian = ["read", "feed"]
bobby = ["read", "feed", "out", "take"]
"#;

struct As {
    kernel: Arc<Kernel>,
    capability: Capability,
}

impl Door for As {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        block_on(Kernel::issue(&self.kernel, request, &self.capability))
    }
}

fn grant(slug: &str, held: &[(&str, &[&str])]) -> Capability {
    let mut scopes = vec!["urn:cap:collab:read:crew".to_string()];
    for (layer, verbs) in held {
        scopes.extend(verbs.iter().map(|v| format!("urn:cap:layer:{v}:{layer}")));
    }
    Capability::scoped(scopes)
        .with_principal(&format!("urn:collab:participant:{slug}"))
        .expect("a principal")
}

fn call(door: &As, verb: Verb, iri: &str, content: &str) {
    let request = Request::new(verb, Iri::parse(iri).unwrap())
        .with_arg("content", ArgRef::Inline(content.as_bytes().to_vec()));
    door.call(request).unwrap_or_else(|e| panic!("{iri}: {e}"));
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .map(|p| p.parse().expect("a port"))
        .unwrap_or(0);
    let config = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = config.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("layers/p.shapes.ttl", SHAPES);
    write("layers/p.context.jsonld", CONTEXT);
    for (layer, model, label) in [
        ("talk", "channel", "Talk"),
        ("notes", "channel", "Notes"),
        ("tasks", "tuplespace", "Tasks"),
    ] {
        write(
            &format!("layers/{layer}.toml"),
            &format!(
                "model = \"{model}\"\nlabel = \"{label}\"\nshapes = \"p.shapes.ttl\"\ncontext = \"p.context.jsonld\"\nentry_max = \"512KiB\"\n"
            ),
        );
    }
    write("participants.ttl", PARTICIPANTS);
    write("collabs/crew.toml", CREW);
    let state = tempfile::tempdir().unwrap();
    let key = layers_key::state_key(config.path());
    let host = ikigai_layers::Host::open(config.path(), state.path(), &key).expect("the host");
    let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_layers::space(&host))];
    let kernel = Arc::new(
        Kernel::with_meta_renderer(
            Arc::new(Fallback::new(spaces)),
            Arc::new(ikigai_vocab::TurtleRenderer),
        )
        .with_clock(Arc::new(SystemClock)),
    );
    let bobby = As {
        kernel: Arc::clone(&kernel),
        capability: grant(
            "bobby",
            &[
                ("talk", &["read", "feed", "say"]),
                ("notes", &["read", "feed", "say"]),
                ("tasks", &["read", "feed", "out", "take"]),
            ],
        ),
    };
    let brian = As {
        kernel: Arc::clone(&kernel),
        capability: grant(
            "brian",
            &[
                ("talk", &["read", "feed", "say"]),
                ("notes", &["read", "feed"]),
                ("tasks", &["read", "feed"]),
            ],
        ),
    };
    let say = |door: &As, layer: &str, body: &str| {
        call(
            door,
            Verb::Sink,
            &format!("urn:layer:{layer}:say"),
            &format!("{IK}{body}"),
        )
    };
    say(&bobby, "talk", "<urn:layer:new> a ik:Message ; ik:body \"Morning. The tuplespace arc is next; see https://github.com/ikigai-rs/ikigai-layers and ledger #1190.\" .");
    say(
        &brian,
        "talk",
        "<urn:layer:new> a ik:Question ; ik:body \"Does the mirror need a second ledger?\" .",
    );
    say(&bobby, "talk", "<urn:layer:new> a ik:Brief ; dcterms:title \"A layers page in gonk\" ; ik:body \"Read and post layer traffic where you already look.\\n\\n- entries newest first\\n- unread markers\" ; ik:repository <https://github.com/ikigai-rs/ikigai-gonk> .");
    say(&bobby, "talk", "<urn:layer:new> a ik:Issue ; dcterms:title \"The banner wraps badly at 320 px\" ; ik:body \"Seen on a phone.\" ; ik:severity <urn:iki:severity:minor> .");
    let notes = "# Notes\n\nPlain text, saved as notes.md.\n";
    say(&bobby, "talk", &format!(
        "<urn:layer:new> a ik:File ; ik:fileName \"notes.md\" ; ik:mediaType \"text/markdown\" ; ik:contentHash \"sha256:{}\" ; ik:byteSize {} ; ik:encoding \"text\" ; ik:content {} .",
        Sha256::digest(notes.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>(),
        notes.len(),
        serde_json::to_string(notes).unwrap()
    ));
    say(
        &bobby,
        "notes",
        "<urn:layer:new> a ik:Message ; ik:body \"Notes are read-only for brian.\" .",
    );
    call(&bobby, Verb::Sink, "urn:layer:tasks:tuples", &format!("{IK}<urn:layer:tasks:offer:1> a ik:Offer ; dcterms:title \"Wire the Layers page\" ; ik:brief \"The brief.\" ."));

    let view = LayersView::new(
        Arc::new(brian),
        "an in-process scratch layer host".to_string(),
        None,
        Waits::default(),
    );

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", port)))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let gonk_config = tempfile::tempdir().unwrap();
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let passkeys = Arc::new(Passkeys::new(
        quic::Layout::in_config_home(gonk_config.path()),
        addr.port(),
    ));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        toolbox: false,
    });
    let pages = layers_view::bind(web::space(Arc::clone(&face)), &face, Some(view));
    let http = Arc::new(doors::http_kernel(Arc::clone(&hub), pages));
    let mut anonymous =
        ikigai_gonk::grants::anonymous_http_grants(&["default".to_string()]).unwrap();
    // ⚠ The fixture: the real server mints these for NO anonymous caller.
    anonymous.extend([CAP_READ.to_string(), CAP_SAY.to_string()]);
    let door = doors::HttpDoor {
        anonymous,
        bind: addr,
        passkeys: Some(passkeys),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    };
    std::thread::spawn(move || {
        let mut n = 0;
        loop {
            std::thread::sleep(Duration::from_secs(30));
            n += 1;
            say(
                &bobby,
                "talk",
                &format!("<urn:layer:new> a ik:Message ; ik:body \"A live update, number {n}.\" ."),
            );
        }
    });
    println!("http://localhost:{}/layers", addr.port());
    runtime.block_on(async {
        let error = ikigai_web::serve_with_listener(
            http,
            doors::http_cap(door.clone()),
            listener,
            doors::edge_config(door),
        )
        .await;
        eprintln!("the preview door stopped: {error:?}");
    });
}
