//! A library face (ledger [#3778](http://localhost:1062/l/default/item/3778)), driven over real
//! HTTP the way a browser drives it: `gonk.face = "<name> <directory> library"` is served exactly
//! like any face, but no Add-ons row lists it, neither a face's frame nor gonk's own pages.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use ikigai_gonk::face::{self, Face};
use ikigai_gonk::grants::anonymous_http_grants;
use ikigai_gonk::{compose, config, doors, web};
use ikigai_store::DurableStore;

const GRAPH_JS: &str = "export const graph = true;\n";

/// A page of the face's own with a `<body>` tag, which gonk frames.
fn page(title: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title>\
         </head><body><main>{title}</main></body></html>\n"
    )
}

struct Server {
    addr: SocketAddr,
    _dirs: Vec<tempfile::TempDir>,
}

impl Server {
    /// A gonk HTTP door wired the way `main` wires it, its faces read from `lines` (`gonk.face`
    /// values; `~/ops` and `~/graph` exist under the scratch home). `ops` holds a framed front
    /// page; `graph` holds `graph/graph.js` and a framed `page.html`, and no `index.html`.
    fn start(lines: &[&str]) -> Server {
        let home = tempfile::tempdir().unwrap();
        let ops = home.path().join("ops");
        std::fs::create_dir(&ops).unwrap();
        std::fs::write(ops.join("index.html"), page("Ops")).unwrap();
        let graph = home.path().join("graph");
        std::fs::create_dir_all(graph.join("graph")).unwrap();
        std::fs::write(graph.join("graph").join("graph.js"), GRAPH_JS).unwrap();
        std::fs::write(graph.join("page.html"), page("Graph")).unwrap();
        let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        let faces = face::parse_all(&lines, home.path()).unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config_home = tempfile::tempdir().unwrap();
        let layout = ikigai_gonk::quic::Layout::in_config_home(config_home.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let passkeys = Arc::new(ikigai_gonk::identity::Passkeys::new(layout, addr.port()));
        let pages = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: config::QueuePolicy::default(),
            epochs: None,
            holder_aliases: Default::default(),
            display: Default::default(),
            page_assets: Default::default(),
            projects: Default::default(),
            toolbox: false,
            faces: face::listed(&faces),
            inquiry: None,
        });
        let public: Vec<String> = faces.iter().flat_map(Face::grants).collect();
        let http = Arc::new(doors::http_kernel_serving(
            Arc::clone(&hub),
            web::space(pages),
            face::spaces(&faces),
            None,
        ));
        let door = doors::HttpDoor {
            anonymous: anonymous_http_grants(&["default".to_string()]).unwrap(),
            bind: addr,
            passkeys: Some(passkeys),
            public,
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
            public_origin: None,
        };
        let edge = face::route(doors::edge_config(door.clone()));
        let cap = doors::http_cap(door);
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(http, cap, listener, edge))
        });
        Server {
            addr,
            _dirs: vec![home, config_home],
        }
    }

    /// A person opening `path` by typing its address: its status, its media type and its body.
    fn open(&self, path: &str) -> (u16, String, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\n\
             Accept: text/html,application/xhtml+xml,*/*;q=0.8\r\n\
             Sec-Fetch-Site: same-origin\r\nConnection: close\r\n\r\n",
            port = self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let response = String::from_utf8_lossy(&response).to_string();
        let (head, body) = response.split_once("\r\n\r\n").expect("a head and a body");
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {head}"));
        let media = head
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("content-type")
                    .then(|| v.trim().to_string())
            })
            .unwrap_or_default();
        (status, media, body.to_string())
    }
}

/// The bar gonk writes after a framed page's `<body>` tag.
fn bar(html: &str) -> &str {
    let start = html
        .find("<nav id=\"gonk-frame\"")
        .unwrap_or_else(|| panic!("not framed: {html}"));
    let end = html.find("<main>").expect("the page's own content");
    &html[start..end]
}

/// ★ `ops` listed and `graph` a library: `graph` is served, its file with its media type and its
/// own page framed, and neither a face's bar nor gonk's add-on row links it.
#[test]
fn a_library_face_is_served_but_listed_nowhere() {
    let gonk = Server::start(&["ops ~/ops", "graph ~/graph library"]);

    let (status, media, body) = gonk.open("/face/name/graph/graph/graph.js");
    assert_eq!(status, 200, "{body}");
    assert!(media.starts_with("text/javascript"), "{media}");
    assert_eq!(body, GRAPH_JS);

    // The library has no index.html: its front page is the existing not-found, not a page.
    let (status, _, body) = gonk.open("/face/name/graph/");
    assert_eq!(status, 404, "{body}");

    let (status, _, ops) = gonk.open("/face/name/ops/");
    assert_eq!(status, 200, "{ops}");
    let ops = bar(&ops);
    assert!(
        ops.contains(
            "<nav aria-label=\"Add-ons\"><span>Add-ons</span> <ul>\
             <li><a href=\"/face/name/ops/\" aria-current=\"page\">ops</a></li></ul></nav>"
        ),
        "{ops}"
    );
    assert!(!ops.contains("/face/name/graph/"), "{ops}");

    // The library's own HTML is still framed, and its nav does not link it either.
    let (status, _, graph) = gonk.open("/face/name/graph/page.html");
    assert_eq!(status, 200, "{graph}");
    let graph = bar(&graph);
    assert!(
        graph.contains("<span class=\"gonk-frame-face\">graph</span>"),
        "{graph}"
    );
    assert!(
        graph.contains(
            "<nav aria-label=\"Add-ons\"><span>Add-ons</span> <ul>\
             <li><a href=\"/face/name/ops/\">ops</a></li></ul></nav>"
        ),
        "{graph}"
    );
    assert!(!graph.contains("/face/name/graph/"), "{graph}");

    // gonk's own page: one face-link, for `ops`.
    let (status, _, home) = gonk.open("/");
    assert_eq!(status, 200, "{home}");
    assert!(
        home.contains("<ul class='addons-list'><li><a href='/face/name/ops/'>ops</a></li></ul>"),
        "{home}"
    );
    assert!(!home.contains("/face/name/graph/"), "{home}");
}

/// ★ Every face a library: no Add-ons nav anywhere, not an empty list under a visible heading.
/// The faces are still served, and still framed with gonk's bar.
#[test]
fn only_libraries_leave_no_add_ons_nav() {
    let gonk = Server::start(&["ops ~/ops library", "graph ~/graph library"]);

    for path in ["/face/name/ops/", "/face/name/graph/page.html"] {
        let (status, _, page) = gonk.open(path);
        assert_eq!(status, 200, "{path}: {page}");
        let bar = bar(&page);
        assert!(
            bar.starts_with("<nav id=\"gonk-frame\" aria-label=\"gonk\">"),
            "{bar}"
        );
        assert!(bar.ends_with("add-on</span> </nav>"), "{path}: {bar}");
        assert!(!bar.contains("Add-ons"), "{path}: {bar}");
        assert!(!bar.contains("<ul>"), "{path}: {bar}");
    }

    let (status, _, home) = gonk.open("/");
    assert_eq!(status, 200, "{home}");
    assert!(!home.contains("Add-ons"), "{home}");
    assert!(!home.contains("addons"), "{home}");
    assert!(!home.contains("/face/name/"), "{home}");
}
