//! Ledger [#1675](http://localhost:1062/l/default/item/1675): the Layers page, against a
//! SCRATCH layer host.
//!
//! The host is the published `ikigai-layers` in process, behind a door that mints each
//! participant's principal and holds the grant its enrollment would give, as the mirror's tests
//! do (`tests/layer_mirror_1190.rs`); one test serves it over QUIC with scratch certificates and
//! reaches it through a `gonk.layers.view` line, the way the binary does. gonk's HTTP door is a
//! scratch one on `127.0.0.1:0`. Nothing here touches a live service or a real certificate set.
//!
//! - [`entries_render_newest_first_with_their_kinds`]
//! - [`the_unread_marker_moves_when_the_layer_is_opened`]
//! - [`a_post_comes_back_attributed_to_the_identity`]
//! - [`a_layer_without_say_and_an_identity_that_is_not_a_person_get_no_form`]
//! - [`a_file_downloads_only_when_its_digest_matches`]
//! - [`the_long_poll_wakes_on_a_new_entry`]
//! - [`an_unconfigured_page_says_how_to_configure_it`]
//! - [`a_cross_origin_post_is_refused_and_says_nothing`]
//! - [`an_anonymous_caller_holds_neither_token`]
//! - [`a_task_layer_is_read_only`]
//! - [`the_page_reaches_a_scratch_door_over_quic`]
//! - [`the_long_poll_does_not_clear_the_flash`]
//! - [`an_unreachable_door_is_one_banner`]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

const PREFIXES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
"#;

/// The edge's talk shapes, in miniature: a message, a question, a brief and a file.
fn channel_shapes(layer: &str) -> String {
    let stamps =
        "[ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] , \
         [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] , \
         [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ]";
    let reply = format!(
        "[ sh:path ik:inReplyTo ; sh:maxCount 1 ; sh:nodeKind sh:IRI ; sh:pattern \"^urn:layer:{layer}:entry:\" ]"
    );
    let text = |class: &str| {
        format!(
            "<urn:t:shape:{class}> a sh:NodeShape ; sh:targetClass ik:{class} ; sh:closed true ; \
             sh:ignoredProperties ( rdf:type ) ; sh:property \
             [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , {reply} , {stamps} .\n"
        )
    };
    format!(
        "{PREFIXES}{}{}\
         <urn:t:shape:brief> a sh:NodeShape ; sh:targetClass ik:Brief ; sh:closed true ; \
         sh:ignoredProperties ( rdf:type ) ; sh:property \
         [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
         [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
         [ sh:path ik:repository ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] , {reply} , {stamps} .\n\
         <urn:t:shape:file> a sh:NodeShape ; sh:targetClass ik:File ; sh:closed true ; \
         sh:ignoredProperties ( rdf:type ) ; sh:property \
         [ sh:path ik:fileName ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
         [ sh:path ik:mediaType ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
         [ sh:path ik:contentHash ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , \
         [ sh:path ik:byteSize ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] , \
         [ sh:path ik:encoding ; sh:minCount 1 ; sh:maxCount 1 ; sh:in ( \"text\" \"base64\" ) ] , \
         [ sh:path ik:content ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] , {reply} , {stamps} .\n",
        text("Message"),
        text("Question"),
    )
}

const CHANNEL_CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#", "dcterms": "http://purl.org/dc/terms/",
  "prov": "http://www.w3.org/ns/prov#", "xsd": "http://www.w3.org/2001/XMLSchema#",
  "Message": "ik:Message", "Question": "ik:Question", "Brief": "ik:Brief", "File": "ik:File",
  "title": "dcterms:title", "body": "ik:body",
  "repository": { "@id": "ik:repository", "@type": "@id" },
  "fileName": "ik:fileName", "mediaType": "ik:mediaType", "contentHash": "ik:contentHash",
  "byteSize": { "@id": "ik:byteSize", "@type": "xsd:integer" },
  "encoding": "ik:encoding", "content": "ik:content",
  "inReplyTo": { "@id": "ik:inReplyTo", "@type": "@id" },
  "attributedTo": { "@id": "prov:wasAttributedTo", "@type": "@id" },
  "created": { "@id": "dcterms:created", "@type": "xsd:dateTime" },
  "seq": { "@id": "ik:seq", "@type": "xsd:integer" }
} }"#;

const TASK_SHAPES: &str = r#"<urn:t:shape:offer> a sh:NodeShape ;
  sh:targetClass ik:Offer ;
  sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path ik:brief ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .
"#;

const TASK_CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#", "dcterms": "http://purl.org/dc/terms/",
  "prov": "http://www.w3.org/ns/prov#", "xsd": "http://www.w3.org/2001/XMLSchema#",
  "Offer": "ik:Offer", "title": "dcterms:title", "brief": "ik:brief",
  "attributedTo": { "@id": "prov:wasAttributedTo", "@type": "@id" },
  "created": { "@id": "dcterms:created", "@type": "xsd:dateTime" }
} }"#;

const PARTICIPANTS: &str = r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:collab:participant:brian>       a foaf:Person .
<urn:collab:participant:bobby>       a foaf:Person .
<urn:collab:participant:plasma-gonk> a ik:Machine ; prov:actedOnBehalfOf <urn:collab:participant:brian> .
"#;

/// Who holds what. `notes` is a channel Brian only reads; `tasks` a task layer he watches.
/// One layer's holders: each participant's short name and verbs.
type Holders = &'static [(&'static str, &'static [&'static str])];

const CREW: &[(&str, Holders)] = &[
    (
        "talk",
        &[
            ("brian", &["read", "feed", "say"]),
            ("bobby", &["read", "feed", "say"]),
            ("plasma-gonk", &["read", "feed", "say"]),
        ],
    ),
    (
        "notes",
        &[
            ("brian", &["read", "feed"]),
            ("bobby", &["read", "feed", "say"]),
        ],
    ),
    (
        "tasks",
        &[
            ("brian", &["read", "feed"]),
            ("bobby", &["read", "feed", "out", "take"]),
        ],
    ),
];

fn participant(slug: &str) -> String {
    format!("urn:collab:participant:{slug}")
}

/// What the door's enrollment grants `slug`: its verbs on each layer, and the collaboration.
fn grant(slug: &str) -> Capability {
    let mut scopes = vec!["urn:cap:collab:read:crew".to_string()];
    for (layer, holders) in CREW {
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

/// A layer participant: the door's mint of a principal with its grant.
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
struct Host {
    _config: tempfile::TempDir,
    _state: tempfile::TempDir,
    layers: Arc<ikigai_layers::Host>,
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
        for layer in ["talk", "notes"] {
            write(
                &format!("layers/{layer}.shapes.ttl"),
                &channel_shapes(layer),
            );
            write(&format!("layers/{layer}.context.jsonld"), CHANNEL_CONTEXT);
        }
        write(
            "layers/talk.toml",
            "model = \"channel\"\nlabel = \"Talk\"\nshapes = \"talk.shapes.ttl\"\ncontext = \"talk.context.jsonld\"\nentry_max = \"512KiB\"\n",
        );
        write(
            "layers/notes.toml",
            "model = \"channel\"\nlabel = \"Notes\"\nshapes = \"notes.shapes.ttl\"\ncontext = \"notes.context.jsonld\"\n",
        );
        write(
            "layers/tasks.shapes.ttl",
            &format!("{PREFIXES}{TASK_SHAPES}"),
        );
        write("layers/tasks.context.jsonld", TASK_CONTEXT);
        write(
            "layers/tasks.toml",
            "model = \"tuplespace\"\nlabel = \"Tasks\"\nshapes = \"tasks.shapes.ttl\"\ncontext = \"tasks.context.jsonld\"\naddressed = true\n",
        );
        write("participants.ttl", PARTICIPANTS);
        let mut crew = String::new();
        for (layer, holders) in CREW {
            crew.push_str(&format!("[[mount]]\nlayer = \"{layer}\"\n"));
            for (who, verbs) in *holders {
                let verbs: Vec<String> = verbs.iter().map(|v| format!("\"{v}\"")).collect();
                crew.push_str(&format!("{who} = [{}]\n", verbs.join(", ")));
            }
            crew.push('\n');
        }
        write("collabs/crew.toml", &crew);
        let state = tempfile::tempdir().unwrap();
        let host = ikigai_layers::Host::open(config.path(), state.path()).expect("the host");
        let layers = host;
        let kernel = Arc::new(Host::kernel_over(&layers));
        Host {
            _config: config,
            _state: state,
            layers,
            kernel,
        }
    }

    /// A kernel over the host's layers (the host is the state, so two kernels see one feed).
    fn kernel_over(layers: &Arc<ikigai_layers::Host>) -> Kernel {
        let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_layers::space(layers))];
        Kernel::with_meta_renderer(
            Arc::new(Fallback::new(spaces)),
            Arc::new(ikigai_vocab::TurtleRenderer),
        )
        .with_clock(Arc::new(SystemClock))
    }

    fn door(&self, slug: &str) -> Arc<dyn Door> {
        Arc::new(As {
            kernel: Arc::clone(&self.kernel),
            capability: grant(slug),
        })
    }

    /// Say `turtle` on `layer` as `slug`; the entry's IRI.
    fn say(&self, slug: &str, layer: &str, turtle: &str) -> String {
        let request = Request::new(
            Verb::Sink,
            Iri::parse(format!("urn:layer:{layer}:say")).unwrap(),
        )
        .with_arg("content", ArgRef::Inline(turtle.as_bytes().to_vec()));
        let answer = self.door(slug).call(request).expect("the host admits it");
        String::from_utf8(answer.bytes).unwrap().trim().to_string()
    }

    fn message(&self, slug: &str, layer: &str, body: &str) -> String {
        self.say(
            slug,
            layer,
            &format!(
                "@prefix ik: <https://ikigai-rs.dev/ns#> .\n<urn:layer:new> a ik:Message ; ik:body {} .",
                quoted(body)
            ),
        )
    }

    /// The entries of `layer` as Bobby reads them, as Turtle.
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

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

/// A scratch gonk HTTP door with the Layers page bound over `view`.
struct Gonk {
    addr: SocketAddr,
    _config: tempfile::TempDir,
}

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
        let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
        });
        let pages = layers_view::bind(web::space(Arc::clone(&face)), &face, view);
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), pages));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
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
        Response::parse(&String::from_utf8_lossy(&response))
    }

    /// A person opening a page.
    fn page(&self, path: &str) -> Response {
        self.raw(
            "GET",
            path,
            &[
                ("Accept", CHROME_ACCEPT.to_string()),
                ("Sec-Fetch-Site", "none".to_string()),
            ],
            "",
        )
    }

    /// An htmx GET from gonk's own page.
    fn htmx(&self, path: &str) -> Response {
        self.raw(
            "GET",
            path,
            &[
                ("Accept", "*/*".to_string()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
                ("Sec-Fetch-Mode", "cors".to_string()),
                ("HX-Request", "true".to_string()),
            ],
            "",
        )
    }

    /// A same-origin form post, as htmx sends one.
    fn post(&self, path: &str, fields: &[(&str, &str)], origin: &str) -> Response {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let same = origin == self.origin();
        self.raw(
            "POST",
            path,
            &[
                ("Accept", "*/*".to_string()),
                ("Origin", origin.to_string()),
                (
                    "Sec-Fetch-Site",
                    if same { "same-origin" } else { "cross-site" }.to_string(),
                ),
                ("HX-Request", "true".to_string()),
                (
                    "Content-Type",
                    "application/x-www-form-urlencoded".to_string(),
                ),
            ],
            &body,
        )
    }
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

struct Response {
    status: u16,
    head: String,
    body: String,
}

impl Response {
    fn parse(response: &str) -> Response {
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        Response {
            status,
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.head, self.body)
    }
}

fn quick() -> Waits {
    Waits {
        feed_secs: 1,
        client: Duration::from_secs(8),
    }
}

/// A view through `slug`'s door, settled (read once) before it is served.
fn view_as(host: &Host, slug: &str, cursors: Option<std::path::PathBuf>) -> Arc<LayersView> {
    let view = LayersView::new(
        host.door(slug),
        "scratch-door".to_string(),
        cursors,
        quick(),
    );
    view.refresh().expect("the scratch door answers");
    view
}

fn tokens() -> Vec<String> {
    vec![CAP_READ.to_string(), CAP_SAY.to_string()]
}

/// The new entries' position in `page`, by body: every needle must be there.
fn positions(page: &str, needles: &[&str]) -> Vec<usize> {
    needles
        .iter()
        .map(|n| {
            page.find(n)
                .unwrap_or_else(|| panic!("`{n}` is not on the page:\n{page}"))
        })
        .collect()
}

/// Poll `check` for up to `within`.
fn eventually(within: Duration, mut check: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    check()
}

#[test]
fn entries_render_newest_first_with_their_kinds() {
    let host = Host::open();
    host.message("bobby", "talk", "the first word");
    host.say(
        "bobby",
        "talk",
        "@prefix ik: <https://ikigai-rs.dev/ns#> .\n<urn:layer:new> a ik:Question ; ik:body \"is <b>this</b> escaped?\" .",
    );
    host.say(
        "bobby",
        "talk",
        "@prefix ik: <https://ikigai-rs.dev/ns#> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
         <urn:layer:new> a ik:Brief ; dcterms:title \"Layers page\" ; ik:body \"Build it.\" ;\n\
         ik:repository <https://github.com/ikigai-rs/ikigai-gonk> .",
    );
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    let page = gonk.page("/layers/talk");
    assert_eq!(page.status, 200, "{page:?}");
    let at = positions(
        &page.body,
        &["Build it.", "is &lt;b&gt;this", "the first word"],
    );
    assert!(at[0] < at[1] && at[1] < at[2], "newest first: {at:?}");
    assert!(page.body.contains("Brief from bobby"), "{}", page.body);
    assert!(page.body.contains("Question from bobby"));
    assert!(page.body.contains("Message from bobby"));
    assert!(page.body.contains("Layers page"), "a brief's title");
    assert!(
        page.body
            .contains("href='https://github.com/ikigai-rs/ikigai-gonk'"),
        "a brief's repository, as a link"
    );
    assert!(
        !page.body.contains("<b>this</b>"),
        "a body is text, never markup"
    );
    // The header offers the page to a caller who reads it, and the say form is there.
    assert!(
        page.body.contains("class='navlink' href='/layers'"),
        "{}",
        page.body
    );
    assert!(page.body.contains("id='say'"));
    assert!(page.body.contains("reads and posts as"));
}

#[test]
fn the_unread_marker_moves_when_the_layer_is_opened() {
    let host = Host::open();
    host.message("bobby", "talk", "one");
    host.message("bobby", "talk", "two");
    let state = tempfile::tempdir().unwrap();
    let cursors = state.path().join("layers-view").join("cursors.json");
    let view = view_as(&host, "brian", Some(cursors.clone()));
    let gonk = Gonk::start(Some(Arc::clone(&view)), tokens());

    let overview = gonk.page("/layers");
    assert_eq!(overview.status, 200, "{overview:?}");
    assert!(overview.body.contains("2 new"), "{}", overview.body);

    let talk = gonk.page("/layers/talk");
    assert_eq!(talk.body.matches(">New<").count(), 2, "{}", talk.body);
    // Opening it was reading it.
    let overview = gonk.page("/layers");
    assert!(overview.body.contains("nothing new"), "{}", overview.body);
    assert!(cursors.is_file(), "the cursor is kept on disk");
    let again = gonk.page("/layers/talk");
    assert_eq!(again.body.matches(">New<").count(), 0, "{}", again.body);

    // A live update delivered to the open page is read too: the follower (started by the
    // page) brings it, and the long poll hands it over.
    host.message("bobby", "talk", "three");
    let wait = gonk.htmx("/layers/~/wait?layer=talk&after=2&down=0");
    assert_eq!(wait.status, 200, "{wait:?}");
    assert!(wait.body.contains("three"), "{}", wait.body);
    let overview = gonk.page("/layers");
    assert!(
        overview.body.contains("nothing new"),
        "delivered to an open page is read"
    );

    host.message("bobby", "talk", "four");
    assert!(
        eventually(Duration::from_secs(10), || gonk
            .page("/layers")
            .body
            .contains("1 new")),
        "the follower counts what arrived since"
    );
    // A second viewer has its own cursor: the file names the anonymous viewer only.
    let kept: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cursors).unwrap()).unwrap();
    assert!(kept["anonymous"]["talk"].as_u64().unwrap() >= 3, "{kept}");
}

#[test]
fn a_post_comes_back_attributed_to_the_identity() {
    let host = Host::open();
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    let first = host.message("bobby", "talk", "ask me something");
    let id = first.rsplit(':').next().unwrap().to_string();
    let posted = gonk.post(
        "/layers/talk/say",
        &[
            ("kind", "question"),
            ("body", "Does \"this\" work?"),
            ("reply_to", &first),
            // A form cannot name its author: the door stamps who said it.
            ("author", "urn:collab:participant:bobby"),
        ],
        &gonk.origin(),
    );
    assert_eq!(posted.status, 200, "{posted:?}");
    assert!(
        posted.body.contains("Posted to Talk as a question"),
        "{}",
        posted.body
    );
    assert!(posted.body.contains("id='say'"), "a fresh form comes back");
    let stored = host.entries("talk");
    assert!(
        stored.contains("Does \\\"this\\\" work?") || stored.contains("Does \"this\" work?"),
        "{stored}"
    );
    assert!(stored.contains("urn:collab:participant:brian"), "{stored}");
    assert!(
        stored.contains(&format!("urn:layer:talk:entry:{id}")),
        "the reply names its entry"
    );
    // And the page shows it, from the identity.
    assert!(
        eventually(Duration::from_secs(10), || {
            gonk.page("/layers/talk")
                .body
                .contains("Question from brian (you)")
        }),
        "the post comes back attributed to the configured identity"
    );
    // An empty post is refused, with the reason.
    let empty = gonk.post("/layers/talk/say", &[("body", "   ")], &gonk.origin());
    assert_eq!(empty.status, 400, "{empty:?}");
    assert!(empty.body.contains("empty"), "{}", empty.body);
}

#[test]
fn a_layer_without_say_and_an_identity_that_is_not_a_person_get_no_form() {
    let host = Host::open();
    host.message("bobby", "notes", "read-only for brian");
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    let notes = gonk.page("/layers/notes");
    assert_eq!(notes.status, 200, "{notes:?}");
    assert!(notes.body.contains("read-only for brian"));
    assert!(
        !notes.body.contains("id='say'"),
        "no form without say: {}",
        notes.body
    );
    assert!(!notes.body.contains(">Reply<"));
    let refused = gonk.post("/layers/notes/say", &[("body", "hi")], &gonk.origin());
    assert_eq!(refused.status, 403, "{refused:?}");
    assert!(!host.entries("notes").contains("\"hi\""));

    // gonk's own machine identity holds say on talk, and the page still never posts as it.
    let machine = Gonk::start(Some(view_as(&host, "plasma-gonk", None)), tokens());
    let talk = machine.page("/layers/talk");
    assert_eq!(talk.status, 200, "{talk:?}");
    assert!(!talk.body.contains("id='say'"), "{}", talk.body);
    assert!(talk.body.contains("not a person"), "{}", talk.body);
    let refused = machine.post(
        "/layers/talk/say",
        &[("body", "as a machine")],
        &machine.origin(),
    );
    assert_eq!(refused.status, 403, "{refused:?}");
    assert!(!host.entries("talk").contains("as a machine"));
}

#[test]
fn a_file_downloads_only_when_its_digest_matches() {
    let host = Host::open();
    let bytes = "<script>alert(1)</script>\n";
    let file = |hash: &str| {
        format!(
            "@prefix ik: <https://ikigai-rs.dev/ns#> .\n<urn:layer:new> a ik:File ; \
             ik:fileName \"page.html\" ; ik:mediaType \"text/html\" ; ik:contentHash \"{hash}\" ; \
             ik:byteSize {} ; ik:encoding \"text\" ; ik:content {} .",
            bytes.len(),
            quoted(bytes)
        )
    };
    let good = host.say(
        "bobby",
        "talk",
        &file(&format!("sha256:{}", sha256(bytes.as_bytes()))),
    );
    let bad = host.say(
        "bobby",
        "talk",
        &file(&format!("sha256:{}", sha256(b"something else"))),
    );
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    let page = gonk.page("/layers/talk");
    assert!(page.body.contains("download='page.html'"), "{}", page.body);
    assert!(
        !page.body.contains("<script>alert"),
        "the content is never on the page"
    );

    let id = |iri: &str| iri.rsplit(':').next().unwrap().to_string();
    let got = gonk.page(&format!("/layers/talk/file/{}", id(&good)));
    assert_eq!(got.status, 200, "{got:?}");
    assert_eq!(got.body, bytes);
    assert_eq!(
        got.header("content-type").as_deref(),
        Some("application/octet-stream"),
        "never the stated media type: {got:?}"
    );
    assert!(
        got.header("content-security-policy")
            .is_some_and(|csp| csp.contains("sandbox")),
        "{got:?}"
    );
    let refused = gonk.page(&format!("/layers/talk/file/{}", id(&bad)));
    assert_eq!(refused.status, 409, "{refused:?}");
    assert!(refused.body.contains("digest"), "{}", refused.body);
    assert!(!refused.body.contains("alert(1)"), "nothing was sent");
}

#[test]
fn the_long_poll_wakes_on_a_new_entry() {
    let host = Host::open();
    host.message("bobby", "talk", "before");
    let gonk = Arc::new(Gonk::start(Some(view_as(&host, "brian", None)), tokens()));
    // Open the page: it starts the follower and carries the watcher at the layer's end.
    let page = gonk.page("/layers/talk");
    assert!(
        page.body
            .contains("/layers/~/wait?layer=talk&amp;after=1&amp;down=0"),
        "{}",
        page.body
    );
    let waiting = Arc::clone(&gonk);
    let started = Instant::now();
    let poll = std::thread::spawn(move || waiting.htmx("/layers/~/wait?layer=talk&after=1&down=0"));
    std::thread::sleep(Duration::from_millis(500));
    host.message("bobby", "talk", "after, live");
    let answer = poll.join().unwrap();
    assert_eq!(answer.status, 200, "{answer:?}");
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "it woke on the entry, not at its timeout: {:?}",
        started.elapsed()
    );
    assert!(answer.body.contains("after, live"), "{}", answer.body);
    assert!(answer.body.contains("afterbegin:#layer-entries"));
    assert!(
        answer.body.contains("1 new entry in Talk."),
        "the live region's sentence"
    );
    assert!(
        answer.body.contains("after=2"),
        "the next watcher is past it: {}",
        answer.body
    );
    // With nothing new, it answers at its bound with a watcher and no entries.
    let quiet = gonk.htmx("/layers/~/wait?layer=talk&after=2&down=0");
    assert_eq!(quiet.status, 200);
    assert!(!quiet.body.contains("layer-entries"), "{}", quiet.body);
}

#[test]
fn an_unconfigured_page_says_how_to_configure_it() {
    let gonk = Gonk::start(None, tokens());
    let page = gonk.page("/layers");
    assert_eq!(page.status, 200, "{page:?}");
    assert!(page.body.contains("not configured"), "{}", page.body);
    assert!(page.body.contains("gonk.layers.view"), "{}", page.body);
    let say = gonk.post("/layers/talk/say", &[("body", "x")], &gonk.origin());
    assert_eq!(say.status, 404, "{say:?}");
}

#[test]
fn a_cross_origin_post_is_refused_and_says_nothing() {
    let host = Host::open();
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    for origin in ["https://evil.example", "http://localhost:8090"] {
        let refused = gonk.post(
            "/layers/talk/say",
            &[("kind", "message"), ("body", "forged")],
            origin,
        );
        assert_eq!(refused.status, 403, "{origin}: {refused:?}");
    }
    assert!(!host.entries("talk").contains("forged"));
}

#[test]
fn an_anonymous_caller_holds_neither_token() {
    let host = Host::open();
    host.message("bobby", "talk", "private words");
    // What gonk mints for any loopback process: the ledgers' tokens, never the page's.
    let anonymous = ikigai_gonk::grants::anonymous_http_grants(&["default".to_string()]).unwrap();
    assert!(!anonymous
        .iter()
        .any(|s| s.starts_with("urn:cap:gonk:layers")));
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), anonymous);
    let page = gonk.page("/layers/talk");
    assert_eq!(page.status, 200, "{page:?}");
    assert!(
        page.body.contains("Sign in to read the layers."),
        "{}",
        page.body
    );
    assert!(!page.body.contains("private words"));
    assert!(
        !page.body.contains("class='navlink' href='/layers'"),
        "no header link without the grant"
    );
    let say = gonk.post("/layers/talk/say", &[("body", "anonymous")], &gonk.origin());
    assert_eq!(say.status, 403, "{say:?}");
    let wait = gonk.htmx("/layers/~/wait?layer=talk&after=0");
    assert_eq!(wait.status, 403, "{wait:?}");
    let file = gonk.page("/layers/talk/file/x");
    assert_eq!(file.status, 403, "{file:?}");
    assert!(!host.entries("talk").contains("anonymous"));
}

#[test]
fn a_task_layer_is_read_only() {
    let host = Host::open();
    let offer = "@prefix ik: <https://ikigai-rs.dev/ns#> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
                 <urn:layer:tasks:offer:1> a ik:Offer ; dcterms:title \"Wire the page\" ; ik:brief \"Do it.\" .";
    host.door("bobby")
        .call(
            Request::new(Verb::Sink, Iri::parse("urn:layer:tasks:tuples").unwrap())
                .with_arg("content", ArgRef::Inline(offer.as_bytes().to_vec())),
        )
        .unwrap();
    let offer2 = offer
        .replace("offer:1", "offer:2")
        .replace("Wire the page", "Taken one");
    host.door("bobby")
        .call(
            Request::new(Verb::Sink, Iri::parse("urn:layer:tasks:tuples").unwrap())
                .with_arg("content", ArgRef::Inline(offer2.into_bytes())),
        )
        .unwrap();
    host.door("bobby")
        .call(
            Request::new(Verb::Delete, Iri::parse("urn:layer:tasks:tuples").unwrap())
                .with_arg("tuple", ArgRef::Inline(b"urn:layer:tasks:offer:2".to_vec())),
        )
        .unwrap();
    let gonk = Gonk::start(Some(view_as(&host, "brian", None)), tokens());
    let page = gonk.page("/layers/tasks");
    assert_eq!(page.status, 200, "{page:?}");
    assert!(page.body.contains("Offer from bobby"), "{}", page.body);
    assert!(page.body.contains("Wire the page"));
    assert!(
        page.body.contains("took Taken one"),
        "a receipt: {}",
        page.body
    );
    assert!(!page.body.contains("id='say'"));
    assert_eq!(
        page.body.matches("<form").count(),
        1,
        "only the header's passkey form: no out, no take: {}",
        page.body
    );
    let refused = gonk.post("/layers/tasks/say", &[("body", "x")], &gonk.origin());
    assert_eq!(refused.status, 403, "{refused:?}");
}

#[test]
fn the_page_reaches_a_scratch_door_over_quic() {
    let host = Host::open();
    host.message("bobby", "talk", "over the wire");
    // Scratch certificates, and the host's kernel behind a QUIC door that mints Brian.
    let certs = tempfile::tempdir().unwrap();
    let (server, client) = (ikigai_quic::generate(), ikigai_quic::generate());
    for (file, pem) in [
        ("server.crt", &server.cert_pem),
        ("client.crt", &client.cert_pem),
        ("client.key", &client.key_pem),
    ] {
        std::fs::write(certs.path().join(file), pem).unwrap();
    }
    let minter: ikigai_quic::Minter = Arc::new(|_| {
        Some(ikigai_quic::Session {
            capability: grant("brian"),
            file_segment: "brian".to_string(),
            principal: Some(participant("brian")),
        })
    });
    let udp = ikigai_quic::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = udp.local_addr().unwrap();
    let trusted = vec![client.cert_pem];
    let kernel = Host::kernel_over(&host.layers);
    std::thread::spawn(move || {
        ikigai_quic::serve_bounded_on(
            kernel,
            udp,
            &server,
            &trusted,
            minter,
            ikigai_quic::DEFAULT_IDLE_TIMEOUT,
            None,
        )
    });
    let line = format!("quic://{addr} {}", certs.path().display());
    let config = layers_view::parse(&line, std::path::Path::new("/nowhere")).unwrap();
    let view = LayersView::connect(&config, None);
    let gonk = Gonk::start(Some(view), tokens());
    let page = gonk.page("/layers/talk");
    assert_eq!(page.status, 200, "{page:?}");
    assert!(page.body.contains("over the wire"), "{}", page.body);
    assert!(
        page.body
            .contains("urn:collab:participant:brian</code>, a person"),
        "{}",
        page.body
    );
    let posted = gonk.post(
        "/layers/talk/say",
        &[("kind", "message"), ("body", "said over QUIC")],
        &gonk.origin(),
    );
    assert_eq!(posted.status, 200, "{posted:?}");
    let stored = host.entries("talk");
    assert!(stored.contains("said over QUIC"), "{stored}");
    assert!(stored.contains("urn:collab:participant:brian"), "{stored}");
}

/// The long poll answers whenever a layer moves, so it must not empty `#flash` the way a
/// person's request does (a post's "Posted" would vanish before anyone read it). No JavaScript
/// runtime here, as in `tests/web.rs::the_badge_poll_does_not_clear_the_flash`: what is pinned
/// is the wiring, the skip before the clear, and the class the watcher carries.
#[test]
fn the_long_poll_does_not_clear_the_flash() {
    let js = include_str!("../web/gonk.js");
    let at = js
        .find("addEventListener(\"htmx:beforeRequest\"")
        .expect("the clearing listener");
    let body = &js[at..at + js[at..].find("\n  });").expect("the listener ends")];
    let skip = body
        .find(".closest(\".layers-watch\")) return;")
        .unwrap_or_else(|| panic!("the long poll is skipped: {body}"));
    let clear = body
        .find("flash(\"\", \"ok\")")
        .expect("other requests clear");
    assert!(skip < clear, "{body}");
    assert!(include_str!("../web/gonk.xsl").contains("<div class=\"layers-watch\""));
}

/// A door that answers nothing: every call is the transport's `Unavailable`.
struct Down;

impl Door for Down {
    fn call(&self, _: Request) -> Result<Representation, Error> {
        Err(Error::Unavailable("connection refused".to_string()))
    }
}

/// An unreachable door is ONE banner on a page that still renders (200), never an error per
/// read; the long poll carries the banner and backs off at the browser.
#[test]
fn an_unreachable_door_is_one_banner() {
    let view = LayersView::new(
        Arc::new(Down),
        "quic://nowhere:1".to_string(),
        None,
        quick(),
    );
    let gonk = Gonk::start(Some(view), tokens());
    let page = gonk.page("/layers");
    assert_eq!(page.status, 200, "{page:?}");
    assert_eq!(
        page.body.matches("cannot be reached").count(),
        1,
        "{}",
        page.body
    );
    assert!(page.body.contains("quic://nowhere:1"));
    assert!(
        page.body.contains("load delay:15s"),
        "the next poll waits: {}",
        page.body
    );
    let wait = gonk.htmx("/layers/~/wait?layer=talk&after=0&down=1");
    assert_eq!(wait.status, 200, "{wait:?}");
    assert_eq!(
        wait.body.matches("cannot be reached").count(),
        1,
        "{}",
        wait.body
    );
}
