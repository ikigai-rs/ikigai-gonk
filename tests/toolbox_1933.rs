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

mod common;

use common::Authenticator;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{anonymous_http_grants, grants_for, toolbox_read_grants, Authority};
use ikigai_gonk::identity::{self, Passkeys};
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
    layout: quic::Layout,
    _dirs: Vec<tempfile::TempDir>,
}

impl Server {
    /// A gonk HTTP door whose anonymous caller reads the `default` ledger, with the toolbox
    /// over a scratch build when `with_toolbox`, wired the way `main` wires it.
    fn start(with_toolbox: bool) -> Server {
        Server::start_reading(with_toolbox, &["default".to_string()])
    }

    /// The same door, whose anonymous caller reads exactly the ledgers `readable` names.
    fn start_reading(with_toolbox: bool, readable: &[String]) -> Server {
        Server::start_from(with_toolbox, readable, None)
    }

    /// The same door, as a caller on ANOTHER MACHINE reaches it: every request's capability
    /// is computed with a non-loopback peer in place of the test connection's loopback one.
    /// The transport, the edge and the cookie are the real ones (ledger #3202).
    fn start_remote(with_toolbox: bool, readable: &[String]) -> Server {
        Server::start_from(with_toolbox, readable, Some("192.0.2.7".parse().unwrap()))
    }

    fn start_from(
        with_toolbox: bool,
        readable: &[String],
        peer: Option<std::net::IpAddr>,
    ) -> Server {
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
            toolbox: with_toolbox,
            faces: Vec::new(),
        });
        let toolbox = with_toolbox.then(|| Toolbox::at(dist.path().to_path_buf()).unwrap());
        let anonymous = anonymous_http_grants(readable).unwrap();
        let public = toolbox.as_ref().map(Toolbox::grants).unwrap_or_default();
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
            public,
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        let edge = doors::edge_config(door.clone());
        let edge = if with_toolbox {
            toolbox::route(edge)
        } else {
            edge
        };
        let cap = doors::http_cap(door);
        let cap: ikigai_web::CapFn = match peer {
            None => cap,
            Some(peer) => Arc::new(move |request: &ikigai_web::HttpRequest| {
                cap(&ikigai_web::HttpRequest {
                    method: request.method.clone(),
                    path: request.path.clone(),
                    query: request.query.clone(),
                    headers: request.headers.clone(),
                    body: request.body.clone(),
                    peer: Some(peer),
                })
            }),
        };
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(http, cap, listener, edge))
        });
        Server {
            addr,
            hub,
            layout,
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

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    /// A same-origin JSON POST, as the passkey ceremonies send.
    fn post_json(&self, path: &str, body: &str) -> Response {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: application/json\r\n\
             Content-Type: application/json\r\nOrigin: {origin}\r\nSec-Fetch-Site: same-origin\r\n\
             Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            port = self.addr.port(),
            origin = self.origin(),
            len = body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        Response::parse(&response)
    }

    /// Enroll a passkey under a grant that READS the `default` ledger and nothing else, sign
    /// in over HTTP, and return the session token.
    fn sign_in(&self) -> String {
        let authenticator = Authenticator::new();
        let invite = identity::invite(
            &self.layout,
            "person",
            &grants_for("default", Authority::Read).unwrap(),
            false,
            30,
            identity::now_seconds(),
        )
        .unwrap();
        let challenge = |op: &str| {
            let answer = self.post_json(&format!("/auth/{op}"), "{}");
            assert_eq!(answer.status, 200, "{answer:?}");
            let v: serde_json::Value =
                serde_json::from_slice(&answer.body).expect("a JSON challenge");
            v["challenge"].as_str().unwrap().to_string()
        };
        let c = challenge("register-options");
        let registered = self.post_json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(registered.status, 200, "{registered:?}");
        let c = challenge("login-options");
        let login = self.post_json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        );
        assert_eq!(login.status, 200, "{login:?}");
        let v: serde_json::Value = serde_json::from_slice(&login.body).expect("a JSON session");
        v["session"].as_str().unwrap().to_string()
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

    // gonk's own pages keep the door's policy (the page preset, ledger #2137): the toolbox's
    // is on its routes only.
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

/// ★ **`/toolbox` opens on OUR data** (ledger [#2033](http://localhost:1062/l/default/item/2033)):
/// a caller who may read the default ledger gets the check page over its items, which names the
/// items' Turtle face as its input, instead of the empty page and turtle-lsp's samples.
#[test]
fn the_toolbox_opens_on_the_default_ledgers_items() {
    let gonk = Server::start(true);
    gonk.file("An item the toolbox opens on");

    let page = gonk.open("/toolbox");
    assert_eq!(page.status, 200, "{page:?}");
    assert_eq!(
        page.header("content-security-policy").as_deref(),
        Some(ikigai_toolbox::TOOLBOX_CSP),
        "{page:?}"
    );
    assert!(
        page.text().contains(r#"about="urn:iki:ledger:items""#),
        "{page:?}"
    );
    assert!(
        page.text()
            .contains("/toolbox/input/urn%3Aiki%3Aledger%3Aitems"),
        "{page:?}"
    );

    // And that input is the ledger's Turtle, readable by the same caller.
    let input = gonk.get("/toolbox/input/urn%3Aiki%3Aledger%3Aitems", "*/*");
    assert_eq!(input.status, 200, "{input:?}");
    assert!(
        input.text().contains("An item the toolbox opens on"),
        "{input:?}"
    );

    // `theme=` passes through to the page it opens.
    let dark = gonk.open("/toolbox?theme=dark");
    assert_eq!(dark.status, 200, "{dark:?}");
    assert!(dark.text().contains(r#"data-theme="dark""#), "{dark:?}");
    assert!(
        dark.text().contains(r#"about="urn:iki:ledger:items""#),
        "{dark:?}"
    );

    // The empty toolbox, turtle-lsp's samples, is still one name away.
    let empty = gonk.open("/toolbox/page");
    assert_eq!(empty.status, 200, "{empty:?}");
    assert_eq!(
        empty.header("content-security-policy").as_deref(),
        Some(ikigai_toolbox::TOOLBOX_CSP)
    );
    assert!(
        empty.text().contains(r#"about="urn:toolbox:page""#),
        "{empty:?}"
    );
    assert!(!empty.text().contains("/toolbox/input/"), "{empty:?}");
}

/// A caller who may NOT read the default ledger still gets a toolbox: the empty page, which
/// loads the samples, never an error page and never a check page whose one input would fail.
#[test]
fn a_caller_without_the_ledger_read_gets_the_samples() {
    let gonk = Server::start_reading(true, &[]);

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
    assert!(!page.text().contains("/toolbox/input/"), "{page:?}");

    // The read it lacks, for the record: the input it would have named is refused.
    let input = gonk.get("/toolbox/input/urn%3Aiki%3Aledger%3Aitems", "*/*");
    assert_eq!(input.status, 403, "{input:?}");

    // A check page by hand is unchanged: it composes, and its input is what is refused.
    let check = gonk.open("/toolbox/check/urn%3Aiki%3Aledger%3Aitems");
    assert_eq!(check.status, 200, "{check:?}");
    assert!(
        check.text().contains(r#"about="urn:iki:ledger:items""#),
        "{check:?}"
    );
}

/// ★ Ledger [#3202](http://localhost:1062/l/default/item/3202): a viewer signed in from ANOTHER
/// machine loads the toolbox (its page, its script, its engine) under their passkey session,
/// with nothing copied into their grant by hand. Its input is still read under that session's
/// own grant, and a caller from another machine with no session gets nothing.
#[test]
fn the_toolbox_loads_for_a_viewer_signed_in_from_another_machine() {
    // The anonymous grant reads nothing, so whatever the session reads is its own grant's.
    let gonk = Server::start_remote(true, &[]);
    gonk.file("An item the toolbox reads");

    // No session: a caller from another machine is refused the toolbox's files.
    let js = gonk.get("/toolbox/app.js", "*/*");
    assert_eq!(js.status, 403, "{js:?}");
    assert!(!js.contains(APP_JS.as_bytes()), "{js:?}");
    let page = gonk.open("/toolbox");
    assert_ne!(page.status, 200, "{page:?}");

    let session = gonk.sign_in();
    let cookie = format!("Cookie: gonk_session={session}\r\n");
    let same = format!("Sec-Fetch-Site: same-origin\r\n{cookie}");
    let page = gonk.fetch("/toolbox", "text/html", &same);
    assert_eq!(page.status, 200, "{page:?}");
    assert!(media(&page).starts_with("text/html"), "{page:?}");
    let js = gonk.fetch("/toolbox/app.js", "*/*", &same);
    assert_eq!(js.status, 200, "{js:?}");
    assert!(js.contains(APP_JS.as_bytes()), "{js:?}");
    let wasm = gonk.fetch("/toolbox/engine.wasm", "*/*", &same);
    assert_eq!(wasm.status, 200, "{wasm:?}");
    assert!(wasm.contains(WASM), "the engine's bytes");

    // Its inputs: the session's ledger reads, another ledger stays refused.
    let input = gonk.fetch(
        "/toolbox/input/urn%3Aiki%3Aledger%3Adefault%3Aitems",
        "*/*",
        &same,
    );
    assert_eq!(input.status, 200, "{input:?}");
    assert!(input.contains(b"An item the toolbox reads"), "{input:?}");
    let secret = gonk.fetch(
        "/toolbox/input/urn%3Aiki%3Aledger%3Asecret%3Aitems",
        "*/*",
        &same,
    );
    assert_eq!(secret.status, 403, "{secret:?}");
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

/// ★ **The header offers the toolbox exactly when there is one** (ledger
/// [#2021](http://localhost:1062/l/default/item/2021)): through #1933 `/toolbox` worked and no
/// page linked to it. A `gonk.toolbox.dist` line puts a "Toolbox" entry in every page's header,
/// an ordinary link to `/toolbox`; no line, no entry, so nobody is offered a door that answers
/// `404`.
#[test]
fn the_header_offers_the_toolbox_only_when_the_key_is_set() {
    let entry = "<a class='navlink' href='/toolbox'>Toolbox</a>";
    let with = Server::start(true);
    let home = with.open("/");
    assert_eq!(home.status, 200, "{home:?}");
    let page = home.text();
    let header = &page[..page.find("</header>").expect("a header")];
    assert!(header.contains(entry), "{header}");
    // And it leads somewhere: the toolbox page itself.
    assert_eq!(with.open("/toolbox").status, 200);

    let without = Server::start(false);
    let home = without.open("/");
    assert_eq!(home.status, 200, "{home:?}");
    assert!(!home.text().contains("Toolbox"), "{home:?}");
    assert!(!home.text().contains("href='/toolbox'"), "{home:?}");
}
