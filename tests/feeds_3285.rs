//! Ledger [#3285](http://localhost:1062/l/default/item/3285): the Feeds page, its per-viewer
//! subscriptions and concept filters, and a subscribed feed in the Layers view, against a SCRATCH
//! layer host (the published `ikigai-layers` with a `feed = true` layer, in process) and a scratch
//! gonk HTTP door. Nothing here touches a live service.
//!
//! The reader `brian` is the case that matters: a feed's manifest names only its publishers
//! (ledger #3284), so the page finds that identity's feeds by asking, not from the manifest.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
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
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::layer::Door;
use ikigai_gonk::layers_view::{self, LayersView, Waits, CAP_READ, CAP_SAY};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

const RELEASE: &str = "urn:t:concept:release";
const PATCH: &str = "urn:t:concept:patch";
const FEATURE: &str = "urn:t:concept:feature";

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

/// A talk channel's shapes: a message or a question, which may be about a feed entry.
fn channel_shapes() -> String {
    format!(
        "{PREFIXES}<urn:t:shape:talk> a sh:NodeShape ; sh:targetClass ik:Message , ik:Question ; \
         sh:closed true ; sh:ignoredProperties ( rdf:type ) ; sh:property \
         [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
         [ sh:path dcterms:subject ; sh:nodeKind sh:IRI ] , {STAMPS} .\n"
    )
}

fn feed_shapes() -> String {
    format!(
        "{PREFIXES}<urn:t:shape:announcement> a sh:NodeShape ; sh:targetClass ik:Message ; \
         sh:closed true ; sh:ignoredProperties ( rdf:type ) ; sh:property \
         [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
         [ sh:path ik:body ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
         [ sh:path dcterms:subject ; sh:nodeKind sh:IRI ] , {STAMPS} .\n"
    )
}

const CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#", "dcterms": "http://purl.org/dc/terms/",
  "Message": "ik:Message", "Question": "ik:Question",
  "title": "dcterms:title", "body": "ik:body",
  "subject": { "@id": "dcterms:subject", "@type": "@id" }
} }"#;

const SCHEME: &str = r#"@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:t:scheme:releases> a skos:ConceptScheme ; rdfs:label "Release types" .
<urn:t:concept:release> a skos:Concept ; skos:prefLabel "Release" ; skos:notation "1" .
<urn:t:concept:patch> a skos:Concept ; skos:prefLabel "Patch" ; skos:notation "1.1" ;
  skos:broader <urn:t:concept:release> .
<urn:t:concept:feature> a skos:Concept ; skos:prefLabel "Feature" ; skos:notation "2" .
"#;

const PARTICIPANTS: &str = r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
<urn:collab:participant:brian> a foaf:Person .
<urn:collab:participant:bobby> a foaf:Person .
<urn:collab:participant:chris> a foaf:Person .
<urn:collab:participant:dana> a foaf:Person .
"#;

const CREW: &str = r#"[[mount]]
layer = "talk"
brian = ["read", "feed", "say"]
bobby = ["read", "feed", "say"]
chris = ["read", "feed"]
dana = ["read", "feed", "say"]

[[mount]]
layer = "ops"
bobby = ["read", "feed", "say"]
dana = ["read", "feed", "say"]

[[mount]]
layer = "releases"
bobby = ["read", "feed", "say"]
brian = ["read", "feed"]
chris = ["read", "feed"]
dana = ["read", "feed"]
"#;

fn scopes(slug: &str) -> Vec<String> {
    let mut held = vec!["urn:cap:collab:read:crew".to_string()];
    let talk: &[&str] = if slug == "chris" {
        &["read", "feed"]
    } else {
        &["read", "feed", "say"]
    };
    let releases: &[&str] = if slug == "bobby" {
        &["read", "feed", "say"]
    } else {
        &["read", "feed"]
    };
    held.extend(talk.iter().map(|v| format!("urn:cap:layer:{v}:talk")));
    if slug == "bobby" || slug == "dana" {
        held.extend(
            ["read", "feed", "say"]
                .iter()
                .map(|v| format!("urn:cap:layer:{v}:ops")),
        );
    }
    held.extend(
        releases
            .iter()
            .map(|v| format!("urn:cap:layer:{v}:releases")),
    );
    held
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

/// A door that keeps the cursors the follower asked its long poll to wait on.
struct Recording {
    inner: Arc<dyn Door>,
    waits: Mutex<Vec<String>>,
}

impl Door for Recording {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        if request.target.as_str() == "urn:layers:feed" {
            if let Some(ArgRef::Inline(after)) = request.args.get("after") {
                self.waits
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(after).into_owned());
            }
        }
        self.inner.call(request)
    }
}

struct Host {
    _config: tempfile::TempDir,
    _state: tempfile::TempDir,
    _layers: Arc<ikigai_layers::Host>,
    kernel: Arc<Kernel>,
}

impl Host {
    fn open() -> Host {
        let config = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = config.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("layers/talk.shapes.ttl", &channel_shapes());
        write("layers/feed.shapes.ttl", &feed_shapes());
        write("layers/c.context.jsonld", CONTEXT);
        write("layers/releases.scheme.ttl", SCHEME);
        write(
            "layers/talk.toml",
            "model = \"channel\"\nlabel = \"Talk\"\nshapes = \"talk.shapes.ttl\"\ncontext = \"c.context.jsonld\"\n",
        );
        write(
            "layers/ops.toml",
            "model = \"channel\"\nlabel = \"Ops\"\nshapes = \"talk.shapes.ttl\"\ncontext = \"c.context.jsonld\"\n",
        );
        write(
            "layers/releases.toml",
            "model = \"channel\"\nlabel = \"Releases\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"c.context.jsonld\"\nfeed = true\npublic = true\npublishers = [\"bobby\"]\nscheme = \"releases.scheme.ttl\"\n",
        );
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
        Host {
            _config: config,
            _state: state,
            _layers: layers,
            kernel,
        }
    }

    fn door(&self, slug: &str) -> Arc<dyn Door> {
        Arc::new(As {
            kernel: Arc::clone(&self.kernel),
            capability: Capability::scoped(scopes(slug))
                .with_principal(&format!("urn:collab:participant:{slug}"))
                .unwrap(),
        })
    }

    /// Publish one entry on `releases`, as its publisher, under `concepts`; its IRI.
    fn publish(&self, title: &str, concepts: &[&str]) -> String {
        let subjects: String = concepts
            .iter()
            .map(|c| format!(" dcterms:subject <{c}> ;"))
            .collect();
        let content = format!(
            "@prefix ik: <https://ikigai-rs.dev/ns#> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
             <urn:layer:new> a ik:Message ;{subjects} dcterms:title \"{title}\" ; ik:body \"About {title}.\" .\n"
        );
        let request = Request::new(Verb::Sink, Iri::parse("urn:layer:releases:say").unwrap())
            .with_arg("content", ArgRef::Inline(content.into_bytes()));
        let answer = self.door("bobby").call(request).expect("a publisher says");
        String::from_utf8(answer.bytes).unwrap().trim().to_string()
    }

    fn entries(&self, layer: &str) -> String {
        let answer = self
            .door("bobby")
            .call(Request::new(
                Verb::Source,
                Iri::parse(format!("urn:layer:{layer}:entries")).unwrap(),
            ))
            .unwrap();
        String::from_utf8(answer.bytes).unwrap()
    }
}

fn view_as(host: &Host, slug: &str, cursors: Option<std::path::PathBuf>) -> Arc<LayersView> {
    let view = LayersView::new(
        host.door(slug),
        "scratch-door".to_string(),
        cursors,
        Waits { feed_secs: 1 },
    );
    view.refresh().expect("the scratch door answers");
    view
}

struct Response {
    status: u16,
    body: String,
}

struct Gonk {
    addr: SocketAddr,
    layout: quic::Layout,
    _config: tempfile::TempDir,
}

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

impl Gonk {
    fn start(view: Option<Arc<LayersView>>, anonymous: Vec<String>) -> Gonk {
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
            toolbox: false,
            faces: Vec::new(),
        });
        let pages = layers_view::bind(web::space(Arc::clone(&face)), &face, view);
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), pages));
        let door = doors::HttpDoor {
            anonymous,
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

    fn page_with(&self, path: &str, session: Option<&str>) -> Response {
        let mut headers = vec![
            ("Accept", CHROME_ACCEPT.to_string()),
            ("Sec-Fetch-Site", "none".to_string()),
        ];
        if let Some(session) = session {
            headers.push(("Cookie", format!("{}={session}", identity::SESSION_COOKIE)));
        }
        self.raw("GET", path, &headers, "")
    }

    fn page(&self, path: &str) -> Response {
        self.page_with(path, None)
    }

    fn post_with(&self, path: &str, fields: &[(&str, &str)], session: Option<&str>) -> Response {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let mut headers = vec![
            ("Accept", CHROME_ACCEPT.to_string()),
            ("Origin", self.origin()),
            ("Sec-Fetch-Site", "same-origin".to_string()),
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
        ];
        if let Some(session) = session {
            headers.push(("Cookie", format!("{}={session}", identity::SESSION_COOKIE)));
        }
        self.raw("POST", path, &headers, &body)
    }

    fn post(&self, path: &str, fields: &[(&str, &str)]) -> Response {
        self.post_with(path, fields, None)
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

fn tokens() -> Vec<String> {
    vec![CAP_READ.to_string(), CAP_SAY.to_string()]
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

fn subscribe(gonk: &Gonk) {
    let answer = gonk.post(
        "/feed/subscribe",
        &[("feed", "releases"), ("action", "subscribe")],
    );
    assert_eq!(answer.status, 200, "{}", answer.body);
}

fn entry_id(iri: &str) -> &str {
    iri.rsplit(':').next().unwrap()
}

#[test]
fn the_header_link_is_offered_only_with_the_read_grant() {
    let host = Host::open();
    let reader = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    let page = reader.page("/layers");
    assert!(page.body.contains("href='/feed'"), "{}", page.body);
    assert!(page.body.contains(">Feeds<"), "{}", page.body);
    let own = reader.page("/feed").body;
    assert!(own.contains("aria-current='page'"), "{own}");

    let stranger = Gonk::start(Some(view_as(&host, "brian", None)), Vec::new());
    let page = stranger.page("/l/default");
    assert!(!page.body.contains("href='/feed'"), "{}", page.body);
}

#[test]
fn feed_lists_only_feed_layers_with_the_newest_entry() {
    let host = Host::open();
    host.publish("Version 1.2", &[RELEASE]);
    host.publish("Version 1.3", &[RELEASE]);
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    let page = gonk.page("/feed").body;
    assert!(page.contains("Releases"), "{page}");
    assert!(page.contains("(releases)"), "{page}");
    assert!(!page.contains("(talk)"), "a channel is not a feed: {page}");
    assert!(page.contains("Version 1.3"), "the newest entry: {page}");
    assert!(!page.contains("Version 1.2"), "only the newest: {page}");
    assert!(page.contains("<time"), "{page}");
    assert!(
        page.contains("Subscribe"),
        "a feed starts unsubscribed: {page}"
    );
    assert!(!page.contains("Unsubscribe"), "{page}");
}

#[test]
fn subscribe_and_unsubscribe_round_trip_per_viewer() {
    let host = Host::open();
    let dir = tempfile::tempdir().unwrap();
    let cursors = dir.path().join("layers-view").join("cursors.json");
    let view = view_as(&host, "brian", Some(cursors));
    let gonk = Gonk::start(Some(Arc::clone(&view)), Vec::new());
    let a = gonk.sign_in("a", &[CAP_READ.to_string()]);
    let b = gonk.sign_in("b", &[CAP_READ.to_string()]);

    let page = gonk.page_with("/feed", Some(&a)).body;
    assert!(page.contains(">Subscribe"), "{page}");

    let answer = gonk.post_with(
        "/feed/subscribe",
        &[("feed", "releases"), ("action", "subscribe")],
        Some(&a),
    );
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(
        answer.body.contains("Subscribed to Releases"),
        "{}",
        answer.body
    );

    let page_a = gonk.page_with("/feed", Some(&a)).body;
    assert!(page_a.contains("Unsubscribe"), "{page_a}");
    let page_b = gonk.page_with("/feed", Some(&b)).body;
    assert!(
        page_b.contains(">Subscribe") && !page_b.contains("Unsubscribe"),
        "another viewer is unaffected: {page_b}"
    );
    assert!(
        gonk.page_with("/layers", Some(&a))
            .body
            .contains("/layers/releases"),
        "the subscriber sees it in Layers"
    );
    assert!(
        !gonk
            .page_with("/layers", Some(&b))
            .body
            .contains("/layers/releases"),
        "the other viewer does not"
    );

    let saved = std::fs::read_to_string(dir.path().join("layers-view/subscription.json")).unwrap();
    assert!(saved.contains(r#""feed": "releases""#), "{saved}");
    assert!(saved.contains(r#""concepts": []"#), "{saved}");

    let answer = gonk.post_with(
        "/feed/subscribe",
        &[("feed", "releases"), ("action", "unsubscribe")],
        Some(&a),
    );
    assert_eq!(answer.status, 200, "{}", answer.body);
    let page_a = gonk.page_with("/feed", Some(&a)).body;
    assert!(
        page_a.contains(">Subscribe") && !page_a.contains("Unsubscribe"),
        "{page_a}"
    );
}

#[test]
fn a_subscription_needs_the_read_grant_and_the_same_origin() {
    let host = Host::open();
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), Vec::new());
    let answer = gonk.post(
        "/feed/subscribe",
        &[("feed", "releases"), ("action", "subscribe")],
    );
    assert_ne!(answer.status, 200, "{}", answer.body);

    let reader = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    let refused = reader.raw(
        "POST",
        "/feed/subscribe",
        &[
            ("Origin", "http://evil.example".to_string()),
            ("Sec-Fetch-Site", "cross-site".to_string()),
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
        ],
        "feed=releases&action=subscribe",
    );
    assert_ne!(refused.status, 200, "{}", refused.body);
}

#[test]
fn a_concept_filter_narrows_the_page_and_includes_narrower_concepts() {
    let host = Host::open();
    host.publish("A release", &[RELEASE]);
    host.publish("A patch", &[PATCH]);
    host.publish("A feature", &[FEATURE]);
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    subscribe(&gonk);

    // A subscribed feed draws its scheme as a tree of checkboxes.
    let feeds = gonk.page("/feed").body;
    assert!(feeds.contains("type='checkbox'"), "{feeds}");
    assert!(feeds.contains("name='concept'"), "{feeds}");
    assert!(feeds.contains("Filter Releases by concept"), "{feeds}");
    assert!(feeds.contains("<fieldset"), "{feeds}");
    let release = feeds.find(">Release<").expect("the broader concept");
    let patch = feeds.find(">Patch<").expect("the narrower concept");
    assert!(patch > release, "narrower drawn under broader: {feeds}");
    assert!(
        feeds[release..patch].contains("<ul class='concepts'>"),
        "a nested list: {feeds}"
    );

    // None checked: the whole feed.
    let whole = gonk.page("/layers/releases").body;
    for title in ["A release", "A patch", "A feature"] {
        assert!(whole.contains(title), "{title}: {whole}");
    }
    assert!(whole.contains("Showing the whole feed"), "{whole}");

    let answer = gonk.post(
        "/feed/subscribe",
        &[
            ("feed", "releases"),
            ("action", "filter"),
            ("concept", RELEASE),
        ],
    );
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(answer.body.contains("Filter saved"), "{}", answer.body);

    let filtered = gonk.page("/layers/releases").body;
    assert!(filtered.contains("A release"), "{filtered}");
    assert!(filtered.contains("A patch"), "narrower: {filtered}");
    assert!(!filtered.contains("A feature"), "{filtered}");
    assert!(filtered.contains("Showing only Release"), "{filtered}");

    let answer = gonk.post(
        "/feed/subscribe",
        &[
            ("feed", "releases"),
            ("action", "filter"),
            ("concept", "urn:t:concept:nope"),
        ],
    );
    assert_ne!(answer.status, 200, "an unknown concept is refused");
}

#[test]
fn unread_counts_respect_the_filter() {
    let host = Host::open();
    host.publish("Before subscribing", &[RELEASE]);
    let view = view_as(&host, "brian", None);
    let gonk = Gonk::start(Some(Arc::clone(&view)), tokens());
    subscribe(&gonk);
    assert_eq!(
        view.unread("anonymous", None),
        Some(0),
        "what was published before is not new to a new subscriber"
    );
    gonk.post(
        "/feed/subscribe",
        &[
            ("feed", "releases"),
            ("action", "filter"),
            ("concept", FEATURE),
        ],
    );
    host.publish("A patch", &[PATCH]);
    host.publish("A feature", &[FEATURE]);
    assert!(
        eventually(Duration::from_secs(10), || {
            view.refresh().unwrap();
            view.unread("anonymous", None) == Some(1)
        }),
        "only the feature is new: {:?}",
        view.unread("anonymous", None)
    );
    let overview = gonk.page("/layers").body;
    assert!(overview.contains("1 new"), "{overview}");
}

#[test]
fn an_unsubscribed_feed_is_absent_from_layers_and_the_followers_wait() {
    let host = Host::open();
    let door: Arc<dyn Door> = host.door("brian");
    let recording = Arc::new(Recording {
        inner: door,
        waits: Mutex::new(Vec::new()),
    });
    let view = LayersView::new(
        Arc::clone(&recording) as Arc<dyn Door>,
        "scratch-door".to_string(),
        None,
        Waits { feed_secs: 1 },
    );
    view.refresh().unwrap();
    let gonk = Gonk::start(Some(Arc::clone(&view)), tokens());
    let page = gonk.page("/layers").body;
    assert!(!page.contains("Releases"), "{page}");
    assert_eq!(view.unread("anonymous", None), Some(0));
    let refused = gonk.page("/layers/releases");
    assert_eq!(refused.status, 404, "{}", refused.body);
    view.start();
    assert!(eventually(Duration::from_secs(10), || !recording
        .waits
        .lock()
        .unwrap()
        .is_empty()));
    assert!(
        recording
            .waits
            .lock()
            .unwrap()
            .iter()
            .all(|w| !w.contains("releases")),
        "{:?}",
        recording.waits.lock().unwrap()
    );

    subscribe(&gonk);
    assert!(
        eventually(Duration::from_secs(10), || recording
            .waits
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.contains("releases"))),
        "{:?}",
        recording.waits.lock().unwrap()
    );
    assert!(gonk.page("/layers").body.contains("Releases"));
}

#[test]
fn a_feed_page_has_no_post_form_even_for_its_publisher() {
    let host = Host::open();
    host.publish("Version 2", &[RELEASE]);
    // Bobby is the feed's publisher and holds `say` on it.
    let gonk = Gonk::start(Some(view_as(&host, "bobby", None)), tokens());
    subscribe(&gonk);
    let page = gonk.page("/layers/releases").body;
    assert!(page.contains("Version 2"), "{page}");
    assert!(!page.contains("id='say'"), "{page}");
    assert!(!page.contains("/layers/releases/say"), "{page}");
    let refused = gonk.post("/layers/releases/say", &[("body", "nope")]);
    assert_eq!(refused.status, 403, "{}", refused.body);
    assert!(
        !host.entries("releases").contains("nope"),
        "nothing was said"
    );
}

#[test]
fn the_ask_form_posts_a_question_with_its_subject_to_the_viewers_own_channel() {
    let host = Host::open();
    let entry = host.publish("Version 3", &[RELEASE]);
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    subscribe(&gonk);
    let id = entry_id(&entry);
    let page = gonk.page("/layers/releases").body;
    assert!(page.contains("Ask a question about this"), "{page}");
    assert!(
        page.contains("name='channel' type='hidden' value='talk'"),
        "the one own channel: {page}"
    );

    let answer = gonk.post(
        "/feed/ask",
        &[
            ("feed", "releases"),
            ("entry", id),
            ("channel", "talk"),
            ("body", "Does this break config?"),
        ],
    );
    assert_eq!(answer.status, 200, "{}", answer.body);
    let talk = host.entries("talk");
    assert!(talk.contains("Question"), "{talk}");
    assert!(talk.contains("Does this break config?"), "{talk}");
    assert!(talk.contains(&entry), "the subject names the entry: {talk}");
    assert!(
        talk.contains("dcterms:subject") || talk.contains("http://purl.org/dc/terms/subject"),
        "{talk}"
    );

    // In that channel the question links back to the feed entry.
    let channel = gonk.page("/layers/talk").body;
    assert!(
        channel.contains(&format!("/layers/releases#entry-{id}")),
        "{channel}"
    );
    assert!(channel.contains("an entry of Releases"), "{channel}");
}

#[test]
fn the_ask_form_refuses_a_feed_and_a_channel_outside_the_identitys() {
    let host = Host::open();
    let entry = host.publish("Version 4", &[RELEASE]);
    let gonk = Gonk::start(Some(view_as(&host, "bobby", None)), tokens());
    subscribe(&gonk);
    // `releases` is a channel the publisher may say on, but it is a feed.
    let refused = gonk.post(
        "/feed/ask",
        &[
            ("feed", "releases"),
            ("entry", entry_id(&entry)),
            ("channel", "releases"),
            ("body", "x"),
        ],
    );
    assert_eq!(refused.status, 403, "{}", refused.body);
}

#[test]
fn the_ask_form_is_hidden_when_the_identity_may_say_nowhere() {
    let host = Host::open();
    host.publish("Version 5", &[RELEASE]);
    // Chris reads the feed and talk, and may say on neither.
    let gonk = Gonk::start(Some(view_as(&host, "chris", None)), tokens());
    subscribe(&gonk);
    let page = gonk.page("/layers/releases").body;
    assert!(page.contains("Version 5"), "{page}");
    assert!(!page.contains("Ask a question about this"), "{page}");
}

#[test]
fn the_ask_form_offers_a_choice_when_the_identity_has_several_channels() {
    let host = Host::open();
    let entry = host.publish("Version 7", &[RELEASE]);
    let gonk = Gonk::start(Some(view_as(&host, "dana", None)), tokens());
    subscribe(&gonk);
    let page = gonk.page("/layers/releases").body;
    let id = entry_id(&entry);
    assert!(
        page.contains(&format!("<label for='ask-{id}-channel'>Ask in</label>")),
        "{page}"
    );
    assert!(page.contains("<option value='ops'>Ops</option>"), "{page}");
    assert!(
        page.contains("<option value='talk'>Talk</option>"),
        "{page}"
    );
    let answer = gonk.post(
        "/feed/ask",
        &[
            ("feed", "releases"),
            ("entry", id),
            ("channel", "ops"),
            ("body", "Which ops window?"),
        ],
    );
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(host.entries("ops").contains("Which ops window?"));
    assert!(!host.entries("talk").contains("Which ops window?"));
}

#[test]
fn the_form_is_hidden_without_the_post_token() {
    let host = Host::open();
    host.publish("Version 6", &[RELEASE]);
    let gonk = Gonk::start(
        Some(view_as(&host, "brian", None)),
        vec![CAP_READ.to_string()],
    );
    subscribe(&gonk);
    let page = gonk.page("/layers/releases").body;
    assert!(page.contains("Version 6"), "{page}");
    assert!(!page.contains("Ask a question about this"), "{page}");
}
