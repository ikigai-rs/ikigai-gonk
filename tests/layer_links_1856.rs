//! Ledger [#1856](http://localhost:1062/l/default/item/1856): a task layer's entries link to the
//! right item in the right ledger, on the side that holds it.
//!
//! A SCRATCH layer host (the published `ikigai-layers`, in process, behind a door that mints each
//! participant's principal, as `tests/layers_page_1675.rs` does) and a scratch gonk HTTP door on
//! `127.0.0.1:0` over an in-memory store holding two ledgers, `default` and `crew`. Nothing here
//! touches a live service.
//!
//! - [`the_offering_side_links_its_own_item_in_its_own_ledger`]
//! - [`the_taking_side_links_the_item_it_filed`]
//! - [`hostile_layer_content_is_text`]
//!
//! And the follow-up: a ledger reference in an entry's TEXT links by context, never to `default`
//! by assumption (Brian's rules, 2026-10-18, on the item). The scratch gonk also holds `bobby`
//! (readable) and `secret` (held, but not readable by the page's caller).
//!
//! - [`a_collaborators_bare_reference_is_their_ledger_not_default`]
//! - [`a_qualified_reference_links_only_a_held_readable_ledger`]
//! - [`this_hosts_own_side_links_a_bare_reference_to_default`]
//! - [`hostile_references_are_text`]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, Fallback, Iri, Kernel, Representation, Request, Space, SystemClock,
    Verb,
};
use ikigai_gonk::layer::Door;
use ikigai_gonk::layers_view::{self, LayersView, Waits, CAP_READ};
use ikigai_gonk::{compose, doors, identity::Passkeys, quic, web};
use ikigai_ledger::Ledger;
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;

const LAYER: &str = "tasks";

const SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:t:shape:offer> a sh:NodeShape ; sh:targetClass ik:Offer ; sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path ik:brief ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .
<urn:t:shape:report> a sh:NodeShape ; sh:targetClass ik:Progress , ik:Release , ik:Handback ;
  sh:closed true ; sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path ik:offer ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path ik:addressedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path ik:note ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .
"#;

const CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#", "dcterms": "http://purl.org/dc/terms/",
  "prov": "http://www.w3.org/ns/prov#", "xsd": "http://www.w3.org/2001/XMLSchema#",
  "Offer": "ik:Offer", "Progress": "ik:Progress", "Release": "ik:Release", "Handback": "ik:Handback",
  "title": "dcterms:title", "brief": "ik:brief", "note": "ik:note",
  "offer": { "@id": "ik:offer", "@type": "@id" },
  "addressedTo": { "@id": "ik:addressedTo", "@type": "@id" },
  "attributedTo": { "@id": "prov:wasAttributedTo", "@type": "@id" },
  "created": { "@id": "dcterms:created", "@type": "xsd:dateTime" }
} }"#;

const PARTICIPANTS: &str = r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
<urn:collab:participant:brian> a foaf:Person .
<urn:collab:participant:bobby> a foaf:Person .
"#;

const VERBS: &[&str] = &["read", "feed", "out", "take"];

fn participant(slug: &str) -> String {
    format!("urn:collab:participant:{slug}")
}

fn grant(slug: &str) -> Capability {
    let mut scopes = vec!["urn:cap:collab:read:crew".to_string()];
    scopes.extend(VERBS.iter().map(|v| format!("urn:cap:layer:{v}:{LAYER}")));
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

/// The scratch layer host: one task layer, `tasks`, where brian and bobby both drop and take.
struct Host {
    _config: tempfile::TempDir,
    _state: tempfile::TempDir,
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
        write(&format!("layers/{LAYER}.shapes.ttl"), SHAPES);
        write(&format!("layers/{LAYER}.context.jsonld"), CONTEXT);
        write(
            &format!("layers/{LAYER}.toml"),
            &format!(
                "model = \"tuplespace\"\nlabel = \"Tasks\"\nshapes = \"{LAYER}.shapes.ttl\"\n\
                 context = \"{LAYER}.context.jsonld\"\naddressed = true\n"
            ),
        );
        write("participants.ttl", PARTICIPANTS);
        let verbs: Vec<String> = VERBS.iter().map(|v| format!("\"{v}\"")).collect();
        write(
            "collabs/crew.toml",
            &format!(
                "[[mount]]\nlayer = \"{LAYER}\"\nbrian = [{v}]\nbobby = [{v}]\n",
                v = verbs.join(", ")
            ),
        );
        let state = tempfile::tempdir().unwrap();
        let layers = ikigai_layers::Host::open(config.path(), state.path()).expect("the host");
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
            kernel,
        }
    }

    fn door(&self, slug: &str) -> Arc<dyn Door> {
        Arc::new(As {
            kernel: Arc::clone(&self.kernel),
            capability: grant(slug),
        })
    }

    /// Drop `turtle` into the layer as `slug`.
    fn out(&self, slug: &str, turtle: &str) {
        let content = format!(
            "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
             @prefix dcterms: <http://purl.org/dc/terms/> .\n{turtle}"
        );
        self.door(slug)
            .call(
                Request::new(
                    Verb::Sink,
                    Iri::parse(format!("urn:layer:{LAYER}:tuples")).unwrap(),
                )
                .with_arg("content", ArgRef::Inline(content.into_bytes())),
            )
            .expect("the host admits it");
    }

    /// An offer from `slug`.
    fn offer(&self, slug: &str, id: &str, title: &str, brief: &str) -> String {
        let iri = format!("urn:layer:{LAYER}:offer:{id}");
        self.out(
            slug,
            &format!(
                "<{iri}> a ik:Offer ; dcterms:title {} ; ik:brief {} .",
                quoted(title),
                quoted(brief)
            ),
        );
        iri
    }

    /// Take `tuple` as `slug`.
    fn take(&self, slug: &str, tuple: &str) {
        self.door(slug)
            .call(
                Request::new(
                    Verb::Delete,
                    Iri::parse(format!("urn:layer:{LAYER}:tuples")).unwrap(),
                )
                .with_arg("tuple", ArgRef::Inline(tuple.as_bytes().to_vec())),
            )
            .expect("the take");
    }
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

/// A scratch gonk: an in-memory hub with the ledgers `default` and `crew`, and an HTTP door
/// whose anonymous caller holds the Layers page's read token and read on both ledgers.
struct Gonk {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    _config: tempfile::TempDir,
}

impl Gonk {
    fn start(view: Arc<LayersView>) -> Gonk {
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
            ledgers: ["default", "crew", "bobby", "secret"]
                .map(str::to_string)
                .to_vec(),
            browse_roots: Vec::new(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
        });
        let pages = layers_view::bind(web::space(Arc::clone(&face)), &face, Some(view));
        let http = Arc::new(doors::http_kernel_with(Arc::clone(&hub), pages, None));
        let mut anonymous = vec![CAP_READ.to_string()];
        // `secret` is held (the header's list names it) and NOT readable by this caller.
        for name in ["default", "crew", "bobby"] {
            let ledger = Ledger::parse(name).unwrap();
            anonymous.push(ledger.cap_read());
            anonymous.push(ikigai_store::cap_read_graph(&ledger.graph()));
        }
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
            hub,
            _config: config,
        }
    }

    /// File an item into `ledger` as its owner, with `args`; its number.
    fn file(&self, ledger: &str, content: &str, args: &[(&str, &str)]) -> i64 {
        let request = args.iter().fold(
            Request::new(
                Verb::Sink,
                Iri::parse(format!("urn:iki:ledger:{ledger}:append")).unwrap(),
            )
            .with_arg("content", ArgRef::Inline(content.as_bytes().to_vec()))
            .with_arg("as", ArgRef::Inline(b"application/json".to_vec())),
            |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
        );
        let (answer, _) = Resolver::issue(&*self.hub, request).expect("the append");
        let json: serde_json::Value = serde_json::from_slice(&answer.bytes).unwrap();
        json.pointer("/item/number").unwrap().as_i64().unwrap()
    }

    /// The layer's page, as a person opens it; its status and body.
    fn page(&self) -> (u16, String) {
        self.get(
            &format!("/layers/{LAYER}"),
            "Accept: text/html,application/xhtml+xml\r\nSec-Fetch-Site: none\r\n",
        )
    }

    /// What an open page's poll asks, from the page's first position: every tuple is new.
    fn poll(&self) -> (u16, String) {
        self.get(
            &format!("{}?layer={LAYER}&after=0&door=ok", layers_view::WAIT_PATH),
            "Accept: */*\r\nSec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: cors\r\n\
             HX-Request: true\r\n",
        )
    }

    fn get(&self, path: &str, headers: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\n{headers}Connection: close\r\n\r\n",
            self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let response = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        (status, body.to_string())
    }
}

/// A view through `slug`'s door, read once before it is served.
fn view_as(host: &Host, slug: &str) -> Arc<LayersView> {
    let view = LayersView::new(
        host.door(slug),
        "scratch-door".to_string(),
        None,
        Waits { feed_secs: 1 },
    );
    view.refresh().expect("the scratch door answers");
    view
}

/// The part of `page` from the row whose text holds `needle` to the end of that row.
fn row<'a>(page: &'a str, needle: &str) -> &'a str {
    let at = page
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is not on the page:\n{page}"));
    let start = page[..at].rfind("<li").unwrap_or(0);
    let end = page[at..].find("</li>").map_or(page.len(), |e| at + e);
    &page[start..end]
}

/// The OFFERING gonk: its offer is about an item of ledger `crew`, recorded as the mirror
/// records it (an `about` edge to the offer's IRI). The row links to `/l/crew/item/N`, never to
/// `default`, even though `default` holds an item with the same number. An offer about no item
/// here (the other side's) carries no link.
#[test]
fn the_offering_side_links_its_own_item_in_its_own_ledger() {
    let host = Host::open();
    host.offer("brian", "1", "Wire the page", "Do it.");
    host.offer("bobby", "9", "Their own offer", "Nothing here is about it.");
    let gonk = Gonk::start(view_as(&host, "brian"));
    // `default` holds a #1 and a #2, so a link that assumed `default` would land on a real item.
    gonk.file("default", "Unrelated one\n\nIn default.", &[]);
    gonk.file("default", "Unrelated two\n\nIn default.", &[]);
    let offer = format!("urn:layer:{LAYER}:offer:1");
    let n = gonk.file(
        "crew",
        "Wire the page\n\nDo it.",
        &[("about", offer.as_str())],
    );
    assert_eq!(n, 1, "crew's first item");

    let (status, page) = gonk.page();
    assert_eq!(status, 200, "{page}");
    let ours = row(&page, "Wire the page");
    assert!(
        ours.contains("tracked here as <a class='entry-item' href='/l/crew/item/1'>crew #1</a>")
            || ours.contains(
                "tracked here as <a class=\"entry-item\" href=\"/l/crew/item/1\">crew #1</a>"
            ),
        "the offer links to crew #1: {ours}"
    );
    assert!(!ours.contains("/l/default/item/"), "{ours}");
    let theirs = row(&page, "Their own offer");
    assert!(
        !theirs.contains("tracked here as") && !theirs.contains("/l/"),
        "the other side's offer names no item here: {theirs}"
    );
}

/// The TAKING gonk: it took the offer and filed it into its own ledger (keyed and `about` the
/// offer, as the taker files). The take's receipt and the progress report it dropped both link
/// to the item it filed, in the ledger it filed it in.
#[test]
fn the_taking_side_links_the_item_it_filed() {
    let host = Host::open();
    let offer = host.offer("bobby", "2", "Fix the thing", "Please.");
    host.take("brian", &offer);
    host.out(
        "brian",
        &format!(
            "<urn:layer:{LAYER}:report:offer-2:progress:1> a ik:Progress ; ik:offer <{offer}> ; \
             ik:addressedTo <{}> ; ik:note \"halfway\" .",
            participant("bobby")
        ),
    );
    let gonk = Gonk::start(view_as(&host, "brian"));
    gonk.file("crew", "Something else\n\nNot this.", &[]);
    let n = gonk.file(
        "crew",
        "Fix the thing\n\nPlease.",
        &[
            ("about", offer.as_str()),
            ("key", &format!("layer:{LAYER}:offer:2")),
        ],
    );
    assert_eq!(n, 2);

    let (status, page) = gonk.page();
    assert_eq!(status, 200, "{page}");
    let link = |r: &str| {
        r.contains("href='/l/crew/item/2'>crew #2</a>")
            || r.contains("href=\"/l/crew/item/2\">crew #2</a>")
    };
    let receipt = row(&page, "took Fix the thing");
    assert!(
        link(receipt),
        "the receipt links to the filed item: {receipt}"
    );
    let progress = row(&page, "halfway");
    assert!(
        link(progress),
        "the report links to the filed item: {progress}"
    );
    // And an open page's poll brings the same rows with the same links.
    let (status, poll) = gonk.poll();
    assert_eq!(status, 200, "{poll}");
    assert!(link(row(&poll, "took Fix the thing")), "{poll}");
    assert!(link(row(&poll, "halfway")), "{poll}");
}

/// Layer content is collaborator data: a title, a brief and a note full of quotes, markup and a
/// `javascript:` URL render as text, and the only links a tuple gets are built from this gonk's
/// own ledger.
#[test]
fn hostile_layer_content_is_text() {
    let host = Host::open();
    let hostile = "\"><script>alert(1)</script><a href='javascript:alert(2)'>x</a>";
    let offer = host.offer(
        "bobby",
        "3",
        hostile,
        "javascript:alert(3) \"quoted\" <b>bold</b>",
    );
    host.take("brian", &offer);
    host.out(
        "brian",
        &format!(
            "<urn:layer:{LAYER}:report:offer-3:progress:1> a ik:Progress ; \
             ik:offer <{offer}> ; ik:addressedTo <{}> ; ik:note {} .",
            participant("bobby"),
            quoted(&format!("{hostile} javascript:alert(4)"))
        ),
    );
    // An offer of bobby's this gonk holds no item for.
    host.offer(
        "bobby",
        "4",
        "javascript:alert(5)",
        "\"><img src=x onerror=alert(6)>",
    );
    let gonk = Gonk::start(view_as(&host, "brian"));
    gonk.file(
        "crew",
        "Taken\n\nFiled from a hostile offer.",
        &[("about", offer.as_str())],
    );

    let (status, page) = gonk.page();
    assert_eq!(status, 200, "{page}");
    assert!(!page.contains("<script>alert"), "{page}");
    assert!(!page.to_lowercase().contains("href='javascript:"), "{page}");
    assert!(
        !page.to_lowercase().contains("href=\"javascript:"),
        "{page}"
    );
    assert!(
        page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{page}"
    );
    // The take of the hostile offer still links to the item filed from it, by the store's answer.
    let receipt = row(&page, "took ");
    assert!(
        receipt.contains("/l/crew/item/1"),
        "the receipt links to crew #1: {receipt}"
    );
    // The report on it links to the same item, and its hostile note is text.
    let report = row(&page, "alert(4)");
    assert!(report.contains("/l/crew/item/1"), "{report}");
    // The offer this gonk holds no item for gets no link, and its markup is text.
    let unheld = row(&page, "onerror=alert(6)");
    assert!(!unheld.contains("tracked here as"), "{unheld}");
    assert!(!page.contains("<img src=x"), "{page}");
}

/// Whether `html` holds a link to `href`, in either quoting.
fn links_to(html: &str, href: &str) -> bool {
    html.contains(&format!("href='{href}'")) || html.contains(&format!("href=\"{href}\""))
}

/// Brian's rule 3: a bare `ledger #N` in a COLLABORATOR's entry names their ledger, which this
/// gonk does not hold. It is text with a title, never a link to `default` (which holds a #2
/// here, so the old assumption landed on a real, unrelated item). Fails on main, where every
/// layer body was linkified as `default`.
#[test]
fn a_collaborators_bare_reference_is_their_ledger_not_default() {
    let host = Host::open();
    host.offer(
        "bobby",
        "5",
        "Their task",
        "Picks up where ledger #2 left off.",
    );
    let gonk = Gonk::start(view_as(&host, "brian"));
    gonk.file("default", "One\n\nIn default.", &[]);
    gonk.file("default", "Two\n\nIn default.", &[]);

    for (what, html) in [("page", gonk.page()), ("poll", gonk.poll())] {
        let (status, html) = html;
        assert_eq!(status, 200, "{what}: {html}");
        let theirs = row(&html, "Picks up where");
        assert!(!links_to(theirs, "/l/default/item/2"), "{what}: {theirs}");
        assert!(!theirs.contains("<a href='/l/"), "{what}: {theirs}");
        assert!(
            theirs.contains(
                "<span class='foreign-ref' title='A reference to an item in the author&apos;s \
                 own ledger, which this gonk does not hold'>ledger #2"
            ),
            "{what}: the reference is text with a title: {theirs}"
        );
    }
}

/// Brian's rule 1: a qualified reference links to THAT ledger, whoever wrote it, when this gonk
/// holds it AND the viewer may read it. `bobby` is held and readable; `secret` is held and not
/// readable; `nosuch` is not held. Fails on main, where `bobby#2` was text.
#[test]
fn a_qualified_reference_links_only_a_held_readable_ledger() {
    let host = Host::open();
    host.offer(
        "bobby",
        "6",
        "Qualified references",
        "See bobby#2, crew #3, default:#4, secret#5 and nosuch#3.",
    );
    let gonk = Gonk::start(view_as(&host, "brian"));

    let (status, page) = gonk.page();
    assert_eq!(status, 200, "{page}");
    let r = row(&page, "Qualified references");
    assert!(
        r.contains("<a href='/l/bobby/item/2'>bobby#2</a>"),
        "bobby#2 links into bobby: {r}"
    );
    assert!(r.contains("<a href='/l/crew/item/3'>crew #3</a>"), "{r}");
    assert!(
        r.contains("<a href='/l/default/item/4'>default:#4</a>"),
        "{r}"
    );
    assert!(
        !r.contains("/l/secret/") && r.contains("secret#5"),
        "a ledger the viewer may not read is text: {r}"
    );
    assert!(
        !r.contains("/l/nosuch/") && r.contains("nosuch#3"),
        "a ledger this gonk does not hold is text: {r}"
    );
}

/// Brian's rule 2: a bare `ledger #N` in an entry from THIS host's own side links to `default`:
/// the page's own identity (brian here), and whoever `gonk.layers.local` names
/// ([`LayersView::treat_as_local`]); everyone else's stays text.
#[test]
fn this_hosts_own_side_links_a_bare_reference_to_default() {
    let host = Host::open();
    host.offer("brian", "7", "Our own offer", "Follows ledger #12.");
    host.offer("bobby", "8", "Their offer", "Their ledger #13.");
    let gonk = Gonk::start(view_as(&host, "brian"));
    let (status, page) = gonk.page();
    assert_eq!(status, 200, "{page}");
    let ours = row(&page, "Our own offer");
    assert!(
        ours.contains("<a href='/l/default/item/12'>ledger #12</a>"),
        "{ours}"
    );
    let theirs = row(&page, "Their offer");
    assert!(!links_to(theirs, "/l/default/item/13"), "{theirs}");

    // With bobby named local (as `gonk.layers.local = "bobby"` would), the same entry links.
    let view = view_as(&host, "brian");
    view.treat_as_local(["urn:collab:participant:bobby".to_string()]);
    let gonk = Gonk::start(view);
    let (_, page) = gonk.page();
    let theirs = row(&page, "Their offer");
    assert!(
        theirs.contains("<a href='/l/default/item/13'>ledger #13</a>"),
        "{theirs}"
    );
    let (_, poll) = gonk.poll();
    assert!(
        links_to(row(&poll, "Their offer"), "/l/default/item/13"),
        "{poll}"
    );
}

/// Brian's rule 4: never a link built from the layer's text. Quotes, markup, `javascript:` and
/// ledger-ish names with `/` or `..` around a held ledger's name are all text.
#[test]
fn hostile_references_are_text() {
    let host = Host::open();
    let hostile = "x ../bobby#1 ..bobby#2 bobby..#3 ../../l/default#4 'bobby'#5 \"crew\"#6 \
                   javascript:bobby#7 <a href='javascript:alert(1)'>click</a> \
                   bobby%2f..#9 ledger #10";
    host.offer("bobby", "9", "Hostile refs", hostile);
    let gonk = Gonk::start(view_as(&host, "brian"));
    let (status, page) = gonk.page();
    assert_eq!(status, 200, "{page}");
    let r = row(&page, "Hostile refs");
    assert!(!r.contains("<a href="), "no link at all: {r}");
    assert!(!r.to_lowercase().contains("href='javascript:"), "{r}");
    assert!(
        r.contains("&lt;a href=&apos;javascript:alert(1)&apos;&gt;"),
        "{r}"
    );
    assert!(
        r.contains("class='foreign-ref'"),
        "the bare one is their ledger's: {r}"
    );
}
