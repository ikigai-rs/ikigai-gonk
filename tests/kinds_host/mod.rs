//! A scratch layer host whose layers carry kinds (ledger #3883), for the Layers page's tabs: the
//! published `ikigai-layers` in process, its layer files declaring `kind = "<key>"` as a real
//! host's do, behind a door that mints each participant's principal and holds the grant its
//! enrollment would give (the shape of `tests/layers_page_1675.rs`'s host).
//!
//! - `talk` and `notes`: kind `talk`, channels, Brian and Bobby read and say.
//! - `tasks`: kind `task`, a tuplespace; Brian reads, Bobby offers.
//! - `joins`: kind `join`, a channel only Bobby reads: Brian has no Join tab.
//! - `misc`: a channel whose definition names no kind: under "Other".

#![allow(dead_code)]

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, Fallback, Iri, Kernel, Representation, Request, Space, SystemClock,
    Verb,
};
use ikigai_gonk::layer::Door;

/// The host's state key: a fresh owner-only age identity under `dir`, as
/// `tests/layers_key/mod.rs` mints one. Written out here rather than included, because
/// `examples/axe_site.rs` includes that module already through `tests/inquiry_host`.
fn state_key(dir: &std::path::Path) -> ikigai_layers::StateKey {
    use age::secrecy::ExposeSecret;
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .expect("secure the test state key directory");
    let identity = age::x25519::Identity::generate();
    let path = dir.join("state.key");
    let mut key = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .expect("create the test state key");
    writeln!(
        key,
        "# created by a test\n{}",
        identity.to_string().expose_secret()
    )
    .expect("write the test state key");
    ikigai_layers::StateKey::load(&path).expect("load the test state key")
}

const PREFIXES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
"#;

const STAMPS: &str =
    "[ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] , \
     [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] , \
     [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ]";

fn channel_shapes() -> String {
    format!(
        "{PREFIXES}<urn:t:shape:Message> a sh:NodeShape ; sh:targetClass ik:Message ; sh:closed true ; \
         sh:ignoredProperties ( rdf:type ) ; sh:property \
         [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , {STAMPS} .\n"
    )
}

const CHANNEL_CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#", "dcterms": "http://purl.org/dc/terms/",
  "prov": "http://www.w3.org/ns/prov#", "xsd": "http://www.w3.org/2001/XMLSchema#",
  "Message": "ik:Message", "body": "ik:body",
  "attributedTo": { "@id": "prov:wasAttributedTo", "@type": "@id" },
  "created": { "@id": "dcterms:created", "@type": "xsd:dateTime" },
  "seq": { "@id": "ik:seq", "@type": "xsd:integer" }
} }"#;

fn task_shapes() -> String {
    format!(
        "{PREFIXES}<urn:t:shape:offer> a sh:NodeShape ; sh:targetClass ik:Offer ; sh:closed true ; \
         sh:ignoredProperties ( rdf:type ) ; sh:property \
         [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
         [ sh:path ik:brief ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , {STAMPS} .\n"
    )
}

const TASK_CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#", "dcterms": "http://purl.org/dc/terms/",
  "prov": "http://www.w3.org/ns/prov#", "xsd": "http://www.w3.org/2001/XMLSchema#",
  "Offer": "ik:Offer", "title": "dcterms:title", "brief": "ik:brief",
  "attributedTo": { "@id": "prov:wasAttributedTo", "@type": "@id" },
  "created": { "@id": "dcterms:created", "@type": "xsd:dateTime" }
} }"#;

const PARTICIPANTS: &str = r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
<urn:collab:participant:brian> a foaf:Person .
<urn:collab:participant:bobby> a foaf:Person .
"#;

type Holders = &'static [(&'static str, &'static [&'static str])];

/// Each layer: its name, its definition's `kind` (`None`: it names none), its model and holders.
pub const LAYERS: &[(&str, Option<&str>, &str, Holders)] = &[
    (
        "talk",
        Some("talk"),
        "channel",
        &[
            ("brian", &["read", "feed", "say"]),
            ("bobby", &["read", "feed", "say"]),
        ],
    ),
    (
        "notes",
        Some("talk"),
        "channel",
        &[
            ("brian", &["read", "feed", "say"]),
            ("bobby", &["read", "feed", "say"]),
        ],
    ),
    (
        "tasks",
        Some("task"),
        "tuplespace",
        &[
            ("brian", &["read", "feed"]),
            ("bobby", &["read", "feed", "out", "take"]),
        ],
    ),
    (
        "joins",
        Some("join"),
        "channel",
        &[("bobby", &["read", "feed", "say"])],
    ),
    (
        "misc",
        None,
        "channel",
        &[
            ("brian", &["read", "feed", "say"]),
            ("bobby", &["read", "feed", "say"]),
        ],
    ),
];

pub fn participant(slug: &str) -> String {
    format!("urn:collab:participant:{slug}")
}

fn grant(slug: &str) -> Capability {
    let mut scopes = vec!["urn:cap:collab:read:crew".to_string()];
    for (layer, _, _, holders) in LAYERS {
        for (who, verbs) in *holders {
            if *who == slug {
                scopes.extend(verbs.iter().map(|v| format!("urn:cap:layer:{v}:{layer}")));
            }
        }
    }
    Capability::scoped(scopes)
        .with_principal(&participant(slug))
        .unwrap()
}

struct As {
    kernel: Arc<Kernel>,
    capability: Capability,
}

impl Door for As {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        block_on(Kernel::issue(&self.kernel, request, &self.capability))
    }
}

/// The scratch layer host.
pub struct Host {
    _config: tempfile::TempDir,
    _state: tempfile::TempDir,
    _layers: Arc<ikigai_layers::Host>,
    kernel: Arc<Kernel>,
}

impl Host {
    pub fn open() -> Host {
        let config = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = config.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        let mut crew = String::new();
        for (layer, kind, model, holders) in LAYERS {
            let (shapes, context) = match *model {
                "tuplespace" => (task_shapes(), TASK_CONTEXT),
                _ => (channel_shapes(), CHANNEL_CONTEXT),
            };
            write(&format!("layers/{layer}.shapes.ttl"), &shapes);
            write(&format!("layers/{layer}.context.jsonld"), context);
            let mut toml = format!(
                "model = \"{model}\"\nlabel = \"{}\"\nshapes = \"{layer}.shapes.ttl\"\ncontext = \"{layer}.context.jsonld\"\n",
                capitalized(layer)
            );
            if let Some(kind) = kind {
                toml.push_str(&format!("kind = \"{kind}\"\n"));
            }
            write(&format!("layers/{layer}.toml"), &toml);
            crew.push_str(&format!("[[mount]]\nlayer = \"{layer}\"\n"));
            for (who, verbs) in *holders {
                let verbs: Vec<String> = verbs.iter().map(|v| format!("\"{v}\"")).collect();
                crew.push_str(&format!("{who} = [{}]\n", verbs.join(", ")));
            }
            crew.push('\n');
        }
        write("participants.ttl", PARTICIPANTS);
        write("collabs/crew.toml", &crew);
        let state = tempfile::tempdir().unwrap();
        let key = state_key(config.path());
        let layers =
            ikigai_layers::Host::open(config.path(), state.path(), &key).expect("the host");
        let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_layers::space(&layers))];
        let kernel = Arc::new(
            Kernel::with_meta_renderer(
                Arc::new(Fallback::new(spaces)),
                Arc::new(ikigai_vocab::TurtleRenderer),
            )
            .with_clock(Arc::new(SystemClock)),
        );
        Host {
            _config: config,
            _state: state,
            _layers: layers,
            kernel,
        }
    }

    pub fn door(&self, slug: &str) -> Arc<dyn Door> {
        Arc::new(As {
            kernel: Arc::clone(&self.kernel),
            capability: grant(slug),
        })
    }

    fn say(&self, slug: &str, layer: &str, verb: &str, turtle: &str) {
        let request = Request::new(
            Verb::Sink,
            Iri::parse(format!("urn:layer:{layer}:{verb}")).unwrap(),
        )
        .with_arg("content", ArgRef::Inline(turtle.as_bytes().to_vec()));
        self.door(slug).call(request).expect("the host admits it");
    }

    /// A message on a channel, as `slug`.
    pub fn message(&self, slug: &str, layer: &str, body: &str) {
        self.say(
            slug,
            layer,
            "say",
            &format!(
                "@prefix ik: <https://ikigai-rs.dev/ns#> .\n<urn:layer:new> a ik:Message ; ik:body {} .",
                serde_json::to_string(body).unwrap()
            ),
        );
    }

    /// Offer `n` on a tuplespace, as `slug`.
    pub fn offer(&self, slug: &str, layer: &str, n: u32, title: &str) {
        self.say(
            slug,
            layer,
            "tuples",
            &format!(
                "@prefix ik: <https://ikigai-rs.dev/ns#> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
                 <urn:layer:{layer}:offer:{n}> a ik:Offer ; dcterms:title {} ; ik:brief \"Do it.\" .",
                serde_json::to_string(title).unwrap()
            ),
        );
    }
}

fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
