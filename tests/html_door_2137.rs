//! Ledger [#2137](http://localhost:1062/l/default/item/2137) (FND-H's follow-on, #1813, #1958
//! point 3): the HTTP door sends `ikigai-web`'s page preset, [`SecurityHeaders::html_door`],
//! not the edge default.
//!
//! The preset is the default plus `script-src 'self'`, `style-src 'self'` (no
//! `'unsafe-inline'` in either), `object-src 'none'` and `Cross-Origin-Opener-Policy:
//! same-origin`. Before this, gonk's pages went out under the default — `default-src 'self';
//! frame-ancestors 'none'; base-uri 'none'; form-action 'self'` and no opener policy — and
//! every assertion on the preset below failed against it.
//!
//! The two rows with a CSP of their own keep it: a layer file's sandbox
//! ([`ikigai_gonk::layers_view::FILE_CSP`]) and the RDF Toolbox's
//! ([`ikigai_toolbox::TOOLBOX_CSP`], which the toolbox's WebAssembly worker needs). A row's
//! `csp` REPLACES the door's on that row alone, so the preset must neither leak onto them nor
//! be loosened by them onto the rest.
//!
//! The browser pass that preceded this (every page below, and the passkey enrollment and
//! sign-in through a virtual authenticator, in Chromium, before and after) reported no CSP
//! violation; it is described in the pull request, not repeated here.
//!
//! [`SecurityHeaders::html_door`]: ikigai_web::SecurityHeaders::html_door

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::anonymous_http_grants;
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::layers_view::FILE_CSP;
use ikigai_gonk::toolbox::{self, Toolbox};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;
use ikigai_toolbox::TOOLBOX_CSP;
use ikigai_web::HTML_DOOR_CSP;

/// The page preset's lines other than the CSP, which every row carries whatever its CSP.
const COMPANIONS: [(&str, &str); 3] = [
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("cross-origin-opener-policy", "same-origin"),
];

/// A scratch toolbox build: just enough for its rows to answer.
fn build(dir: &Path) {
    std::fs::write(
        dir.join("toolbox.html"),
        "<!doctype html><title>t</title>\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("page.template.html"),
        "<!doctype html>\n<html><body></body></html>\n",
    )
    .unwrap();
    std::fs::write(dir.join("app.js"), "export const toolbox = 1;\n").unwrap();
}

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    _dirs: Vec<tempfile::TempDir>,
}

impl Server {
    /// The HTTP door wired the way `main` wires it — [`doors::edge_config`], then the toolbox's
    /// rows — with an anonymous caller that reads the `default` ledger.
    fn start() -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let dist = tempfile::tempdir().unwrap();
        build(dist.path());
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
            toolbox: true,
        });
        let toolbox = Toolbox::at(dist.path().to_path_buf()).unwrap();
        let mut anonymous = anonymous_http_grants(&["default".to_string()]).unwrap();
        anonymous.extend(toolbox.grants());
        let http = Arc::new(doors::http_kernel_serving(
            Arc::clone(&hub),
            web::space(face),
            toolbox.spaces(),
            None,
        ));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        let edge = toolbox::route(doors::edge_config(door.clone()));
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door),
                listener,
                edge,
            ))
        });
        Server {
            addr,
            hub,
            _dirs: vec![config, dist],
        }
    }

    /// File an item in the `default` ledger with root authority, straight on the hub.
    fn file(&self, title: &str) {
        let request = Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
            .with_arg("content", ArgRef::Inline(title.as_bytes().to_vec()));
        futures::executor::block_on(Kernel::issue(&self.hub, request, &Capability::root()))
            .expect("append");
    }

    /// A person opening `path` by typing its address. Returns the status and the head.
    fn open(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\n\
             Accept: text/html,application/xhtml+xml,*/*;q=0.8\r\n\
             Sec-Fetch-Site: none\r\nSec-Fetch-Mode: navigate\r\nConnection: close\r\n\r\n",
            port = self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let response = String::from_utf8_lossy(&response);
        let head = response.split("\r\n\r\n").next().unwrap_or_default();
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {head}"));
        (status, head.to_string())
    }
}

/// Every value `head` carries for header `name`, so a header sent twice is seen twice.
fn headers(head: &str, name: &str) -> Vec<String> {
    head.lines()
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
        .collect()
}

/// `head` carries exactly one `Content-Security-Policy`, `csp`, and the preset's companions.
fn assert_door_headers(path: &str, head: &str, csp: &str) {
    assert_eq!(
        headers(head, "content-security-policy"),
        [csp],
        "{path}: one CSP, this one\n{head}"
    );
    for (name, value) in COMPANIONS {
        assert_eq!(headers(head, name), [value], "{path}: {name}\n{head}");
    }
}

#[test]
fn the_preset_is_the_one_ikigai_web_names() {
    // What this door now promises a browser, spelled out, so a library that changes its
    // preset is noticed here and not only in a browser.
    assert_eq!(
        HTML_DOOR_CSP,
        "default-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; \
         base-uri 'none'; frame-ancestors 'none'; form-action 'self'"
    );
    let config = doors::edge_config(doors::HttpDoor {
        anonymous: Vec::new(),
        bind: "127.0.0.1:1062".parse().unwrap(),
        passkeys: None,
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    });
    let security = config.security.expect("the door sends security headers");
    assert_eq!(security.csp.as_deref(), Some(HTML_DOOR_CSP));
    assert_eq!(security.coop.as_deref(), Some("same-origin"));
    assert!(security.nosniff);
    assert_eq!(security.referrer_policy.as_deref(), Some("no-referrer"));
}

#[test]
fn the_html_routes_carry_the_page_preset() {
    let gonk = Server::start();
    gonk.file("An item to open");
    for (path, status) in [
        ("/", 200),
        ("/l", 200),
        ("/l/default", 200),
        ("/l/default/item/1", 200),
        ("/queue", 200),
        ("/browse", 200),
        ("/sparql", 200),
        // 404 only because this harness binds neither resource; `main` binds pages that say
        // how to configure them. The policy is the row's either way.
        ("/layers", 404),
        ("/onboard", 404),
        // A refusal and a miss are pages too, and go out under the same policy.
        ("/views", 403),
        ("/l/default/item/999", 404),
        // The page's own files.
        ("/static/gonk.js", 200),
        ("/static/gonk.css", 200),
    ] {
        let (got, head) = gonk.open(path);
        assert_eq!(got, status, "{path}\n{head}");
        assert_door_headers(path, &head, HTML_DOOR_CSP);
    }
}

#[test]
fn a_row_with_its_own_csp_replaces_the_preset_on_that_row_alone() {
    let gonk = Server::start();

    // The toolbox's rows: its own CSP (it compiles WebAssembly in a worker), not the preset's.
    for path in ["/toolbox", "/toolbox/app.js", "/toolbox/toolbox.html"] {
        let (status, head) = gonk.open(path);
        assert_eq!(status, 200, "{path}\n{head}");
        assert_door_headers(path, &head, TOOLBOX_CSP);
    }

    // A layer file's row: the sandbox, whatever the answer. This door has no Layers view
    // configured, so the file is not served — and its refusal still goes out sandboxed, as
    // the bytes would (tests/layers_page_1675.rs serves one and pins the same header).
    let path = "/layers/talk/file/0123456789abcdef";
    let (status, head) = gonk.open(path);
    assert!(status >= 400, "{path}: {status}\n{head}");
    assert_door_headers(path, &head, FILE_CSP);

    // And neither leaks back: the page beside them is under the preset.
    let (_, head) = gonk.open("/layers");
    assert_door_headers("/layers", &head, HTML_DOOR_CSP);
}
