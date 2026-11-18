//! A scratch layer host holding owner `brian`'s inquiry layers (ledger #3695), opened on the
//! layer files `ikigai-inquiry` 0.1.0 ships (`layers/`, as `ikigai_inquiry::vocab` carries them)
//! the way that crate's own tests open them, and a door onto it that mints a participant, as
//! the door's enrollment would, and counts what reaches it.

#![allow(dead_code)]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, Fallback, Iri, Kernel, Representation, Request, Space, SystemClock,
    Verb,
};
use ikigai_gonk::layer::Door;
use ikigai_inquiry::vocab;
use serde_json::{json, Value};

#[path = "../layers_key/mod.rs"]
mod layers_key;

pub const OWNER: &str = "brian";
pub const INQUIRY: &str = "inquiry-brian";
pub const SPAM: &str = "inquiry-spam-brian";
pub const HOLD: &str = "inquiry-hold-brian";
pub const EXAMPLE: &str = "inquiry-example-brian";
pub const INTENT: &str = "inquiry-intent-brian";

/// Who holds what at the host: Brian (a person) uses every view and action, the edge drops
/// arrivals, the auditor reads everything for the tests' own checks, and `plasma-gonk` is a
/// machine holding Brian's verbs.
fn verbs(slug: &str, layer: &str) -> &'static [&'static str] {
    match (slug, layer) {
        ("brian" | "plasma-gonk", INQUIRY | SPAM) => &["read", "out", "take"],
        ("brian" | "plasma-gonk", HOLD) => &["read"],
        ("brian" | "plasma-gonk", EXAMPLE) => &["say"],
        ("brian" | "plasma-gonk", INTENT) => &["out"],
        ("edge", INQUIRY | SPAM | HOLD) => &["out"],
        ("auditor", _) => &["read"],
        _ => &[],
    }
}

const LAYERS: [&str; 5] = [INQUIRY, SPAM, HOLD, EXAMPLE, INTENT];
const SLUGS: [&str; 4] = ["brian", "plasma-gonk", "edge", "auditor"];

pub fn participant(slug: &str) -> String {
    format!("urn:collab:participant:{slug}")
}

/// What the door's enrollment grants `slug`.
fn grant(slug: &str) -> Capability {
    let mut scopes = vec!["urn:cap:collab:read:inquiry".to_string()];
    for layer in LAYERS {
        scopes.extend(
            verbs(slug, layer)
                .iter()
                .map(|v| format!("urn:cap:layer:{v}:{layer}")),
        );
    }
    Capability::scoped(scopes)
        .with_principal(&participant(slug))
        .unwrap()
}

/// A participant at the door: mints its principal and grant, and counts every request.
pub struct As {
    kernel: Arc<Kernel>,
    capability: Capability,
    seen: Arc<AtomicUsize>,
    targets: Arc<Mutex<Vec<String>>>,
}

impl Door for As {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        self.seen.fetch_add(1, Ordering::SeqCst);
        self.targets.lock().unwrap().push(format!(
            "{:?} {}",
            request.verb,
            request.target.as_str()
        ));
        block_on(Kernel::issue(&self.kernel, request, &self.capability))
    }
}

/// The scratch host.
pub struct Host {
    _config: tempfile::TempDir,
    _state: tempfile::TempDir,
    _layers: Arc<ikigai_layers::Host>,
    kernel: Arc<Kernel>,
    seen: Arc<AtomicUsize>,
    targets: Arc<Mutex<Vec<String>>>,
}

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

impl Host {
    pub fn open() -> Host {
        let config = tempfile::tempdir().unwrap();
        let dir = config.path();
        write(dir, "layers/inquiry.context.jsonld", vocab::CONTEXT);
        write(dir, "layers/inquiry.shapes.ttl", vocab::INQUIRY_SHAPES);
        write(dir, "layers/example.shapes.ttl", vocab::EXAMPLE_SHAPES);
        write(dir, "layers/intent.shapes.ttl", vocab::INTENT_SHAPES);
        for layer in [INQUIRY, SPAM, HOLD] {
            write(
                dir,
                &format!("layers/{layer}.toml"),
                "model = \"tuplespace\"\nshapes = \"inquiry.shapes.ttl\"\n\
                 context = \"inquiry.context.jsonld\"\n",
            );
        }
        write(
            dir,
            &format!("layers/{INTENT}.toml"),
            "model = \"tuplespace\"\nshapes = \"intent.shapes.ttl\"\n\
             context = \"inquiry.context.jsonld\"\n",
        );
        write(
            dir,
            &format!("layers/{EXAMPLE}.toml"),
            "model = \"channel\"\nshapes = \"example.shapes.ttl\"\n\
             context = \"inquiry.context.jsonld\"\n",
        );
        write(
            dir,
            "participants.ttl",
            "@prefix foaf: <http://xmlns.com/foaf/0.1/> .\n\
             @prefix prov: <http://www.w3.org/ns/prov#> .\n\
             @prefix ik: <https://ikigai-rs.dev/ns#> .\n\
             <urn:collab:participant:brian> a foaf:Person .\n\
             <urn:collab:participant:auditor> a foaf:Person .\n\
             <urn:collab:participant:plasma-gonk> a ik:Machine ; \
             prov:actedOnBehalfOf <urn:collab:participant:brian> .\n\
             <urn:collab:participant:edge> a prov:SoftwareAgent ; \
             prov:actedOnBehalfOf <urn:collab:participant:brian> .\n",
        );
        let mut manifest = String::new();
        for layer in LAYERS {
            manifest.push_str(&format!("[[mount]]\nlayer = \"{layer}\"\n"));
            for slug in SLUGS {
                let held = verbs(slug, layer);
                if !held.is_empty() {
                    let held: Vec<String> = held.iter().map(|v| format!("\"{v}\"")).collect();
                    manifest.push_str(&format!("{slug} = [{}]\n", held.join(", ")));
                }
            }
            manifest.push('\n');
        }
        write(dir, "collabs/inquiry.toml", &manifest);
        let state = tempfile::tempdir().unwrap();
        let key = layers_key::state_key(dir);
        let layers = ikigai_layers::Host::open(dir, state.path(), &key).expect("the host opens");
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
            seen: Arc::new(AtomicUsize::new(0)),
            targets: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The door as `slug`, counted in [`Host::seen`].
    pub fn door(&self, slug: &str) -> Arc<dyn Door> {
        Arc::new(As {
            kernel: Arc::clone(&self.kernel),
            capability: grant(slug),
            seen: Arc::clone(&self.seen),
            targets: Arc::clone(&self.targets),
        })
    }

    /// The door as `slug`, NOT counted: the tests' own seeding and checks.
    fn quiet(&self, slug: &str) -> As {
        As {
            kernel: Arc::clone(&self.kernel),
            capability: grant(slug),
            seen: Arc::new(AtomicUsize::new(0)),
            targets: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// How many requests reached the host through [`Host::door`].
    pub fn seen(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }

    /// What reached it, `{verb} {target}` each.
    pub fn targets(&self) -> Vec<String> {
        self.targets.lock().unwrap().clone()
    }

    /// What the edge would drop: one inquiry `id` from `site`, into `layer`.
    pub fn arrive(&self, layer: &str, id: &str, site: &str, text: &str, received: &str) {
        let inq = vocab::INQ;
        let node = json!({
            "@id": format!("urn:layer:{layer}:inquiry:id:{id}"),
            "@type": format!("{inq}Inquiry"),
            format!("{inq}inquiry"): id,
            format!("{inq}site"): site,
            format!("{inq}text"): text,
            format!("{inq}replyTo"): "ada@example.org",
            format!("{inq}sender"): format!("hash-of-{id}"),
            format!("{inq}reason"): "score",
            format!("{inq}received"): {
                "@value": received,
                "@type": "http://www.w3.org/2001/XMLSchema#dateTime"
            },
        });
        let request = Request::new(
            Verb::Sink,
            Iri::parse(format!("urn:layer:{layer}:tuples")).unwrap(),
        )
        .with_arg("content", ArgRef::Inline(node.to_string().into_bytes()));
        self.quiet("edge")
            .call(request)
            .expect("the shipped shapes admit what the edge drops");
    }

    /// The pending tuples of `layer`, as the auditor reads them (JSON-LD nodes).
    pub fn pending(&self, layer: &str) -> Vec<Value> {
        let request = Request::new(
            Verb::Source,
            Iri::parse(format!("urn:layer:{layer}:tuples")).unwrap(),
        )
        .with_arg("as", ArgRef::Inline(b"application/ld+json".to_vec()));
        let answer = self.quiet("auditor").call(request).unwrap();
        let doc: Value = serde_json::from_slice(&answer.bytes).unwrap();
        doc["@graph"].as_array().cloned().unwrap_or_default()
    }

    /// The inquiry ids pending in `layer`.
    pub fn ids(&self, layer: &str) -> Vec<String> {
        self.pending(layer)
            .iter()
            .filter_map(|n| n["inquiry"].as_str().map(str::to_string))
            .collect()
    }
}
