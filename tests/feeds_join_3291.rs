//! Ledger [#3291](http://localhost:1062/l/default/item/3291): gonk joins a host's public feeds
//! from a clean data home, against a SCRATCH layer host (the published `ikigai-layers`, its join
//! lobby and its own descriptor) behind a real QUIC door in this process, and a scratch gonk HTTP
//! door. Only the HTTPS read of the descriptor is a test double ([`Published`]), since no scratch
//! host holds a certificate a public root signed; one test holds the real [`WebPki`] to that.
//! Nothing here touches a live service.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;
mod layers_key;

use common::Authenticator;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, Fallback, Iri, Kernel, Representation, Request, Space, SystemClock,
    Verb,
};
use ikigai_fn::html::url_segment as encode;
use ikigai_gonk::feeds_join::{Fetch, PublicFeeds, WebPki, CAP_JOIN, JOIN_PATH, LEAVE_PATH};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::layer::Door;
use ikigai_gonk::layers_view::{self, LayersView, Views, Waits, CAP_READ, CAP_SAY};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

const HOST: &str = "https://feeds.example";
const DESCRIPTOR_URL: &str = "https://feeds.example/.well-known/ikigai/layers.json";
const CERT_URL: &str = "https://feeds.example/.well-known/ikigai/server.crt";

/// The host's signing key, for these tests alone.
const HOST_KEY: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIOUDni4Jkl6XH4vehdjEkoWVAwUKgzn0zAn9f6YVf2TM
-----END PRIVATE KEY-----
";

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
const CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#", "dcterms": "http://purl.org/dc/terms/",
  "Message": "ik:Message", "Question": "ik:Question",
  "title": "dcterms:title", "body": "ik:body",
  "subject": { "@id": "dcterms:subject", "@type": "@id" }
} }"#;
const SCHEME: &str = r#"@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
<urn:t:scheme:s> a skos:ConceptScheme .
<urn:t:concept:release> a skos:Concept ; skos:prefLabel "Release" ; skos:notation "1" .
"#;
const JOIN_SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix onb: <https://ikigai-rs.dev/ns/onboard#> .
<urn:iki:onboard:shape:join> a sh:NodeShape ; sh:targetClass onb:JoinRequest ; sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ; sh:property
    [ sh:path onb:certificate ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
    [ sh:path onb:fingerprint ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
    [ sh:path onb:layer ; sh:minCount 1 ; sh:nodeKind sh:IRI ] ,
    [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
    [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
    [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .
"#;
const JOIN_CONTEXT: &str = r#"{ "@context": {
  "onb": "https://ikigai-rs.dev/ns/onboard#",
  "layer": { "@id": "onb:layer", "@type": "@id" }
} }"#;
const PARTICIPANTS: &str = r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
<urn:collab:participant:brian> a foaf:Person .
<urn:collab:participant:bobby> a foaf:Person .
"#;
const CREW: &str = r#"[[mount]]
layer = "talk"
brian = ["read", "feed", "say"]

[[mount]]
layer = "releases"
bobby = ["read", "feed", "say"]
brian = ["read", "feed"]

[[mount]]
layer = "features"
bobby = ["read", "feed", "say"]

[[mount]]
layer = "internal"
bobby = ["read", "feed", "say"]
"#;

fn feed_toml(label: &str, public: bool) -> String {
    format!(
        "model = \"channel\"\nlabel = \"{label}\"\nshapes = \"feed.shapes.ttl\"\n\
         context = \"c.context.jsonld\"\nfeed = true\npublic = {public}\n\
         publishers = [\"bobby\"]\nscheme = \"s.scheme.ttl\"\n"
    )
}

/// A real layer host with two public feeds and one private one, behind a real QUIC door.
struct Scratch {
    config: tempfile::TempDir,
    _state: tempfile::TempDir,
    _layers: Arc<ikigai_layers::Host>,
    kernel: Arc<Kernel>,
    address: String,
    door_dir: tempfile::TempDir,
}

impl Scratch {
    fn open() -> Scratch {
        let config = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = config.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "layers/talk.shapes.ttl",
            &format!(
                "{PREFIXES}<urn:t:shape:talk> a sh:NodeShape ; sh:targetClass ik:Message , \
                 ik:Question ; sh:closed true ; sh:ignoredProperties ( rdf:type ) ; sh:property \
                 [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
                 [ sh:path dcterms:subject ; sh:nodeKind sh:IRI ] , {STAMPS} .\n"
            ),
        );
        write(
            "layers/feed.shapes.ttl",
            &format!(
                "{PREFIXES}<urn:t:shape:announcement> a sh:NodeShape ; sh:targetClass ik:Message ; \
                 sh:closed true ; sh:ignoredProperties ( rdf:type ) ; sh:property \
                 [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
                 [ sh:path dcterms:subject ; sh:nodeKind sh:IRI ] , {STAMPS} .\n"
            ),
        );
        write("layers/c.context.jsonld", CONTEXT);
        write("layers/s.scheme.ttl", SCHEME);
        write(
            "layers/talk.toml",
            "model = \"channel\"\nlabel = \"Talk\"\nshapes = \"talk.shapes.ttl\"\ncontext = \"c.context.jsonld\"\n",
        );
        write("layers/releases.toml", &feed_toml("Releases", true));
        write("layers/features.toml", &feed_toml("Features", true));
        write("layers/internal.toml", &feed_toml("Internal", false));
        write("layers/join.shapes.ttl", JOIN_SHAPES);
        write("layers/join.context.jsonld", JOIN_CONTEXT);
        write(
            "layers/join.toml",
            "model = \"channel\"\nlabel = \"Join\"\nshapes = \"join.shapes.ttl\"\n\
             context = \"join.context.jsonld\"\nlobby = true\njoin = \"public\"\n",
        );
        write("join.pem", HOST_KEY);
        write("layers.toml", "[join]\nhost_key = \"join.pem\"\n");
        write("participants.ttl", PARTICIPANTS);
        write("collabs/crew.toml", CREW);
        let state = tempfile::tempdir().unwrap();
        let key = layers_key::state_key(config.path());
        let layers = ikigai_layers::Host::open(config.path(), state.path(), &key).expect("host");
        let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_layers::space(&layers))];
        let kernel = Arc::new(
            Kernel::with_meta_renderer(
                Arc::new(Fallback::new(spaces)),
                Arc::new(ikigai_vocab::TurtleRenderer),
            )
            .with_clock(Arc::new(SystemClock)),
        );

        let identity = ikigai_quic::generate();
        let door_dir = tempfile::tempdir().unwrap();
        std::fs::write(door_dir.path().join("server.crt"), &identity.cert_pem).unwrap();
        let socket: UdpSocket = ikigai_quic::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = format!("quic://{}", socket.local_addr().unwrap());
        let trust: ikigai_quic::TrustList = Arc::new(|| Ok(Vec::new()));
        let mint_for = Arc::clone(&layers);
        let lobby: ikigai_quic::Minter = Arc::new(move |peer: &ikigai_quic::PeerIdentity| {
            let capability = mint_for
                .lobby_capability("edge", &peer.fingerprint)
                .ok()??;
            let principal = capability.principal().map(str::to_string);
            Some(ikigai_quic::Session {
                capability,
                file_segment: peer.segment_id.clone(),
                principal,
            })
        });
        let refused: ikigai_quic::Minter = Arc::new(|_: &ikigai_quic::PeerIdentity| None);
        let serving = Arc::clone(&kernel);
        std::thread::spawn(move || {
            ikigai_quic::serve_lobby_on(
                serving,
                socket,
                &identity,
                trust,
                refused,
                lobby,
                ikigai_quic::DEFAULT_IDLE_TIMEOUT,
                None,
            )
        });
        Scratch {
            config,
            _state: state,
            _layers: layers,
            kernel,
            address,
            door_dir,
        }
    }

    /// The host's own descriptor and certificate, as a deployment publishes them.
    fn published(&self) -> Arc<Published> {
        let descriptor = ikigai_layers::onboard::join::descriptor(
            self.config.path(),
            &self.address,
            self.door_dir.path(),
        )
        .expect("a descriptor");
        let mut files = BTreeMap::new();
        files.insert(
            DESCRIPTOR_URL.to_string(),
            serde_json::to_vec(&descriptor.json).unwrap(),
        );
        files.insert(
            CERT_URL.to_string(),
            descriptor.server_cert.as_bytes().to_vec(),
        );
        Arc::new(Published(Mutex::new(files)))
    }

    fn as_participant(&self, slug: &str, scopes: &[&str]) -> Arc<dyn Door> {
        Arc::new(As {
            kernel: Arc::clone(&self.kernel),
            capability: Capability::scoped(scopes.iter().map(|s| s.to_string()))
                .with_principal(&format!("urn:collab:participant:{slug}"))
                .unwrap(),
        })
    }

    fn publish(&self, layer: &str, title: &str) -> String {
        let bobby = self.as_participant(
            "bobby",
            &[
                &format!("urn:cap:layer:say:{layer}"),
                &format!("urn:cap:layer:read:{layer}"),
            ],
        );
        let content = format!(
            "@prefix ik: <https://ikigai-rs.dev/ns#> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
             <urn:layer:new> a ik:Message ; dcterms:title \"{title}\" .\n"
        );
        let answer = bobby
            .call(
                Request::new(
                    Verb::Sink,
                    Iri::parse(format!("urn:layer:{layer}:say")).unwrap(),
                )
                .with_arg("content", ArgRef::Inline(content.into_bytes())),
            )
            .expect("a publisher says");
        String::from_utf8(answer.bytes).unwrap().trim().to_string()
    }

    fn entries(&self, layer: &str) -> String {
        let reader = self.as_participant(
            "brian",
            &[
                &format!("urn:cap:layer:read:{layer}"),
                &format!("urn:cap:layer:say:{layer}"),
            ],
        );
        let answer = reader
            .call(Request::new(
                Verb::Source,
                Iri::parse(format!("urn:layer:{layer}:entries")).unwrap(),
            ))
            .unwrap();
        String::from_utf8(answer.bytes).unwrap()
    }
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

/// What the host publishes over HTTPS, and what a compromised copy of it would serve instead.
struct Published(Mutex<BTreeMap<String, Vec<u8>>>);

impl Published {
    fn alter(&self, url: &str, change: impl FnOnce(&mut Vec<u8>)) {
        change(self.0.lock().unwrap().get_mut(url).expect("published"));
    }

    fn alter_descriptor(&self, change: impl FnOnce(&mut serde_json::Value)) {
        self.alter(DESCRIPTOR_URL, |bytes| {
            let mut value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            change(&mut value);
            *bytes = serde_json::to_vec(&value).unwrap();
        });
    }
}

impl Fetch for Published {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.0
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or_else(|| format!("{url} answered 404"))
    }
}

struct Response {
    status: u16,
    body: String,
}

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

struct Gonk {
    addr: SocketAddr,
    layout: quic::Layout,
    _config: tempfile::TempDir,
}

impl Gonk {
    fn start(views: Views) -> Gonk {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let passkeys = Arc::new(Passkeys::new(layout.clone(), addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            holder_aliases: Default::default(),
            page_assets: Default::default(),
            toolbox: false,
            faces: Vec::new(),
        });
        let pages = layers_view::bind(web::space(Arc::clone(&face)), &face, views);
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), pages));
        let door = doors::HttpDoor {
            anonymous: Vec::new(),
            bind: addr,
            passkeys: Some(passkeys),
            public: Vec::new(),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door.clone()),
                listener,
                doors::edge_config(door),
            ))
        });
        Gonk {
            addr,
            layout,
            _config: config,
        }
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    fn raw(&self, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> Response {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n",
            self.addr.port()
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ));
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let response = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        Response {
            status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            body: body.to_string(),
        }
    }

    fn page(&self, path: &str, session: &str) -> Response {
        let headers = [
            ("Accept", CHROME_ACCEPT.to_string()),
            ("Sec-Fetch-Site", "none".to_string()),
            ("Cookie", format!("{}={session}", identity::SESSION_COOKIE)),
        ];
        self.raw("GET", path, &headers, "")
    }

    fn post(&self, path: &str, fields: &[(&str, &str)], session: &str) -> Response {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let headers = [
            ("Accept", CHROME_ACCEPT.to_string()),
            ("Origin", self.origin()),
            ("Sec-Fetch-Site", "same-origin".to_string()),
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
            ("Cookie", format!("{}={session}", identity::SESSION_COOKIE)),
        ];
        self.raw("POST", path, &headers, &body)
    }

    /// Enroll a passkey under `grant`, sign in, and answer the session token.
    fn sign_in(&self, grant: &str, scopes: &[String]) -> String {
        let authenticator = Authenticator::named(grant);
        let invite = identity::invite(
            &self.layout,
            grant,
            scopes,
            false,
            30,
            identity::now_seconds(),
        )
        .unwrap();
        let json = |path: &str, body: &str| {
            let answer = self.raw(
                "POST",
                path,
                &[
                    ("Accept", "application/json".to_string()),
                    ("Content-Type", "application/json".to_string()),
                    ("Origin", self.origin()),
                    ("Sec-Fetch-Site", "same-origin".to_string()),
                ],
                body,
            );
            assert_eq!(answer.status, 200, "{}", answer.body);
            serde_json::from_str::<serde_json::Value>(&answer.body).unwrap()
        };
        let challenge = |op: &str| {
            json(&format!("/auth/{op}"), "{}")["challenge"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let c = challenge("register-options");
        json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        let c = challenge("login-options");
        json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        )["session"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

fn eventually(within: Duration, mut check: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    check()
}

fn all_tokens() -> Vec<String> {
    vec![
        CAP_READ.to_string(),
        CAP_SAY.to_string(),
        CAP_JOIN.to_string(),
    ]
}

/// A clean gonk: no `gonk.layers.view`, nothing in the data home.
struct Clean {
    scratch: Scratch,
    published: Arc<Published>,
    data: tempfile::TempDir,
    feeds: Arc<PublicFeeds>,
    gonk: Gonk,
}

fn clean() -> Clean {
    let scratch = Scratch::open();
    let published = scratch.published();
    let data = tempfile::tempdir().unwrap();
    let feeds = PublicFeeds::open(
        HOST.to_string(),
        data.path(),
        Arc::clone(&published) as Arc<dyn Fetch>,
        None,
        None,
        Waits { feed_secs: 1 },
    );
    let gonk = Gonk::start(Views::new(None, Some(Arc::clone(&feeds))));
    Clean {
        scratch,
        published,
        data,
        feeds,
        gonk,
    }
}

fn identity_file(data: &Path, name: &str) -> std::path::PathBuf {
    data.join("feeds").join("identity").join(name)
}

#[test]
fn a_clean_data_home_joins_and_lists_the_feeds() {
    let c = clean();
    c.scratch.publish("releases", "Version one");
    let session = c.gonk.sign_in("owner", &all_tokens());

    let before = c.gonk.page("/feed", &session);
    assert_eq!(before.status, 200, "{}", before.body);
    assert!(
        before.body.contains("Join the public feeds"),
        "{}",
        before.body
    );
    assert!(
        !before.body.contains("Leave the public feeds"),
        "{}",
        before.body
    );
    assert!(!identity_file(c.data.path(), "client.crt").exists());

    let joined = c.gonk.post(JOIN_PATH, &[], &session);
    assert_eq!(joined.status, 200, "{}", joined.body);
    assert!(
        joined.body.contains("Joined the public feeds"),
        "{}",
        joined.body
    );
    assert!(
        joined.body.contains("Leave the public feeds"),
        "{}",
        joined.body
    );

    for file in ["client.crt", "client.key", "server.crt", "joined.json"] {
        assert!(
            identity_file(c.data.path(), file).exists(),
            "no {file} after joining"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(identity_file(c.data.path(), "client.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the private key is the owner's alone");
    }

    let page = c.gonk.page("/feed", &session).body;
    assert!(page.contains("Releases"), "{page}");
    assert!(page.contains("Features"), "{page}");
    assert!(page.contains("Version one"), "{page}");
    assert!(!page.contains("Internal"), "a private feed: {page}");

    // Subscribing brings the feed into the Layers view, read by the joined identity.
    let subscribed = c.gonk.post(
        "/feed/subscribe",
        &[("feed", "releases"), ("action", "subscribe")],
        &session,
    );
    assert_eq!(subscribed.status, 200, "{}", subscribed.body);
    let layers = c.gonk.page("/layers", &session).body;
    assert!(layers.contains("Releases"), "{layers}");
    c.scratch.publish("releases", "Version two");
    assert!(
        eventually(Duration::from_secs(20), || {
            c.gonk
                .page("/layers/releases", &session)
                .body
                .contains("Version two")
        }),
        "a new entry reaches the joined reader"
    );
}

#[test]
fn a_clean_gonk_joins_with_the_same_identity_from_the_shell_path() {
    // The CLI is PublicFeeds::join with the same arguments; a running server notices the
    // directory change without a restart.
    let c = clean();
    let session = c.gonk.sign_in("owner", &all_tokens());
    let shell = PublicFeeds::open(
        HOST.to_string(),
        c.data.path(),
        Arc::clone(&c.published) as Arc<dyn Fetch>,
        None,
        None,
        Waits { feed_secs: 1 },
    );
    let joined = shell.join().expect("the join");
    assert_eq!(joined.host, HOST);
    assert_eq!(joined.layers, ["features", "releases"]);
    let page = c.gonk.page("/feed", &session).body;
    assert!(page.contains("Leave the public feeds"), "{page}");
    assert!(page.contains("Features"), "{page}");

    shell.leave().expect("leaving");
    let page = c.gonk.page("/feed", &session).body;
    assert!(page.contains("Join the public feeds"), "{page}");
}

#[test]
fn a_descriptor_whose_certificate_does_not_match_its_sha256_is_refused() {
    let c = clean();
    c.published.alter_descriptor(|d| {
        d["server_cert"]["sha256"] = serde_json::Value::String("ab".repeat(32));
    });
    let session = c.gonk.sign_in("owner", &all_tokens());

    let answer = c.gonk.post(JOIN_PATH, &[], &session);
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(answer.body.contains("role='alert'"), "{}", answer.body);
    assert!(answer.body.contains("sha256"), "{}", answer.body);
    assert!(answer.body.contains("Nothing was asked of the door"));
    assert!(
        !answer.body.contains("needs urn:cap:gonk:feeds:join"),
        "the owner holds the token: {}",
        answer.body
    );
    assert!(
        answer.body.contains("Join the public feeds"),
        "{}",
        answer.body
    );

    assert!(c.feeds.joined().is_none());
    assert!(!identity_file(c.data.path(), "joined.json").exists());
    assert!(
        !identity_file(c.data.path(), "client.crt").exists(),
        "no certificate is made for a door that did not prove itself"
    );
}

#[test]
fn a_join_refused_by_the_host_shows_its_reason() {
    let c = clean();
    // A descriptor listing a layer the host does not hold public: the host refuses the whole.
    c.published.alter_descriptor(|d| {
        d["public_layers"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({ "name": "internal", "type": "ik:Feed", "read_only": true }));
    });
    let session = c.gonk.sign_in("owner", &all_tokens());

    let answer = c.gonk.post(JOIN_PATH, &[], &session);
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(answer.body.contains("role='alert'"), "{}", answer.body);
    assert!(answer.body.contains("The host refused the join"));
    assert!(
        answer
            .body
            .contains("`internal` is not a public layer of this host"),
        "{}",
        answer.body
    );
    assert!(c.feeds.joined().is_none());
    assert!(!identity_file(c.data.path(), "joined.json").exists());
    // The certificate stays: a retry is the same certificate to the host's limits.
    assert!(identity_file(c.data.path(), "client.crt").exists());
    assert!(
        answer.body.contains("Join the public feeds"),
        "{}",
        answer.body
    );
}

#[test]
fn leaving_removes_the_identity() {
    let c = clean();
    let session = c.gonk.sign_in("owner", &all_tokens());
    assert_eq!(c.gonk.post(JOIN_PATH, &[], &session).status, 200);
    assert!(identity_file(c.data.path(), "client.key").exists());

    let left = c.gonk.post(LEAVE_PATH, &[], &session);
    assert_eq!(left.status, 200, "{}", left.body);
    assert!(
        left.body.contains("You left the public feeds"),
        "{}",
        left.body
    );
    assert!(left.body.contains("Join the public feeds"), "{}", left.body);
    assert!(!c.data.path().join("feeds").join("identity").exists());
    assert!(c.feeds.joined().is_none());

    let page = c.gonk.page("/feed", &session).body;
    assert!(!page.contains("/feed/subscribe"), "{page}");
    assert!(!page.contains("Features"), "{page}");
    let layers = c.gonk.page("/layers", &session).body;
    assert!(
        !layers.contains("Releases"),
        "nothing reads it now: {layers}"
    );

    // Joining again mints a fresh identity.
    assert_eq!(c.gonk.post(JOIN_PATH, &[], &session).status, 200);
    let page = c.gonk.page("/feed", &session).body;
    assert!(page.contains("Features"), "{page}");
}

#[test]
fn joining_needs_the_token_and_the_same_origin() {
    let c = clean();
    let session = c
        .gonk
        .sign_in("reader", &[CAP_READ.to_string(), CAP_SAY.to_string()]);

    let page = c.gonk.page("/feed", &session).body;
    assert!(!page.contains("Join the public feeds<"), "{page}");
    assert!(
        page.contains("needs urn:cap:gonk:feeds:join"),
        "the reader is told what it would take: {page}"
    );
    let refused = c.gonk.post(JOIN_PATH, &[], &session);
    assert_eq!(refused.status, 403, "{}", refused.body);
    assert!(c.feeds.joined().is_none());

    let owner = c.gonk.sign_in("owner", &all_tokens());
    let crossed = c.gonk.raw(
        "POST",
        JOIN_PATH,
        &[
            ("Accept", CHROME_ACCEPT.to_string()),
            ("Origin", "https://elsewhere.example".to_string()),
            ("Sec-Fetch-Site", "cross-site".to_string()),
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
            ("Cookie", format!("{}={owner}", identity::SESSION_COOKIE)),
        ],
        "",
    );
    assert_eq!(crossed.status, 403, "{}", crossed.body);
    assert!(c.feeds.joined().is_none());
}

#[test]
fn the_joined_identity_alone_never_says_anything() {
    let c = clean();
    let session = c.gonk.sign_in("owner", &all_tokens());
    assert_eq!(c.gonk.post(JOIN_PATH, &[], &session).status, 200);
    c.gonk.post(
        "/feed/subscribe",
        &[("feed", "releases"), ("action", "subscribe")],
        &session,
    );
    let entry = c.scratch.publish("releases", "A release");
    let page = c.gonk.page("/layers/releases", &session).body;
    assert!(
        !page.contains("Ask a question about this"),
        "no channel of its own to ask in: {page}"
    );
    let id = entry.rsplit(':').next().unwrap();
    let asked = c.gonk.post(
        "/feed/ask",
        &[
            ("feed", "releases"),
            ("entry", id),
            ("channel", "releases"),
            ("body", "Why?"),
        ],
        &session,
    );
    assert_eq!(asked.status, 403, "{}", asked.body);
    let said = c.gonk.post(
        "/layers/releases/say",
        &[("kind", "message"), ("body", "Hello")],
        &session,
    );
    assert_eq!(said.status, 403, "{}", said.body);
    assert!(
        !c.scratch.entries("releases").contains("Hello"),
        "nothing was posted"
    );
}

/// `brian` reads `releases` by grant and `features` is public, so the joined identity adds only
/// `features`, and a question about it goes through brian's own channel.
#[test]
fn with_a_view_identity_the_joined_one_only_adds_the_public_feeds_it_lacks() {
    let scratch = Scratch::open();
    let published = scratch.published();
    let data = tempfile::tempdir().unwrap();
    let view = LayersView::new(
        scratch.as_participant(
            "brian",
            &[
                "urn:cap:collab:read:crew",
                "urn:cap:layer:read:talk",
                "urn:cap:layer:feed:talk",
                "urn:cap:layer:say:talk",
                "urn:cap:layer:read:releases",
                "urn:cap:layer:feed:releases",
            ],
        ),
        "scratch-door".to_string(),
        None,
        Waits { feed_secs: 1 },
    );
    view.refresh().expect("the scratch door answers");
    let feeds = PublicFeeds::open(
        HOST.to_string(),
        data.path(),
        Arc::clone(&published) as Arc<dyn Fetch>,
        Some(Arc::clone(&view)),
        None,
        Waits { feed_secs: 1 },
    );
    let gonk = Gonk::start(Views::new(Some(Arc::clone(&view)), Some(feeds)));
    let session = gonk.sign_in("owner", &all_tokens());

    let page = gonk.page("/feed", &session).body;
    assert!(page.contains("Releases"), "{page}");
    assert!(
        !page.contains("Features"),
        "not joined, it holds no grant on features: {page}"
    );
    assert!(page.contains("Join the public feeds"), "{page}");

    assert_eq!(gonk.post(JOIN_PATH, &[], &session).status, 200);
    let page = gonk.page("/feed", &session).body;
    assert!(page.contains("Features"), "joined adds it: {page}");
    assert!(page.contains("Releases"), "{page}");
    // The view identity is still who it was.
    assert_eq!(
        view.snapshot().me.unwrap().principal,
        "urn:collab:participant:brian"
    );

    gonk.post(
        "/feed/subscribe",
        &[("feed", "features"), ("action", "subscribe")],
        &session,
    );
    let entry = scratch.publish("features", "A feature");
    let id = entry.rsplit(':').next().unwrap().to_string();
    assert!(
        eventually(Duration::from_secs(20), || {
            gonk.page("/layers/features", &session)
                .body
                .contains("A feature")
        }),
        "the joined identity's follower reads the added feed"
    );

    // Ask is offered on the joined feed, in the view identity's own channel.
    let page = gonk.page("/layers/features", &session).body;
    assert!(page.contains("Ask a question about this"), "{page}");
    let asked = gonk.post(
        "/feed/ask",
        &[
            ("feed", "features"),
            ("entry", &id),
            ("channel", "talk"),
            ("body", "How does it work?"),
        ],
        &session,
    );
    assert_eq!(asked.status, 200, "{}", asked.body);
    let talk = scratch.entries("talk");
    assert!(talk.contains("How does it work?"), "{talk}");
    assert!(
        talk.contains("urn:collab:participant:brian"),
        "the question is the view identity's: {talk}"
    );

    // Leaving takes the joined feed away and leaves the identity's own alone.
    assert_eq!(gonk.post(LEAVE_PATH, &[], &session).status, 200);
    let page = gonk.page("/feed", &session).body;
    assert!(!page.contains("Features"), "{page}");
    assert!(page.contains("Releases"), "{page}");
}

#[test]
fn a_host_the_web_pki_does_not_vouch_for_is_refused() {
    // A TLS server with a certificate no public root signed: rustls answers the handshake, and
    // the real fetcher must not get as far as sending a request.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let identity = ikigai_quic::generate();
    let tls = Arc::new(ikigai_quic::https_server_tls(&identity).unwrap());
    let served = Arc::new(Mutex::new(false));
    let saw = Arc::clone(&served);
    std::thread::spawn(move || {
        let Ok((mut tcp, _)) = listener.accept() else {
            return;
        };
        let mut server = rustls::ServerConnection::new(tls).unwrap();
        let mut stream = rustls::Stream::new(&mut server, &mut tcp);
        let mut buffer = [0u8; 16];
        if matches!(stream.read(&mut buffer), Ok(n) if n > 0) {
            *saw.lock().unwrap() = true;
        }
    });
    let refused = WebPki
        .get(&format!("https://localhost:{}/descriptor", addr.port()))
        .expect_err("a certificate no public root signed");
    assert!(!refused.is_empty());
    assert!(
        !*served.lock().unwrap(),
        "nothing was sent over the untrusted connection"
    );
}

#[test]
fn a_clean_gonk_needs_no_config_line_for_the_default_door() {
    assert_eq!(
        ikigai_gonk::feeds_join::DEFAULT_DOOR,
        "https://ikigai-rs.dev"
    );
    let data = tempfile::tempdir().unwrap();
    let feeds = PublicFeeds::open(
        ikigai_gonk::feeds_join::DEFAULT_DOOR.to_string(),
        data.path(),
        Arc::new(WebPki),
        None,
        None,
        Waits::default(),
    );
    assert!(
        feeds.joined().is_none(),
        "nothing is fetched or dialed until the owner joins"
    );
    assert!(!data.path().join("feeds").exists());
}
