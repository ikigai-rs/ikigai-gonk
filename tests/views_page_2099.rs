//! **The Views page** (ledger #2099): the named views (ledger #2006) on a page beside Layers,
//! driven over real HTTP the way a browser drives it. One test per acceptance criterion:
//!
//! 1. the page lists the README's example declaration, each exact name a link to
//!    `/view/{name}`, with the status door's state;
//! 2. add, change and remove each round-trip through the declaration and the status, and the
//!    view answers (or stops answering) after each;
//! 3. a caller without the declaration's write token gets the page with no controls at all,
//!    and a post it makes anyway is Denied by the store, changing nothing;
//! 4. a refused declaration leaves the old views in force, and the page lists those and says
//!    why the stored one was refused;
//! 5. a form drawn from a declaration that has since changed writes nothing;
//! 6. the header offers "Views" exactly to a caller whose grant reads the declaration.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, DynamicStatus, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::views::{self, Views, CHECK, CHECK_QUERY, DECLARATION};
use ikigai_gonk::{doors, identity::Passkeys, quic, web};
use ikigai_store::DurableStore;

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

fn iri(s: &str) -> Iri {
    Iri::parse(s).unwrap()
}

struct Gonk {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    views: Views,
    _config: tempfile::TempDir,
}

/// A served gonk with the views started, whose anonymous loopback caller holds `anonymous`.
fn gonk(anonymous: Vec<String>) -> Gonk {
    let views = Views::new();
    let hub = Arc::new(ikigai_gonk::compose_serving(
        DurableStore::in_memory().unwrap(),
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        Some(&views),
    ));
    views.start(&hub, Capability::root()).unwrap();
    wait_for(&views, |status| status.refusal.is_some());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new().into(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        toolbox: false,
    });
    let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
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
        views,
        _config: config,
    }
}

/// The grants of a person who reads the default ledger, plus `extra`.
fn reader_plus(extra: &[String]) -> Vec<String> {
    let mut grants = grants_for("default", Authority::Read).unwrap();
    grants.extend(extra.iter().cloned());
    grants
}

fn operator() -> Vec<String> {
    reader_plus(&[views::read_grant(), views::write_grant()])
}

struct Response {
    status: u16,
    body: String,
}

impl Gonk {
    fn raw(&self, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> Response {
        let mut stream = TcpStream::connect(self.addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
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
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        Response {
            status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            body: body.to_string(),
        }
    }

    /// A browser navigating to `path`.
    fn page(&self, path: &str) -> Response {
        self.raw(
            "GET",
            path,
            &[
                ("Accept", CHROME_ACCEPT.to_string()),
                ("Sec-Fetch-Site", "none".to_string()),
                ("Sec-Fetch-Mode", "navigate".to_string()),
            ],
            "",
        )
    }

    /// A browser submitting one of the page's forms (a plain POST, no htmx).
    fn save(&self, fields: &[(&str, &str)]) -> Response {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        self.raw(
            "POST",
            "/views/save",
            &[
                ("Accept", CHROME_ACCEPT.to_string()),
                ("Origin", format!("http://localhost:{}", self.addr.port())),
                ("Sec-Fetch-Site", "same-origin".to_string()),
                ("Sec-Fetch-Mode", "navigate".to_string()),
                (
                    "Content-Type",
                    "application/x-www-form-urlencoded".to_string(),
                ),
            ],
            &body,
        )
    }

    /// The `base` the page's forms carry now.
    fn base(&self) -> String {
        let page = self.page("/views");
        assert_eq!(page.status, 200, "{}", page.body);
        let at = page
            .body
            .find("name='base' type='hidden' value='")
            .unwrap_or_else(|| panic!("no base in the page: {}", page.body))
            + "name='base' type='hidden' value='".len();
        page.body[at..at + page.body[at..].find('\'').unwrap()].to_string()
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, Error> {
        block_on(
            self.hub
                .issue(Request::new(Verb::Source, iri(name)), &Capability::root()),
        )
        .map(|r| r.bytes)
    }

    fn stored(&self) -> String {
        String::from_utf8(self.get(DECLARATION).unwrap_or_default()).unwrap()
    }

    fn declare_as_root(&self, turtle: &str) {
        let write = Request::new(Verb::Sink, iri(DECLARATION))
            .with_arg("content", ArgRef::Inline(turtle.as_bytes().to_vec()));
        block_on(self.hub.issue(write, &Capability::root())).unwrap();
    }
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn wait_for(views: &Views, done: impl Fn(&DynamicStatus) -> bool) -> DynamicStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = views.dynamic().status();
        if done(&status) {
            return status;
        }
        assert!(Instant::now() < deadline, "never reached: {status:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The README's example declaration, read out of the README as `tests/views_2006.rs` does.
fn readme_declaration() -> String {
    let readme = include_str!("../README.md");
    let section = &readme[readme.find("## Named views").unwrap()..];
    let start = section.find("```turtle\n").unwrap() + "```turtle\n".len();
    section[start..start + section[start..].find("```").unwrap()].to_string()
}

/// How many `<form` elements post to `/views/save`.
fn forms(body: &str) -> usize {
    body.matches("action='/views/save'").count()
}

// 1 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_page_lists_the_readmes_declaration_with_links_and_the_status() {
    let gonk = gonk(operator());
    gonk.declare_as_root(&readme_declaration());
    wait_for(&gonk.views, |s| s.installs == 1);

    let page = gonk.page("/views");
    assert_eq!(page.status, 200, "{}", page.body);
    let body = &page.body;
    assert!(body.starts_with("<!DOCTYPE html>"), "{body}");
    assert!(body.contains("<html lang='en'>"), "{body}");
    assert!(body.contains("<h1 id='views-title'>Views</h1>"), "{body}");
    for (name, local, target) in [
        (
            "urn:iki:gonk:view:next",
            "next",
            "urn:iki:ledger:default:next",
        ),
        (
            "urn:iki:gonk:view:open",
            "open",
            "urn:iki:ledger:default:items",
        ),
    ] {
        assert!(
            body.contains(&format!("<a href='/view/{local}'>{name}</a>")),
            "{name} links to its view: {body}"
        );
        assert!(body.contains(target), "{body}");
    }
    assert!(body.contains("<td>exact</td>"), "{body}");
    assert!(body.contains("<td>root</td>"), "{body}");
    assert!(body.contains("the 1st to bind"), "{body}");
    assert!(
        body.contains("The stored declaration is the one in force."),
        "{body}"
    );
    assert!(body.contains("none since the last bind"), "{body}");
    assert_eq!(
        forms(body),
        0,
        "fixed bindings cannot be edited by this form"
    );
    assert!(body.contains("fields this page does not show"), "{body}");
    // The header's Views link marks this page as the current one.
    assert!(
        body.contains("<a aria-current='page' class='navlink' href='/views'>Views</a>"),
        "{body}"
    );
    // A link from the page reaches the view, which answers what its target answers.
    let view = gonk.raw(
        "GET",
        "/view/open",
        &[("Accept", "text/turtle".to_string())],
        "",
    );
    assert_eq!(view.status, 200, "{}", view.body);
}

// 2 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn add_change_and_remove_each_round_trip_through_the_declaration_and_the_status() {
    let gonk = gonk(operator());
    let page = gonk.page("/views").body;
    assert!(page.contains("No views are in force."), "{page}");
    assert_eq!(forms(&page), 1, "only the add form: {page}");
    assert!(
        page.contains("<h2 id='views-add-title'>Add a view</h2>"),
        "{page}"
    );

    // Add.
    let added = gonk.save(&[
        ("op", "add"),
        ("base", &gonk.base()),
        ("name", "rule"),
        ("target", CHECK),
        ("kind", "exact"),
    ]);
    assert_eq!(added.status, 200, "{}", added.body);
    assert!(
        added
            .body
            .contains("Added the view urn:iki:gonk:view:rule. The views space bound it"),
        "{}",
        added.body
    );
    assert!(added.body.contains("role='status'"), "{}", added.body);
    assert!(gonk.stored().contains("\"urn:iki:gonk:view:rule\""));
    assert_eq!(gonk.views.dynamic().status().installs, 1);
    assert_eq!(
        gonk.get("urn:iki:gonk:view:rule").unwrap(),
        CHECK_QUERY.as_bytes()
    );
    assert_eq!(
        forms(&added.body),
        2,
        "a form for the view and the add form"
    );
    assert!(added.body.contains("the 1st to bind"), "{}", added.body);

    // Change: a new name, and a prefix rather than one name.
    let changed = gonk.save(&[
        ("op", "change"),
        ("base", &gonk.base()),
        ("was", "urn:iki:gonk:view:rule"),
        ("was-kind", "exact"),
        ("name", "rules:"),
        ("target", "urn:iki:gonk:views:"),
        ("kind", "prefix"),
    ]);
    assert_eq!(changed.status, 200, "{}", changed.body);
    assert!(
        changed
            .body
            .contains("Changed the view urn:iki:gonk:view:rules:. The views space bound it"),
        "{}",
        changed.body
    );
    let stored = gonk.stored();
    assert!(!stored.contains("\"urn:iki:gonk:view:rule\""), "{stored}");
    assert!(stored.contains("\"prefix\""), "{stored}");
    assert_eq!(gonk.views.dynamic().status().installs, 2);
    assert!(matches!(
        gonk.get("urn:iki:gonk:view:rule"),
        Err(Error::Unresolved(_))
    ));
    assert_eq!(
        gonk.get("urn:iki:gonk:view:rules:check").unwrap(),
        CHECK_QUERY.as_bytes()
    );
    // A prefix is a namespace, not a name: listed, not linked.
    assert!(changed.body.contains("<td>prefix</td>"), "{}", changed.body);
    assert!(
        !changed.body.contains("href='/view/rules"),
        "{}",
        changed.body
    );

    // Remove.
    let removed = gonk.save(&[
        ("op", "remove"),
        ("base", &gonk.base()),
        ("was", "urn:iki:gonk:view:rules:"),
        ("was-kind", "prefix"),
    ]);
    assert_eq!(removed.status, 200, "{}", removed.body);
    assert!(
        removed
            .body
            .contains("Removed the view urn:iki:gonk:view:rules:. The views space bound it"),
        "{}",
        removed.body
    );
    assert!(
        removed.body.contains("No views are in force."),
        "{}",
        removed.body
    );
    assert_eq!(gonk.views.dynamic().status().installs, 3);
    assert!(matches!(
        gonk.get("urn:iki:gonk:view:rules:check"),
        Err(Error::Unresolved(_))
    ));
}

// 3 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_caller_without_the_write_grant_sees_no_controls_and_changes_nothing() {
    let gonk = gonk(reader_plus(&[views::read_grant()]));
    gonk.declare_as_root(&readme_declaration());
    wait_for(&gonk.views, |s| s.installs == 1);

    let page = gonk.page("/views");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(
        page.body.contains("urn:iki:gonk:view:open"),
        "{}",
        page.body
    );
    assert_eq!(forms(&page.body), 0, "no form at all: {}", page.body);
    for absent in ["<form", "<button", "Add a view", "disabled"] {
        assert!(
            !page.body[page.body.find("<main").unwrap()..].contains(absent),
            "{absent} in {}",
            page.body
        );
    }
    assert!(
        page.body.contains(&format!(
            "You may read the views but not change them: that takes the grant {}.",
            views::write_grant()
        )),
        "{}",
        page.body
    );

    // A post made anyway is the store's to refuse, and it does.
    let before = gonk.stored();
    // The base a form would carry, so the post reaches the write.
    let base = ikigai_core::ContentId::of(before.as_bytes()).to_string();
    let refused = gonk.save(&[
        ("op", "add"),
        ("base", &base),
        ("name", "mine"),
        ("target", CHECK),
        ("kind", "exact"),
    ]);
    assert_eq!(refused.status, 403, "{}", refused.body);
    assert!(
        refused.body.contains(&views::write_grant()),
        "{}",
        refused.body
    );
    assert_eq!(gonk.stored(), before, "nothing was written");
    // And the store itself refuses the same caller's write, page or no page.
    let reader = Capability::root().attenuate(reader_plus(&[views::read_grant()]));
    let write = Request::new(Verb::Sink, iri(DECLARATION))
        .with_arg("content", ArgRef::Inline(readme_declaration().into_bytes()));
    let denied = block_on(gonk.hub.issue(write, &reader)).unwrap_err();
    assert!(matches!(denied, Error::Denied(_)), "{denied}");
    let raw = Request::new(Verb::Sink, iri("urn:iki:store:graph-update"))
        .with_arg("graph", ArgRef::Inline(DECLARATION.as_bytes().to_vec()))
        .with_arg(
            "content",
            ArgRef::Inline(format!("CLEAR SILENT GRAPH <{DECLARATION}>").into_bytes()),
        );
    let denied = block_on(gonk.hub.issue(raw, &reader)).unwrap_err();
    assert!(matches!(denied, Error::Denied(_)), "{denied}");
    assert!(
        denied.to_string().contains("urn:cap:store:write:graph:"),
        "{denied}"
    );
    assert_eq!(gonk.views.dynamic().status().installs, 1);
    assert!(matches!(
        gonk.get("urn:iki:gonk:view:mine"),
        Err(Error::Unresolved(_))
    ));
}

// 4 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_refused_declaration_leaves_the_old_views_and_the_page_says_why() {
    let gonk = gonk(operator());
    let added = gonk.save(&[
        ("op", "add"),
        ("base", &gonk.base()),
        ("name", "rule"),
        ("target", CHECK),
        ("kind", "exact"),
    ]);
    assert!(added.body.contains("it is in force"), "{}", added.body);

    // An operator, over the socket, declares a name outside the views' namespace.
    let refused = readme_declaration().replace("urn:iki:gonk:view:next", "urn:iki:gonk:elsewhere");
    gonk.declare_as_root(&refused);
    wait_for(&gonk.views, |s| s.refusal.is_some());

    let page = gonk.page("/views").body;
    assert!(
        page.contains(
            "The stored declaration was refused, so the views listed below are the \
                       ones that bound before it."
        ),
        "{page}"
    );
    assert!(
        page.contains("the host&apos;s check refused it")
            || page.contains("the host's check refused it"),
        "{page}"
    );
    assert!(page.contains(CHECK), "{page}");
    assert!(
        page.contains("urn:iki:gonk:view:rule"),
        "the old view is listed: {page}"
    );
    assert!(
        !page.contains("urn:iki:gonk:view:open"),
        "the refused one is not: {page}"
    );
    assert_eq!(
        gonk.get("urn:iki:gonk:view:rule").unwrap(),
        CHECK_QUERY.as_bytes(),
        "the old view still answers"
    );

    // The next save from the page builds on what is IN FORCE, so it repairs the declaration.
    let repaired = gonk.save(&[
        ("op", "add"),
        ("base", &gonk.base()),
        ("name", "next"),
        ("target", "urn:iki:ledger:default:next"),
        ("kind", "exact"),
    ]);
    assert!(
        repaired.body.contains("it is in force"),
        "{}",
        repaired.body
    );
    assert!(!gonk.stored().contains("urn:iki:gonk:elsewhere"));
    assert_eq!(
        gonk.get("urn:iki:gonk:view:rule").unwrap(),
        CHECK_QUERY.as_bytes()
    );
}

// 5 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_form_drawn_before_the_declaration_changed_writes_nothing() {
    let gonk = gonk(operator());
    let stale = gonk.base();
    // This test needs editable aliases; the README also contains a fixed binding.
    let mut editable = ikigai_core::Topology::from_turtle(&readme_declaration()).unwrap();
    let ikigai_core::SpaceKind::Alias { rules, .. } = &mut editable.kind else {
        panic!("the README declares an alias")
    };
    rules.retain(|rule| !rule.is_named_request());
    gonk.declare_as_root(&editable.to_turtle());
    wait_for(&gonk.views, |s| s.installs == 1);
    let before = gonk.stored();
    let answer = gonk.save(&[
        ("op", "add"),
        ("base", &stale),
        ("name", "late"),
        ("target", CHECK),
        ("kind", "exact"),
    ]);
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(
        answer
            .body
            .contains("Nothing was saved: the declaration changed after this page was drawn."),
        "{}",
        answer.body
    );
    assert!(answer.body.contains("role='alert'"), "{}", answer.body);
    assert_eq!(gonk.stored(), before);

    // And a form that names a view twice is refused before anything is written.
    let twice = gonk.save(&[
        ("op", "add"),
        ("base", &gonk.base()),
        ("name", "open"),
        ("target", CHECK),
        ("kind", "exact"),
    ]);
    assert!(twice.body.contains("already declared"), "{}", twice.body);
    assert_eq!(gonk.stored(), before);
}

// 6 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_header_offers_views_exactly_to_a_caller_who_reads_them() {
    let reader = gonk(reader_plus(&[views::read_grant()]));
    let home = reader.page("/l").body;
    assert!(
        home.contains("<a class='navlink' href='/views'>Views</a>"),
        "{home}"
    );

    let ledger_only = gonk(reader_plus(&[]));
    let home = ledger_only.page("/l").body;
    assert!(!home.contains("href='/views'"), "{home}");
    let page = ledger_only.page("/views");
    assert_eq!(page.status, 403, "{}", page.body);
    assert!(page.body.contains(&views::read_grant()), "{}", page.body);
}
