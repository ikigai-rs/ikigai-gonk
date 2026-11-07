//! `cargo run --example axe_site -- <out-dir>` — gonk's anonymous pages as a static site, for
//! `ikigai-ui`'s axe checker and its reusable `axe.yml` workflow (UI conventions D18, ledger #2834).
//!
//! It serves the pages through the same HTTP door `main` wires, reads them as a browser would and
//! writes them with [`ikigai_ui::testkit::site`], each under a name ending in `.html`, with the
//! files they link at the paths they link them. The checker opens every `.html` file under a
//! same-origin CSP and fails on an axe violation, a page error or a console error, so an
//! SRI value or a script the CSP would refuse is a failure here too.
//!
//! ⚠ Anonymous only: a static site has no sign-in, and a signed-in header polls paths a file
//! server cannot answer. `ACCESSIBILITY.md` says what covers the signed-in pages.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::anonymous_http_grants;
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose_with, doors, git_object, quic, web};
use ikigai_store::DurableStore;

const PAGES: [(&str, &str); 17] = [
    ("home.html", "/"),
    ("ledgers.html", "/l"),
    ("dashboard.html", "/dashboard"),
    ("ledger.html", "/l/default"),
    (
        "label-filter.html",
        "/l/default?labels=needs-person&without=blocked",
    ),
    (
        "label-empty.html",
        "/l/default?labels=missing&without=blocked",
    ),
    ("closed.html", "/l/default?status=closed"),
    ("item.html", "/l/default/item/1"),
    ("linked-item.html", "/l/default/item/4"),
    ("named-ledger.html", "/l/crew"),
    // A `ledgers=` union (#3216): rows from two ledgers, and a named one left out unread.
    (
        "ledger-union.html",
        "/l/default?ledgers=default,crew,secret",
    ),
    ("named-item.html", "/l/crew/item/1"),
    ("lifecycle.html", "/l/default/lifecycle"),
    ("doctor.html", "/l/default/doctor"),
    ("sparql.html", "/sparql"),
    ("browse.html", "/browse"),
    ("queue.html", "/queue"),
];

const FILES: [(&str, &str); 5] = [
    ("static/tokens.css", "text/css"),
    ("static/gonk.css", "text/css"),
    ("static/shell.js", "text/javascript"),
    ("static/htmx.min.js", "text/javascript"),
    ("static/gonk.js", "text/javascript"),
];

/// A browser's GET of `path`: its status and its body.
fn get(addr: SocketAddr, path: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: text/html,*/*;q=0.8\r\n\
         Sec-Fetch-Site: none\r\nSec-Fetch-Mode: navigate\r\nConnection: close\r\n\r\n",
        port = addr.port()
    );
    stream.write_all(head.as_bytes()).expect("write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read");
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a head");
    let status = String::from_utf8_lossy(&response[..split])
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    (status, response[split + 4..].to_vec())
}

fn appended_iri(bytes: &[u8], ledger: &str) -> String {
    let text = String::from_utf8_lossy(bytes);
    let prefix = format!("urn:iki:ledger:{ledger}:item:");
    let id: String = text
        .split(&prefix)
        .nth(1)
        .expect("the append names its item")
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    format!("{prefix}{id}")
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("usage: axe_site <out-dir>"));
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("a loopback port");
    let addr = listener.local_addr().expect("its address");
    let config = tempfile::tempdir().expect("a scratch config home");
    let layout = quic::Layout::in_config_home(config.path());
    // Git evidence (#3198): a local clone, never a network read, and the real map resources.
    let repository = config.path().join("repository");
    std::fs::create_dir(&repository).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    std::fs::write(
        repository.join("example.rs"),
        "// <code> & evidence\nfn main() {}\n",
    )
    .unwrap();
    git(&["add", "example.rs"]);
    git(&["commit", "-qm", "Accessibility fixture"]);
    let commit = git(&["rev-parse", "HEAD"]);
    let blob = git(&["rev-parse", "HEAD:example.rs"]);
    let repositories = vec![git_object::Repository {
        name: "fixture".into(),
        dir: repository,
        commit: Some("https://code.example/commit/{sha}".into()),
        blob: Some("https://code.example/blob/{commit}/{path}".into()),
        browse: true,
    }];
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().expect("a store"),
        None,
        Vec::new(),
        vec![Arc::new(git_object::space("configured", &repositories))],
        None,
        None,
        None,
    ));
    let object_html = block_on(Kernel::issue(
        &hub,
        Request::new(
            Verb::Source,
            Iri::parse(git_object::object_iri("configured", "fixture", &blob)).unwrap(),
        )
        .with_arg("as", ArgRef::Inline(b"text/html".to_vec())),
        &Capability::root(),
    ))
    .unwrap()
    .bytes;
    block_on(Kernel::issue(
        &hub,
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:append").expect("an IRI"),
        )
        .with_arg(
            "content",
            ArgRef::Inline(b"An item for the checker to read".to_vec()),
        )
        .with_arg(
            "labels",
            ArgRef::Inline(b"needs-person,book:ikigai".to_vec()),
        ),
        &Capability::root(),
    ))
    .expect("the scratch item");
    let crew = block_on(Kernel::issue(
        &hub,
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:crew:append").expect("an IRI"),
        )
        .with_arg("content", ArgRef::Inline(b"A crew item".to_vec())),
        &Capability::root(),
    ))
    .expect("the named-ledger scratch item");
    // Ledger #3195: rows whose badges carry the hub's own facts, so axe reads every one in
    // every theme: an in-flight state held by a claim whose lease has run out, and an item
    // handed back to its dispatcher. Written into the store, because a lease cannot be issued
    // already expired and a scratch site should not sleep to see one end.
    let mut seeded = Vec::new();
    for title in [
        "An item in flight under an expired lease",
        "An item handed back",
    ] {
        let answer = block_on(Kernel::issue(
            &hub,
            Request::new(
                Verb::Sink,
                Iri::parse("urn:iki:ledger:append").expect("an IRI"),
            )
            .with_arg("content", ArgRef::Inline(title.as_bytes().to_vec())),
            &Capability::root(),
        ))
        .expect("a badge scratch item");
        let text = String::from_utf8_lossy(&answer.bytes).into_owned();
        let prefix = "urn:iki:ledger:default:item:";
        let at = text.find(prefix).expect("the append names its item");
        let id: String = text[at + prefix.len()..]
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        seeded.push(format!("{prefix}{id}"));
    }
    let facts = format!(
        "@prefix ledger: <https://ikigai-rs.dev/ns/ledger#> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
         <{flying}> ledger:state <urn:iki:ledger:lifecycle:kata-flight:resolving> ; \
         ledger:claimedBy \"w1\" ; ledger:claimKind ledger:machine ; ledger:lease \"30m\" ; \
         ledger:leaseExpires \"2001-01-01T00:00:00.000Z\"^^xsd:dateTime ; ledger:deferred true .\n\
         <{waiting}> ledger:awaiting <urn:iki:ledger:awaiting:dispatcher> .\n",
        flying = seeded[0],
        waiting = seeded[1],
    );
    block_on(Kernel::issue(
        &hub,
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:store:load").expect("an IRI"),
        )
        .with_arg("content", ArgRef::Inline(facts.into_bytes()))
        .with_arg("format", ArgRef::Inline(b"text/turtle".to_vec()))
        .with_arg(
            "graph",
            ArgRef::Inline(b"urn:iki:ledger:graph:default".to_vec()),
        ),
        &Capability::root(),
    ))
    .expect("the badge facts");
    // #3197: an outgoing flight plan with live badges, another readable ledger,
    // a self-link and unavailable/external IRIs. No inbound links are introduced.
    let record = block_on(Kernel::issue(
        &hub,
        Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
            .with_arg("content", ArgRef::Inline(b"A live flight plan".to_vec())),
        &Capability::root(),
    ))
    .expect("the flight plan");
    let record = appended_iri(&record.bytes, "default");
    let crew = appended_iri(&crew.bytes, "crew");
    let links = format!(
        "@prefix ledger: <https://ikigai-rs.dev/ns/ledger#> .\n\
         <{record}> ledger:related <{flying}>, <{waiting}>, <{crew}> ; \
         ledger:parent <{record}> ; ledger:blocks <urn:iki:ledger:private:item:01unreadable> ; \
         ledger:about <urn:external:record:abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz>, <urn:git:commit:{commit}>, <urn:git:blob:{blob}> .",
        flying = seeded[0], waiting = seeded[1],
    );
    block_on(Kernel::issue(
        &hub,
        Request::new(Verb::Sink, Iri::parse("urn:iki:store:load").unwrap())
            .with_arg("content", ArgRef::Inline(links.into_bytes()))
            .with_arg("format", ArgRef::Inline(b"text/turtle".to_vec()))
            .with_arg(
                "graph",
                ArgRef::Inline(b"urn:iki:ledger:graph:default".to_vec()),
            ),
        &Capability::root(),
    ))
    .expect("the outgoing flight-plan links");
    let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string(), "crew".to_string()],
        browse_roots: Vec::new().into(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        toolbox: false,
        faces: Vec::new(),
    });
    let http = Arc::new(doors::http_kernel_serving(
        hub,
        web::space(face),
        Vec::new(),
        None,
    ));
    let door = doors::HttpDoor {
        anonymous: anonymous_http_grants(&["default".to_string(), "crew".to_string()])
            .expect("the anonymous grant")
            .into_iter()
            .chain([
                "urn:cap:exec:git".to_string(),
                "urn:cap:browse:read:fixture".to_string(),
            ])
            .collect(),
        bind: addr,
        passkeys: Some(passkeys),
        public: Vec::new(),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    };
    let edge = doors::edge_config(door.clone());
    std::thread::spawn(move || {
        runtime.block_on(ikigai_web::serve_with_listener(
            http,
            doors::http_cap(door),
            listener,
            edge,
        ))
    });

    let mut bodies: Vec<(String, &str, Vec<u8>)> = Vec::new();
    for (name, path) in PAGES {
        let (status, body) = get(addr, path);
        assert_eq!(status, 200, "{path}");
        bodies.push((name.to_string(), "text/html; charset=utf-8", body));
    }
    // The local object fragment in the same page shell and shared tokens as the item.
    let (_, shell) = get(addr, "/l/default/item/1");
    let object_page = String::from_utf8(shell).unwrap().replace(
        "</main>",
        &format!("{}</main>", String::from_utf8(object_html).unwrap()),
    );
    bodies.push((
        "git-object.html".into(),
        "text/html; charset=utf-8",
        object_page.into_bytes(),
    ));
    // Independently loaded whole-match counts. The route is also exported so CI's
    // static server can answer the pages' lazy request (queries do not change a11y).
    for (name, path) in [
        ("l/default/facet", "/l/default/facet"),
        ("l/crew/facet", "/l/crew/facet"),
        ("facet.fragment", "/l/default/facet"),
        ("facet-empty.fragment", "/l/default/facet?labels=missing"),
        ("facet-page.html", "/l/default/facet?page=true"),
    ] {
        let (status, body) = get(addr, path);
        assert_eq!(status, 200, "{path}");
        bodies.push((name.to_string(), "text/html; charset=utf-8", body));
    }
    for (name, media) in FILES {
        let (status, body) = get(addr, &format!("/{name}"));
        assert_eq!(status, 200, "/{name}");
        bodies.push((name.to_string(), media, body));
    }
    // A browser asks for one on every first load; the checker fails on a 404 in the console.
    bodies.push(("favicon.ico".to_string(), "image/x-icon", Vec::new()));
    let files: Vec<(&str, &str, &[u8])> = bodies
        .iter()
        .map(|(name, media, body)| (name.as_str(), *media, body.as_slice()))
        .collect();
    ikigai_ui::testkit::site::write(&out, &files).expect("the site");
    println!("{} pages in {}", PAGES.len(), out.display());
}
