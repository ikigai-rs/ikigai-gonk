//! A deployment's own pages, served from gonk's origin (ledger
//! [#3199](http://localhost:1062/l/default/item/3199)), driven over real HTTP the way a browser
//! drives them: a `gonk.face` directory serves under the door's default CSP, jailed (no `..`,
//! no symbolic link out, no hidden name, no type off the allowlist), read-only, and what the
//! face's page then fetches is judged under the viewer's own grant.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::face::{self, Face};
use ikigai_gonk::grants::anonymous_http_grants;
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, config, doors, quic, web};
use ikigai_store::DurableStore;

const INDEX: &str = "<!doctype html><title>Demo face</title><script src=\"js/app.js\"></script>\n";
const APP_JS: &str = "fetch('/iki/ledger/items');\n";
const SECRET: &str = "outside the face, never served";

/// A scratch face directory, and beside it (`outside`) a secret the face must never reach.
fn build(dir: &Path, outside: &Path) {
    std::fs::write(dir.join("index.html"), INDEX).unwrap();
    std::fs::create_dir(dir.join("js")).unwrap();
    std::fs::write(dir.join("js").join("app.js"), APP_JS).unwrap();
    std::fs::write(dir.join(".env.txt"), SECRET).unwrap();
    std::fs::write(dir.join("notes.key"), SECRET).unwrap();
    std::fs::write(outside.join("secret.html"), SECRET).unwrap();
    std::fs::create_dir(outside.join("sub")).unwrap();
    std::fs::write(outside.join("sub").join("secret.html"), SECRET).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("secret.html"), dir.join("link.html")).unwrap();
        std::os::unix::fs::symlink(outside.join("sub"), dir.join("sub")).unwrap();
    }
}

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    _dirs: Vec<tempfile::TempDir>,
}

impl Server {
    /// A gonk HTTP door whose anonymous caller reads the `default` ledger, with the face `demo`
    /// over a scratch directory when `with_face`, wired the way `main` wires it.
    fn start(with_face: bool) -> Server {
        Server::start_reading(with_face, &["default".to_string()])
    }

    /// The same door, whose anonymous caller reads exactly the ledgers `readable` names.
    fn start_reading(with_face: bool, readable: &[String]) -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dist = tempfile::tempdir().unwrap();
        build(dist.path(), outside.path());
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
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
            faces: if with_face {
                vec!["demo".to_string()]
            } else {
                Vec::new()
            },
        });
        let faces: Vec<Face> = if with_face {
            vec![Face::at("demo", dist.path().to_path_buf()).unwrap()]
        } else {
            Vec::new()
        };
        let mut anonymous = anonymous_http_grants(readable).unwrap();
        for face in &faces {
            anonymous.extend(face.grants());
        }
        let faces = face::spaces(&faces);
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
        let edge = if with_face { face::route(edge) } else { edge };
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
            _dirs: vec![config, dist, outside],
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
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.head, self.text())
    }
}

fn media(response: &Response) -> String {
    response.header("content-type").unwrap_or_default()
}

/// ★ The front page and a file in a subdirectory serve with their media types and the door's
/// DEFAULT policy, the one every gonk page has: `script-src 'self'`, no inline script.
#[test]
fn a_face_serves_under_the_doors_default_csp() {
    let gonk = Server::start(true);
    let page = gonk.open("/face/name/demo");
    assert_eq!(page.status, 200, "{page:?}");
    assert!(media(&page).starts_with("text/html"), "{page:?}");
    assert_eq!(page.text(), INDEX);
    assert_eq!(
        page.header("content-security-policy").as_deref(),
        Some(ikigai_web::HTML_DOOR_CSP),
        "{page:?}"
    );

    let same = gonk.open("/face/name/demo/index.html");
    assert_eq!(same.status, 200, "{same:?}");
    assert_eq!(same.text(), INDEX);

    let script = gonk.get("/face/name/demo/js/app.js", "*/*");
    assert_eq!(script.status, 200, "{script:?}");
    assert!(media(&script).starts_with("text/javascript"), "{script:?}");
    assert_eq!(script.text(), APP_JS);
    assert_eq!(
        script.header("content-security-policy").as_deref(),
        Some(ikigai_web::HTML_DOOR_CSP),
        "{script:?}"
    );
}

/// ★ Path traversal out of the directory is refused, however it is spelled, and the secret
/// beside the directory never reaches the response.
#[test]
fn path_traversal_out_of_the_face_is_refused() {
    let gonk = Server::start(true);
    for path in [
        "/face/name/demo/../secret.html",
        "/face/name/demo/..%2Fsecret.html",
        "/face/name/demo/js/..%2F..%2Fsecret.html",
        "/face/name/demo/%2E%2E/secret.html",
        "/face/name/demo/%2Fetc%2Fhosts.txt",
        "/iki/face/name/demo/dir/..%2Fsecret.html",
    ] {
        let response = gonk.get(path, "*/*");
        assert_ne!(response.status, 200, "{path}: {response:?}");
        assert!(!response.text().contains(SECRET), "{path}: {response:?}");
    }
}

/// ★ A symbolic link inside the directory, to a file or a directory outside it, is refused:
/// ikigai-fs's jail never follows a link, in the jail or out of it.
#[cfg(unix)]
#[test]
fn a_symbolic_link_out_of_the_face_is_refused() {
    let gonk = Server::start(true);
    for path in [
        "/face/name/demo/link.html",
        "/face/name/demo/sub/secret.html",
        "/iki/face/name/demo/dir/link.html",
    ] {
        let response = gonk.get(path, "*/*");
        assert_ne!(response.status, 200, "{path}: {response:?}");
        assert!(!response.text().contains(SECRET), "{path}: {response:?}");
    }
}

/// A hidden name (`.env`, `.git`) and a type off the allowlist are not served, though they
/// are in the directory.
#[test]
fn a_face_serves_no_hidden_name_and_no_type_off_the_allowlist() {
    let gonk = Server::start(true);
    for path in ["/face/name/demo/.env.txt", "/face/name/demo/notes.key"] {
        let response = gonk.get(path, "*/*");
        assert_eq!(response.status, 404, "{path}: {response:?}");
        assert!(!response.text().contains(SECRET), "{path}: {response:?}");
    }
}

/// ★ A face is read-only: the grant it adds is one `fs` READ, and a write to the directory by
/// its own name is refused.
#[test]
fn a_face_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let face = Face::at("demo", dir.path().to_path_buf()).unwrap();
    let grants = face.grants();
    assert_eq!(grants.len(), 1);
    assert!(grants[0].starts_with("urn:cap:fs:read:"), "{grants:?}");

    let gonk = Server::start(true);
    let mut stream = TcpStream::connect(gonk.addr).expect("connect");
    let body = "<p>overwritten</p>";
    let head = format!(
        "PUT /iki/face/name/demo/dir/index.html HTTP/1.1\r\nHost: localhost:{port}\r\n\
         Sec-Fetch-Site: same-origin\r\nContent-Type: text/html\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        port = gonk.addr.port(),
        len = body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let response = Response::parse(&response);
    assert_ne!(response.status / 100, 2, "{response:?}");
    assert_eq!(gonk.open("/face/name/demo").text(), INDEX);
}

/// ★ What a face's page fetches runs under the VIEWER's grant: a viewer who reads the ledger
/// gets it, a viewer who does not is refused, and the face itself loads for both. The face
/// adds no authority of its own.
#[test]
fn a_faces_fetch_is_judged_under_the_viewers_grant() {
    let script = "Sec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\n";

    let reader = Server::start(true);
    reader.file("An item the face reads");
    assert_eq!(reader.open("/face/name/demo").status, 200);
    let read = reader.fetch("/iki/ledger/items", "text/turtle", script);
    assert_eq!(read.status, 200, "{read:?}");
    assert!(read.text().contains("An item the face reads"), "{read:?}");
    // A ledger this viewer does not read is refused through the face as it is by hand.
    let other = reader.fetch("/iki/ledger/secret/items", "text/turtle", script);
    assert_eq!(other.status, 403, "{other:?}");

    let stranger = Server::start_reading(true, &[]);
    stranger.file("An item the stranger may not read");
    assert_eq!(stranger.open("/face/name/demo").status, 200);
    let refused = stranger.fetch("/iki/ledger/items", "text/turtle", script);
    assert_eq!(refused.status, 403, "{refused:?}");
    assert!(!refused.text().contains("may not read"), "{refused:?}");
}

/// With no `gonk.face` line there is no face, and the header offers none.
#[test]
fn no_line_no_face() {
    let gonk = Server::start(false);
    let page = gonk.open("/face/name/demo");
    assert_eq!(page.status, 404, "{page:?}");
    let home = gonk.open("/");
    assert!(!home.text().contains("/face/name/"), "{home:?}");
}

/// The header links each face by name, beside Views and Layers.
#[test]
fn the_header_links_each_face() {
    let gonk = Server::start(true);
    let home = gonk.open("/");
    assert_eq!(home.status, 200, "{home:?}");
    let page = home.text();
    let header = &page[..page.find("</header>").expect("a header")];
    assert!(
        header.contains("<a class='navlink' href='/face/name/demo'>demo</a>"),
        "{header}"
    );
}

/// ★ The config: a missing directory fails loud at start, naming the key; so do a name given
/// twice, a line that is not `<name> <directory>`, and a name that is not a name.
#[test]
fn a_face_line_is_checked_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let homes = config::Homes {
        home: dir.path().to_path_buf(),
        config: dir.path().join("config"),
        data: dir.path().join("data"),
    };
    let settings = |text: &str| config::settings(&config::Flags::default(), text, &homes);

    assert!(settings("").unwrap().faces.is_empty());

    let ops = dir.path().join("ops");
    std::fs::create_dir(&ops).unwrap();
    let configured = settings("gonk.face = \"ops ~/ops\"\n").unwrap();
    assert_eq!(configured.faces.len(), 1);
    assert_eq!(configured.faces[0].name(), "ops");
    assert_eq!(configured.faces[0].dir(), ops.as_path());

    let missing = settings("gonk.face = \"ops ~/nowhere\"\n").unwrap_err();
    assert!(missing.contains(face::KEY), "{missing}");
    assert!(missing.contains("not a directory"), "{missing}");

    let twice = settings("gonk.face = \"ops ~/ops\"\ngonk.face = \"ops ~/ops\"\n").unwrap_err();
    assert!(twice.contains("given twice"), "{twice}");

    let shapeless = settings("gonk.face = \"~/ops\"\n").unwrap_err();
    assert!(shapeless.contains("<name> <directory>"), "{shapeless}");

    let bad_name = settings("gonk.face = \"../ops ~/ops\"\n").unwrap_err();
    assert!(bad_name.contains("not a face name"), "{bad_name}");
}
