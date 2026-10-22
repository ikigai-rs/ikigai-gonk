//! **Named views as data** (ledger #2006, arc 2), in gonk's real hub: core 0.1.110's
//! root-resolving `ik:Alias` rules in a `DynamicSpace` placed last, rebuilt from the
//! declaration graph by core's `drive` on its own thread. One test per acceptance criterion:
//!
//! 1. a view added to the declaration resolves after the rebuild;
//! 2. a view removed answers Unresolved, and its cached answer is gone;
//! 3. a declaration naming something outside `urn:iki:gonk:view:` is refused by the check, the
//!    old views still answer, and the status names the reason;
//! 4. a view whose target the caller may not read is refused exactly as the target is;
//! 5. writing the declaration without the graph's write grant is Denied;
//! 6. with no declaration graph the views space is empty, the hub serves, and the status says why;
//! 7. a view never shadows a name gonk already serves (the placement, shown without the check).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, DynamicRefusal, DynamicStatus, Error, Iri, Kernel, Request, Verb,
};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::views::{self, Views, CHECK, CHECK_QUERY, DECLARATION, STATUS};
use ikigai_store::DurableStore;

const VIEW: &str = "urn:iki:gonk:view:check";

fn iri(s: &str) -> Iri {
    Iri::parse(s).unwrap()
}

/// gonk's hub with the views, started as `main` starts them.
fn hub(views: &Views) -> Arc<Kernel> {
    let hub = Arc::new(ikigai_gonk::compose_serving(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        Some(views),
    ));
    views
        .start(&hub, Capability::root())
        .expect("the rebuild starts");
    // Finish the initial empty read before writing: otherwise the driver may
    // install the first declaration both at startup and for its queued cut.
    wait_for(views, |status| status.refusal.is_some());
    hub
}

/// A declaration of root-resolving rules `(kind, from, to)`, over an empty child, written by
/// hand as an operator writes one.
fn declaration(rules: &[(&str, &str, &str)]) -> String {
    let mut out = String::from(
        "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
         @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\n\
         <urn:ikigai:space:_:1> a ik:Alias ;\n    ik:maxHops 8",
    );
    for i in 1..=rules.len() {
        out.push_str(&format!(
            " ;\n    ik:rewrites <urn:ikigai:space:_:1:rule:{i}>"
        ));
    }
    out.push_str(" ;\n    ik:space <urn:ikigai:space:_:2> .\n");
    for (i, (kind, from, to)) in rules.iter().enumerate() {
        out.push_str(&format!(
            "\n<urn:ikigai:space:_:1:rule:{}> a ik:RewriteRule ;\n    ik:ruleKind \"{kind}\" ;\n    \
             ik:logical \"{from}\" ;\n    ik:canonical \"{to}\" ;\n    ik:resolveAt \"root\" .\n",
            i + 1
        ));
    }
    out.push_str("\n<urn:ikigai:space:_:2> a ik:EndpointSpace ;\n    ik:doors rdf:nil .\n");
    out
}

fn source(target: &str) -> Request {
    Request::new(Verb::Source, iri(target))
}

fn get(hub: &Kernel, target: &str, cap: &Capability) -> Result<Vec<u8>, Error> {
    block_on(hub.issue(source(target), cap)).map(|r| r.bytes)
}

fn declare(hub: &Kernel, turtle: &str, cap: &Capability) -> Result<Vec<u8>, Error> {
    let write = Request::new(Verb::Sink, iri(DECLARATION))
        .with_arg("content", ArgRef::Inline(turtle.as_bytes().to_vec()));
    block_on(hub.issue(write, cap)).map(|r| r.bytes)
}

/// Wait for the rebuild thread to reach a status `done` accepts.
fn wait_for(views: &Views, done: impl Fn(&DynamicStatus) -> bool) -> DynamicStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = views.dynamic().status();
        if done(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the views never reached the expected status: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Declare `turtle` under root and wait until it bound (or was refused) as install `n`.
fn declare_and_wait(views: &Views, hub: &Kernel, turtle: &str, n: u64) -> DynamicStatus {
    declare(hub, turtle, &Capability::root()).expect("the declaration is written");
    wait_for(views, |s| s.installs == n)
}

// 1, 2 ─────────────────────────────────────────────────────────────────────────

#[test]
fn a_view_added_resolves_and_a_view_removed_is_gone_with_its_cached_answer() {
    let views = Views::new();
    let hub = hub(&views);
    let root = Capability::root();
    assert!(matches!(get(&hub, VIEW, &root), Err(Error::Unresolved(_))));

    declare_and_wait(&views, &hub, &declaration(&[("exact", VIEW, CHECK)]), 1);
    assert_eq!(
        get(&hub, VIEW, &root).unwrap(),
        CHECK_QUERY.as_bytes(),
        "the view answers what its target answers"
    );
    assert!(hub.is_cached(&source(VIEW), &root), "the view is cached");

    // Removed: a declaration that binds nothing.
    declare_and_wait(&views, &hub, &declaration(&[]), 2);
    assert!(
        !hub.is_cached(&source(VIEW), &root),
        "the swap took the view's cached answer with it"
    );
    assert!(matches!(get(&hub, VIEW, &root), Err(Error::Unresolved(_))));
}

#[test]
fn a_prefix_rule_maps_a_whole_view_namespace() {
    let views = Views::new();
    let hub = hub(&views);
    declare_and_wait(
        &views,
        &hub,
        &declaration(&[("prefix", "urn:iki:gonk:view:rule:", "urn:iki:gonk:views:")]),
        1,
    );
    assert_eq!(
        get(&hub, "urn:iki:gonk:view:rule:check", &Capability::root()).unwrap(),
        CHECK_QUERY.as_bytes()
    );
}

// 3 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_declaration_outside_the_view_namespace_is_refused_and_the_old_views_stand() {
    let views = Views::new();
    let hub = hub(&views);
    let root = Capability::root();
    declare_and_wait(&views, &hub, &declaration(&[("exact", VIEW, CHECK)]), 1);

    let outside = declaration(&[
        ("exact", VIEW, CHECK),
        ("exact", "urn:iki:gonk:elsewhere", CHECK),
    ]);
    declare(&hub, &outside, &root).expect("the write itself is the store's to allow");
    let status = wait_for(&views, |s| s.refusal.is_some());
    assert_eq!(status.installs, 1, "nothing new bound");
    let (refusal, _) = status.refusal.unwrap();
    assert!(
        matches!(&refusal, DynamicRefusal::Check { reason } if reason.contains(CHECK)),
        "{refusal}"
    );
    assert_eq!(
        get(&hub, VIEW, &root).unwrap(),
        CHECK_QUERY.as_bytes(),
        "the old views still answer"
    );
    let text = String::from_utf8(get(&hub, STATUS, &root).unwrap()).unwrap();
    assert!(text.contains("urn:iki:gonk:view:"), "{text}");
    assert!(text.contains(CHECK), "{text}");
}

// 4 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_view_grants_nothing_its_reader_could_not_already_read() {
    let views = Views::new();
    let hub = hub(&views);
    let target = "urn:iki:ledger:default:items";
    let view = "urn:iki:gonk:view:open";
    declare_and_wait(&views, &hub, &declaration(&[("exact", view, target)]), 1);

    let reader = Capability::root().attenuate(grants_for("default", Authority::Read).unwrap());
    let direct = get(&hub, target, &reader).map_err(|e| e.to_string());
    let through = get(&hub, view, &reader).map_err(|e| e.to_string());
    assert!(direct.is_ok(), "{direct:?}");
    assert_eq!(through, direct, "a reader reads the same through the view");

    let stranger = Capability::root().attenuate(["urn:cap:example:other"]);
    let direct = get(&hub, target, &stranger).unwrap_err();
    let through = get(&hub, view, &stranger).unwrap_err();
    assert!(matches!(direct, Error::Denied(_)), "{direct}");
    assert!(matches!(through, Error::Denied(_)), "{through}");
    assert_eq!(through.to_string(), direct.to_string());
}

// 5 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn only_the_graphs_write_grant_changes_the_views() {
    let views = Views::new();
    let hub = hub(&views);
    let turtle = declaration(&[("exact", VIEW, CHECK)]);

    // A ledger writer is not a views writer.
    let writer = Capability::root().attenuate(grants_for("default", Authority::Write).unwrap());
    let refused = declare(&hub, &turtle, &writer).unwrap_err();
    assert!(matches!(refused, Error::Denied(_)), "{refused}");
    // Nor through the store's own door, which is where the boundary lives.
    let raw = Request::new(Verb::Sink, iri("urn:iki:store:graph-update"))
        .with_arg("graph", ArgRef::Inline(DECLARATION.as_bytes().to_vec()))
        .with_arg(
            "content",
            ArgRef::Inline(format!("CLEAR SILENT GRAPH <{DECLARATION}>").into_bytes()),
        );
    let refused = block_on(hub.issue(raw, &writer)).unwrap_err();
    assert!(matches!(refused, Error::Denied(_)), "{refused}");
    assert!(
        refused.to_string().contains(&views::write_grant()),
        "{refused}"
    );

    // Exactly the narrow write token is enough.
    let operator = Capability::root().attenuate([views::write_grant()]);
    declare(&hub, &turtle, &operator).unwrap();
    wait_for(&views, |s| s.installs == 1);
    assert_eq!(
        get(&hub, VIEW, &Capability::root()).unwrap(),
        CHECK_QUERY.as_bytes()
    );
}

// 6 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn with_no_declaration_the_space_is_empty_and_the_status_says_why() {
    let views = Views::new();
    let hub = hub(&views);
    let status = wait_for(&views, |s| s.refusal.is_some());
    assert_eq!(status.installs, 0);
    assert!(status.in_force.is_none());
    let (refusal, _) = status.refusal.unwrap();
    assert!(
        matches!(&refusal, DynamicRefusal::Unreadable { reason } if reason.contains("no views are declared")),
        "{refusal}"
    );
    // gonk serves as before.
    assert_eq!(
        get(&hub, CHECK, &Capability::root()).unwrap(),
        CHECK_QUERY.as_bytes()
    );
    assert!(matches!(
        get(&hub, VIEW, &Capability::root()),
        Err(Error::Unresolved(_))
    ));
    let text = String::from_utf8(get(&hub, STATUS, &Capability::root()).unwrap()).unwrap();
    assert!(text.contains("no views are declared"), "{text}");
}

// 7 ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_view_never_shadows_a_name_gonk_already_serves() {
    let served = "urn:iki:ledger:ledgers";
    let shadow = declaration(&[("exact", served, CHECK)]);

    // gonk's check refuses to declare it at all …
    let views = Views::new();
    let checked = hub(&views);
    declare(&checked, &shadow, &Capability::root()).unwrap();
    let status = wait_for(&views, |s| {
        matches!(&s.refusal, Some((DynamicRefusal::Check { .. }, _)))
    });
    assert_eq!(status.installs, 0);

    // … and without the check, the placement alone keeps the served name the hub's.
    let unchecked = Views::unchecked();
    let bare = hub(&unchecked);
    let before = get(&bare, served, &Capability::root()).map_err(|e| e.to_string());
    declare_and_wait(&unchecked, &bare, &shadow, 1);
    let after = get(&bare, served, &Capability::root()).map_err(|e| e.to_string());
    assert_eq!(
        after, before,
        "the served name still answers as gonk serves it"
    );
    assert_ne!(after, Ok(CHECK_QUERY.as_bytes().to_vec()));
}

// Through a door ────────────────────────────────────────────────────────────────

#[test]
fn a_view_answers_through_the_socket_door_and_so_does_the_status() {
    let views = Views::new();
    let hub = hub(&views);
    let door = ikigai_gonk::doors::door_kernel(Arc::clone(&hub));
    let root = Capability::root();
    // The declaration's own Sink, through the door, is what the rebuild hears.
    let write = Request::new(Verb::Sink, iri(DECLARATION)).with_arg(
        "content",
        ArgRef::Inline(declaration(&[("exact", VIEW, CHECK)]).into_bytes()),
    );
    block_on(door.issue(write, &root)).unwrap();
    wait_for(&views, |s| s.installs == 1);
    assert_eq!(get(&door, VIEW, &root).unwrap(), CHECK_QUERY.as_bytes());
    let text = String::from_utf8(get(&door, STATUS, &root).unwrap()).unwrap();
    assert!(text.contains(DECLARATION), "{text}");
}

// The README's example ─────────────────────────────────────────────────────────

/// The declaration the README shows, read out of the README, so the example an operator copies
/// is the one this test binds.
fn readme_declaration() -> String {
    let readme = include_str!("../README.md");
    let section = &readme[readme
        .find("## Named views")
        .expect("the README's views section")..];
    let start = section.find("```turtle\n").expect("a Turtle block") + "```turtle\n".len();
    section[start..start + section[start..].find("```").unwrap()].to_string()
}

#[test]
fn the_readmes_declaration_binds_and_its_views_forward_the_callers_arguments() {
    let views = Views::new();
    let hub = hub(&views);
    declare_and_wait(&views, &hub, &readme_declaration(), 1);
    let root = Capability::root();
    for (view, target) in [
        ("urn:iki:gonk:view:next", "urn:iki:ledger:default:next"),
        ("urn:iki:gonk:view:open", "urn:iki:ledger:default:items"),
    ] {
        let with_state = |name: &str| {
            Request::new(Verb::Source, iri(name))
                .with_arg("state", ArgRef::Inline(b"open".to_vec()))
        };
        let direct = block_on(hub.issue(with_state(target), &root)).map(|r| r.bytes);
        let through = block_on(hub.issue(with_state(view), &root)).map(|r| r.bytes);
        assert!(direct.is_ok(), "{target}: {direct:?}");
        assert_eq!(
            through.map_err(|e| e.to_string()),
            direct.map_err(|e| e.to_string()),
            "{view}"
        );
    }
}

// HTTP negotiation must use the target's faces, rather than the view forwarder's.
#[test]
fn a_browser_negotiates_the_targets_faces_through_a_view() {
    use ikigai_gonk::doors;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let views = Views::new();
    let hub = hub(&views);
    declare_and_wait(&views, &hub, &readme_declaration(), 1);
    let filed = Request::new(Verb::Sink, iri("urn:iki:ledger:append"))
        .with_arg("content", ArgRef::Inline(b"Readable view item".to_vec()));
    block_on(hub.issue(filed, &Capability::root())).unwrap();
    let http = Arc::new(doors::http_kernel(
        Arc::clone(&hub),
        ikigai_core::EndpointSpace::new(),
    ));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let door = doors::HttpDoor {
        anonymous: grants_for("default", Authority::Read).unwrap(),
        bind: addr,
        passkeys: None,
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    };
    let server = runtime.spawn(ikigai_web::serve_with_listener(
        http,
        doors::http_cap(door.clone()),
        listener,
        doors::edge_config(door),
    ));
    let get = |path: &str, accept: &str| {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: {accept}\r\nConnection: close\r\n\r\n", addr.port()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    };
    let browser = "text/html,application/xhtml+xml,*/*;q=0.8";
    let response = get("/view/open", browser);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("Readable view item"), "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("content-type: text/plain"),
        "{response}"
    );
    let explicit = get("/view/open?as=application/json", "image/png");
    assert!(explicit.starts_with("HTTP/1.1 200"), "{explicit}");
    assert!(
        explicit
            .to_ascii_lowercase()
            .contains("content-type: application/json"),
        "{explicit}"
    );
    let refused = get("/view/open", "image/png");
    assert!(refused.starts_with("HTTP/1.1 406"), "{refused}");
    for (accept, content_type) in [
        (
            "application/json;q=0.9,text/plain;q=0.2",
            "application/json",
        ),
        ("text/plain;q=0,*/*;q=0.8", "text/turtle"),
        ("*/*", "text/plain"),
    ] {
        let response = get("/view/open", accept);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            response
                .to_ascii_lowercase()
                .contains(&format!("content-type: {content_type}")),
            "{response}"
        );
        assert!(response.contains("Readable view item"), "{response}");
    }
    let explicit = get("/view/open?as=text/plain", browser);
    assert!(explicit.starts_with("HTTP/1.1 200"), "{explicit}");
    for (path, accept) in [
        ("/view/open", "*/*;q=0"),
        ("/view/open?as=image/png", browser),
    ] {
        let response = get(path, accept);
        assert!(response.starts_with("HTTP/1.1 406"), "{response}");
    }
    // Prefixes and chains still use core's rule precedence, and the view grants
    // no access to a ledger the anonymous caller cannot read.
    declare_and_wait(
        &views,
        &hub,
        &declaration(&[
            (
                "exact",
                "urn:iki:gonk:view:open",
                "urn:iki:gonk:view:staged",
            ),
            (
                "exact",
                "urn:iki:gonk:view:staged",
                "urn:iki:ledger:default:items",
            ),
            (
                "prefix",
                "urn:iki:gonk:view:list-",
                "urn:iki:ledger:default:",
            ),
            (
                "exact",
                "urn:iki:gonk:view:private",
                "urn:iki:ledger:private:items",
            ),
        ]),
        2,
    );
    for path in ["/view/open", "/view/list-items"] {
        let response = get(path, browser);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("Readable view item"), "{response}");
    }
    let denied = get("/view/private", browser);
    assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");

    // A child rewrite before a root-resolving alias must describe the alias that
    // actually answered, rather than whichever table happens to match first.
    let nested = r#"
        @prefix ik: <https://ikigai-rs.dev/ns#> .
        @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
        @prefix v: <urn:iki:gonk:view:> .
        v:outer a ik:Alias ; ik:maxHops 8 ; ik:rewrites v:outer:rule:1 ; ik:space v:inner .
        v:outer:rule:1 a ik:RewriteRule ; ik:ruleKind "exact" ;
            ik:logical "urn:iki:gonk:view:open" ; ik:canonical "urn:iki:gonk:view:inside" .
        v:inner a ik:Alias ; ik:maxHops 8 ; ik:rewrites v:inner:rule:1 ; ik:space v:empty .
        v:inner:rule:1 a ik:RewriteRule ; ik:ruleKind "exact" ;
            ik:logical "urn:iki:gonk:view:inside" ; ik:canonical "urn:iki:ledger:default:items" ;
            ik:resolveAt "root" .
        v:empty a ik:EndpointSpace ; ik:doors rdf:nil .
    "#;
    declare_and_wait(&views, &hub, nested, 3);
    let response = get("/view/open", browser);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("Readable view item"), "{response}");

    // A changed declaration must change the negotiated contract immediately.
    declare_and_wait(
        &views,
        &hub,
        &declaration(&[("exact", "urn:iki:gonk:view:open", CHECK)]),
        4,
    );
    let changed = get("/view/open", "application/json");
    assert!(changed.starts_with("HTTP/1.1 406"), "{changed}");
    declare_and_wait(&views, &hub, &declaration(&[]), 5);
    let removed = get("/view/open", browser);
    assert!(removed.starts_with("HTTP/1.1 404"), "{removed}");
    server.abort();
}
