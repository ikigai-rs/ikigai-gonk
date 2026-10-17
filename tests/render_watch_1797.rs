//! Ledger [#1797](http://localhost:1062/l/default/item/1797): can a page on another port make
//! `urn:iki:gonk:render` draw a gonk page around a `view:watch` it chose?
//!
//! The claim, read from the code: the resource requires nothing and renders whatever envelope
//! it is handed, so a document saying `full="true"` comes back as a whole gonk page, the shell
//! that loads `/static/htmx.min.js`, around a `view:watch` whose `@url` and `@trigger` the
//! CALLER chose. Opened in a window, that page is gonk's own origin, so the
//! `hx-trigger="load"` GET it fires would be same-origin and carry the identity's whole grant,
//! which is what ledger #880 took away from a page on another port
//! (`tests/cross_site_gets_880.rs`: such a page may navigate here with only the READ half of
//! the grant, and gonk's own page still derives).
//!
//! ★ Measured on `f169a5c`: the renderer does draw that page for a caller who can hand it the
//! document (the payload check below), but **no GET carries one**, for two reasons that live
//! outside this file and that these tests pin:
//!
//! - **`/k` splits its command on whitespace** (`k::parse_command`, browse's grammar), and an
//!   XML attribute needs whitespace before it (xrust's `whitespace1` is exactly space, tab, CR
//!   and LF, all of which `split_whitespace` splits on). So `content=` through `/k` can carry
//!   no attribute at all: no `full`, no `view`, no namespace declaration. The planted page is
//!   refused as `a command's arguments are key=value`.
//! - **The mechanical mapping does not forward `content` from a query** (`ikigai-web`'s
//!   `forwarded_query` reserves it as the adapter's own; only a Sink's BODY becomes `content`).
//!   So `/iki/gonk/render?content=…` reaches the resource with no document and is refused as
//!   a missing argument. A Sink cannot reach it either: it answers Source only, and a POST from
//!   another page is refused at the edge.
//!
//! The socket and QUIC doors can hand it any document, but their callers are processes, not
//! browsers: the HTML goes back to the caller and no cookie rides anything it says. Every
//! caller inside gonk builds its document with `render::chunk` (`view="chunk"`, no shell).
//!
//! If either transport fact changes (`/k` grows quoting, or the library forwards `content` on a
//! read), the first test fails, and the fix is to render only the chunk envelope.
//!
//! Nothing here touches a live gonk: the server binds `127.0.0.1:0` over a tempdir config home.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::render;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

struct Server {
    addr: SocketAddr,
    _config: tempfile::TempDir,
}

fn server() -> Server {
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let passkeys = Arc::new(Passkeys::new(
        quic::Layout::in_config_home(config.path()),
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
    });
    let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
    let door = doors::HttpDoor {
        anonymous: grants_for("default", Authority::Write).unwrap(),
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
    Server {
        addr,
        _config: config,
    }
}

impl Server {
    /// A page on another localhost port navigating the window here (`location = …`): the one
    /// way ledger #880 leaves such a page to reach gonk.
    fn navigate_from_another_page(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html\r\n\
             Sec-Fetch-Site: same-site\r\nSec-Fetch-Mode: navigate\r\n\
             Sec-Fetch-Dest: document\r\nReferer: http://localhost:8090/innocent.html\r\n\
             Connection: close\r\n\r\n",
            self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let response = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line for GET {path}: {response:?}"));
        (status, body.to_string())
    }
}

fn encoded(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The route the planted watch asks for: a derivation, which ledger #880 refuses to another
/// page and grants to gonk's own.
const PLANTED: &str = "/k?c=source urn:repo:demo:explain:README.md";

/// A whole gonk page around a watch whose URL and trigger the caller chose.
fn planted_page() -> String {
    render::envelope(
        "page",
        &[("view", "layers"), ("full", "true"), ("title", "Layers")],
        &format!("<view:watch url=\"{PLANTED}\" trigger=\"load\"/>"),
    )
}

/// What a browser would act on: a script that loads htmx, or an element that issues a request.
fn would_fire(body: &str) -> bool {
    body.contains("htmx.min.js") || body.contains("hx-get") || body.contains("hx-trigger")
}

/// The planted document is a real payload: given to the renderer, it draws a page a browser
/// would act on. Without this the guard below could pass on a document that never fired.
#[test]
fn the_planted_document_would_fire_if_it_reached_the_renderer() {
    let page = render::render(&planted_page(), true).expect("the stylesheet renders it");
    assert!(page.contains("/static/htmx.min.js"), "{page}");
    assert!(page.contains(&format!("hx-get='{PLANTED}'")), "{page}");
    assert!(page.contains("hx-trigger='load'"), "{page}");
}

/// The guard: another page's navigation, by either path a GET has to the resource, is refused
/// before the renderer sees the document, and for the reason stated above.
#[test]
fn another_pages_navigation_cannot_render_a_page_that_requests_a_route_it_chose() {
    let server = server();
    let content = planted_page();
    for (path, why) in [
        (
            format!("/iki/gonk/render?content={}", encoded(&content)),
            "missing required argument `content`",
        ),
        (
            format!(
                "/k?c={}",
                encoded(&format!("source urn:iki:gonk:render content={content}"))
            ),
            "a command's arguments are `key=value`",
        ),
    ] {
        let (status, body) = server.navigate_from_another_page(&path);
        assert!(
            status == 400 && body.contains(why) && !would_fire(&body),
            "GET {path} answered {status}, expected 400 naming {why:?}: {body}"
        );
    }
}
