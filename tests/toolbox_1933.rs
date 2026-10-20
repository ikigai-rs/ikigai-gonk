//! The RDF Toolbox at `/toolbox` (ledger [#1933](http://localhost:1062/l/default/item/1933)),
//! driven over real HTTP the way a browser drives it: the page and its files serve with their
//! media types and the toolbox's own CSP, an `input:` is read under the caller's capability,
//! and with no `gonk.toolbox.dist` there is no `/toolbox` at all.
//!
//! The build is a scratch directory with stand-ins for the built files and a small compose
//! template of our own, not the real build (an 8 MB engine has no place in a test).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{anonymous_http_grants, toolbox_read_grants};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::toolbox::{self, Toolbox};
use ikigai_gonk::{compose, config, doors, quic, web};
use ikigai_store::DurableStore;

/// A compose template using the real one's markers for the theme, the subject and the inputs.
const TEMPLATE: &str = "<!doctype html>\n\
<html data-theme=\"$h{{theme} || urn:toolbox:text:auto}\">\n\
<body about=\"$h{{iri} || urn:toolbox:text:urn:toolbox:page}\" \
data-inputs=\"[&quot;$h{urn:toolbox:text:/toolbox/input/{iri} || urn:toolbox:none}&quot;]\">\n\
<script type=\"module\" src=\"/toolbox/app.js\"></script>\n\
</body>\n</html>\n";

const APP_JS: &str = "export const toolbox = 1;\n";
const WASM: &[u8] = b"\0asm\x01\0\0\0";

/// A scratch build directory: a few of the allowlisted files, plus one that is NOT on the
/// allowlist (and so not in the grant).
fn build(dir: &Path) {
    std::fs::write(
        dir.join("toolbox.html"),
        "<!doctype html><title>t</title>\n",
    )
    .unwrap();
    std::fs::write(dir.join("page.template.html"), TEMPLATE).unwrap();
    std::fs::write(dir.join("app.js"), APP_JS).unwrap();
    std::fs::write(dir.join("engine.wasm"), WASM).unwrap();
    std::fs::write(dir.join("stray.txt"), "left here by hand\n").unwrap();
}

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    _dirs: Vec<tempfile::TempDir>,
}

impl Server {
    /// A gonk HTTP door whose anonymous caller reads the `default` ledger, with the toolbox
    /// over a scratch build when `with_toolbox`, wired the way `main` wires it.
    fn start(with_toolbox: bool) -> Server {
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
        });
        let toolbox = with_toolbox.then(|| Toolbox::at(dist.path().to_path_buf()).unwrap());
        let mut anonymous = anonymous_http_grants(&["default".to_string()]).unwrap();
        if let Some(toolbox) = &toolbox {
            anonymous.extend(toolbox.grants());
        }
        let faces = toolbox.as_ref().map(Toolbox::spaces).unwrap_or_default();
        let http = Arc::new(doors::http_kernel_serving(
            Arc::clone(&hub),
            web::space(face),
            faces,
            None,
        ));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        let edge = doors::edge_config(door.clone());
        let edge = if with_toolbox {
            toolbox::route(edge)
        } else {
            edge
        };
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

    /// A same-origin GET, as the page's own requests are.
    fn get(&self, path: &str, accept: &str) -> Response {
        self.fetch(path, accept, "Sec-Fetch-Site: same-origin\r\n")
    }

    /// A GET carrying `metadata` (whole header lines) as its fetch metadata.
    fn fetch(&self, path: &str, accept: &str, metadata: &str) -> Response {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: {accept}\r\n\
             {metadata}Connection: close\r\n\r\n",
            port = self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        Response::parse(&response)
    }

    /// File an item in the `default` ledger with root authority, straight on the hub.
    fn file(&self, title: &str) {
        let request = Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
            .with_arg("content", ArgRef::Inline(title.as_bytes().to_vec()));
        futures::executor::block_on(Kernel::issue(&self.hub, request, &Capability::root()))
            .expect("append");
    }

    /// A person opening a page by typing its address.
    fn open(&self, path: &str) -> Response {
        self.get(path, "text/html,application/xhtml+xml,*/*;q=0.8")
    }
}

struct Response {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Response {
    fn parse(response: &[u8]) -> Response {
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a head and a body");
        let head = String::from_utf8_lossy(&response[..split]).to_string();
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {head}"));
        Response {
            status,
            head,
            body: response[split + 4..].to_vec(),
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

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn contains(&self, needle: &[u8]) -> bool {
        self.body.windows(needle.len()).any(|w| w == needle)
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.head, self.text())
    }
}

fn media(response: &Response) -> String {
    response.header("content-type").unwrap_or_default()
}

#[test]
fn the_page_and_its_files_serve_with_their_types_and_the_toolbox_csp() {
    let gonk = Server::start(true);

    let page = gonk.open("/toolbox");
    assert_eq!(page.status, 200, "{page:?}");
    assert!(media(&page).starts_with("text/html"), "{page:?}");
    assert_eq!(
        page.header("content-security-policy").as_deref(),
        Some(ikigai_toolbox::TOOLBOX_CSP),
        "{page:?}"
    );
    assert!(
        page.text().contains(r#"about="urn:toolbox:page""#),
        "{page:?}"
    );
    assert!(page.text().contains(r#"data-theme="auto""#), "{page:?}");

    let js = gonk.get("/toolbox/app.js", "*/*");
    assert_eq!(js.status, 200, "{js:?}");
    assert!(media(&js).starts_with("text/javascript"), "{js:?}");
    assert_eq!(
        js.header("content-security-policy").as_deref(),
        Some(ikigai_toolbox::TOOLBOX_CSP)
    );
    assert!(js.contains(APP_JS.as_bytes()), "{js:?}");

    let wasm = gonk.get("/toolbox/engine.wasm", "*/*");
    assert_eq!(wasm.status, 200, "{wasm:?}");
    assert_eq!(media(&wasm), "application/wasm", "{wasm:?}");
    assert!(wasm.contains(WASM), "the engine's bytes, not a string");

    // The static page too, by its file name.
    let html = gonk.open("/toolbox/toolbox.html");
    assert_eq!(html.status, 200, "{html:?}");
    assert!(media(&html).starts_with("text/html"), "{html:?}");
    assert_eq!(
        html.header("content-security-policy").as_deref(),
        Some(ikigai_toolbox::TOOLBOX_CSP)
    );

    // gonk's own pages keep the door's default policy: the toolbox's is on its routes only.
    let home = gonk.open("/");
    assert_eq!(home.status, 200, "{home:?}");
    let policy = home.header("content-security-policy").expect("a CSP");
    assert_ne!(policy, ikigai_toolbox::TOOLBOX_CSP);
    assert!(!policy.contains("wasm-unsafe-eval"), "{policy}");
}

#[test]
fn a_check_page_names_its_input_and_the_input_is_the_resources_turtle() {
    let gonk = Server::start(true);
    let iri = "urn:iki:ledger:default:items";

    let check = gonk.open("/toolbox/check/urn%3Aiki%3Aledger%3Adefault%3Aitems");
    assert_eq!(check.status, 200, "{check:?}");
    assert_eq!(
        check.header("content-security-policy").as_deref(),
        Some(ikigai_toolbox::TOOLBOX_CSP)
    );
    assert!(
        check.text().contains(&format!(r#"about="{iri}""#)),
        "{check:?}"
    );
    assert!(
        check
            .text()
            .contains("/toolbox/input/urn%3Aiki%3Aledger%3Adefault%3Aitems"),
        "{check:?}"
    );

    gonk.file("A sample item for the toolbox");
    let input = gonk.get("/toolbox/input/urn%3Aiki%3Aledger%3Adefault%3Aitems", "*/*");
    assert_eq!(input.status, 200, "{input:?}");
    assert!(media(&input).starts_with("text/plain"), "{input:?}");
    // The ledger's Turtle face, as text for the page to check.
    assert!(
        input.text().contains("A sample item for the toolbox"),
        "{input:?}"
    );
    assert!(
        input.text().contains("@prefix") || input.text().contains("<urn:"),
        "{input:?}"
    );
}

#[test]
fn an_input_the_caller_cannot_read_is_refused() {
    let gonk = Server::start(true);
    // The anonymous caller reads `default` and nothing else.
    let input = gonk.get("/toolbox/input/urn%3Aiki%3Aledger%3Asecret%3Aitems", "*/*");
    assert_eq!(input.status, 403, "{input:?}");
    // The same resource, asked for directly, is refused the same way: the toolbox reaches
    // nothing its caller could not.
    let direct = gonk.get("/iki/ledger/secret/items", "text/turtle");
    assert_eq!(direct.status, 403, "{direct:?}");
}

#[test]
fn the_build_directory_serves_only_the_toolboxs_files() {
    let gonk = Server::start(true);
    // Not on the allowlist: the family never reads it.
    let stray = gonk.get("/toolbox/stray.txt", "*/*");
    assert_eq!(stray.status, 404, "{stray:?}");
    // By the directory's own name: the grant names the toolbox's files one by one, so a file
    // an operator left in the directory is refused.
    let direct = gonk.get("/iki/toolbox/dist/stray.txt", "*/*");
    assert_eq!(direct.status, 403, "{direct:?}");
    assert!(!direct.text().contains("left here by hand"), "{direct:?}");
}

/// The README's caveat, pinned: a link from another site is a foreign navigation, which keeps
/// only the READ half of a caller's authority (`admit::FOREIGN_PAGE_KEEPS`), and the `fs` read
/// the toolbox's build needs is not in it.
#[test]
fn a_link_from_another_site_is_refused() {
    let gonk = Server::start(true);
    let foreign = gonk.fetch(
        "/toolbox",
        "text/html",
        "Sec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\n",
    );
    assert_eq!(foreign.status, 403, "{foreign:?}");
    let typed = gonk.fetch("/toolbox", "text/html", "Sec-Fetch-Site: none\r\n");
    assert_eq!(typed.status, 200, "{typed:?}");
}

#[test]
fn with_no_config_key_there_is_no_toolbox() {
    let gonk = Server::start(false);
    for path in ["/toolbox", "/toolbox/app.js", "/toolbox/check/urn%3Ax%3Ay"] {
        let answer = gonk.open(path);
        assert_eq!(answer.status, 404, "{path}: {answer:?}");
        assert_ne!(
            answer.header("content-security-policy").as_deref(),
            Some(ikigai_toolbox::TOOLBOX_CSP),
            "{path}"
        );
    }
}

#[test]
fn the_config_key_is_optional_and_checked() {
    let dir = tempfile::tempdir().unwrap();
    let homes = config::Homes {
        home: dir.path().to_path_buf(),
        config: dir.path().join("config"),
        data: dir.path().join("data"),
    };
    let settings = |text: &str| config::settings(&config::Flags::default(), text, &homes);

    assert_eq!(settings("").unwrap().toolbox, None);

    let dist = dir.path().join("dist");
    std::fs::create_dir(&dist).unwrap();
    let configured = settings("gonk.toolbox.dist = \"~/dist\"\n").unwrap();
    assert_eq!(
        configured.toolbox.as_ref().map(|t| t.dist().to_path_buf()),
        Some(dist.clone())
    );
    assert_eq!(
        configured.toolbox.unwrap().grants(),
        toolbox_read_grants(&dist)
    );

    let missing = settings("gonk.toolbox.dist = \"~/nowhere\"\n").unwrap_err();
    assert!(missing.contains(toolbox::KEY), "{missing}");
    assert!(missing.contains("not a directory"), "{missing}");

    let twice =
        settings("gonk.toolbox.dist = \"~/dist\"\ngonk.toolbox.dist = \"~/dist\"\n").unwrap_err();
    assert!(twice.contains("2 times"), "{twice}");
}
