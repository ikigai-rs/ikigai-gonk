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
use ikigai_gonk::{compose_serving, doors, git_object, quic, web};
use ikigai_store::DurableStore;

const PAGES: [(&str, &str); 33] = [
    ("home.html", "/"),
    ("ledgers.html", "/l"),
    ("dashboard.html", "/dashboard"),
    ("ledger.html", "/l/default"),
    (
        "display-group.html",
        "/l/default/group/label/Ym9vazppa2lnYWk",
    ),
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
    // Ledger #3217: an item blocked from three ledgers, with its inbound links panel.
    ("inbound-item.html", "/l/default/item/3"),
    ("named-ledger.html", "/l/crew"),
    // A union of ledgers (ledger #3216): `ledger#N` rows, the sentence naming the ledgers
    // read and one left out (`private` is not in the anonymous grant), and a filtered union.
    (
        "union-ledger.html",
        "/l/default?ledgers=default,crew,private",
    ),
    (
        "union-filter.html",
        "/l/default?ledgers=default,crew&labels=needs-person",
    ),
    (
        "project-ledger.html",
        "/l/default?projects=work&ledgers=default",
    ),
    (
        "project-filter.html",
        "/l/default?projects=work,overlap&labels=needs-person",
    ),
    ("project-empty.html", "/l/default?projects=empty"),
    (
        "saved-filter.html",
        "/l/default?projects=work,overlap&priority=1,none&sort=priority,-number",
    ),
    ("saved-view.html", "/view/triage"),
    ("views.html", "/views"),
    ("named-item.html", "/l/crew/item/1"),
    ("lifecycle.html", "/l/default/lifecycle"),
    ("doctor.html", "/l/default/doctor"),
    ("sparql.html", "/sparql"),
    ("browse.html", "/browse"),
    // Ledger #3721: the fixture repository's harness, its gaps and its firings.
    ("harness.html", "/harness"),
    // Ledger #3777: its detail pages, as a browser is answered them (no `as`, `Accept` HTML).
    (
        "harness-control.html",
        "/harness/control/require-claim?root=fixture",
    ),
    (
        "harness-guide.html",
        "/harness/guide/claim-before-write?root=fixture",
    ),
    (
        "harness-incident.html",
        "/harness/incident/unclaimed-edit?root=fixture",
    ),
    (
        "harness-day.html",
        "/harness/firing/2026-11-02?root=fixture",
    ),
    (
        "harness-coverage.html",
        "/harness/coverage/sensors-without-guide?root=fixture",
    ),
    ("queue.html", "/queue"),
    // Ledger #3285: the sign-in state only, since a static site has no passkey and the door's
    // feeds are live; the subscribed page with its checkbox tree is `tests/feeds_3285.rs`'s.
    ("feed.html", "/feed"),
];

const FILES: [(&str, &str); 6] = [
    ("static/tokens.css", "text/css"),
    ("static/gonk.css", "text/css"),
    ("static/gonk-frame.css", "text/css"),
    ("static/shell.js", "text/javascript"),
    ("static/htmx.min.js", "text/javascript"),
    ("static/gonk.js", "text/javascript"),
];

/// The fixture repository's harness (ledger #3721), the one `tests/harness_3721.rs` builds: two
/// guides (one no control checks), three sensors (one no guide states, one never fired) and two
/// incidents (one no control exists for), so the page draws each grouping and a gap of each kind.
const HARNESS: &str = r#"@prefix harness: <https://ikigai-rs.dev/ns/harness#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ctl:     <urn:iki:harness:fixture:control:> .
@prefix guide:   <urn:iki:harness:fixture:guide:> .
@prefix inc:     <urn:iki:harness:fixture:incident:> .
@prefix test:    <urn:iki:harness:fixture:test:> .
guide:claim-before-write a harness:Guide ; rdfs:label "no claim, no write" ;
    harness:statedIn "AGENTS.md" ; harness:section "1. Claim the item before you write" ;
    harness:direction harness:feedforward ; harness:execution harness:inferential ;
    harness:stage harness:toolCall ; harness:category harness:authoritySafety .
guide:reproduce-first a harness:Guide ; rdfs:label "reproduce before you fix" ;
    harness:statedIn "AGENTS.md" ; harness:section "3. Reproduce first" ;
    harness:direction harness:feedforward ; harness:execution harness:inferential ;
    harness:stage harness:toolCall ; harness:category harness:behavior .
inc:unclaimed-edit a harness:Incident ; rdfs:label "an edit landed with no claim" .
inc:lost-lease a harness:Incident ; rdfs:label "a lease ran out mid-run" .
ctl:require-claim a harness:Control ; rdfs:label "hook: no claim, no write" ;
    harness:direction harness:feedback ; harness:execution harness:computational ;
    harness:stage harness:toolCall ; harness:category harness:authoritySafety ;
    harness:outcome harness:refuse ; harness:enforcedAt "tools/claim_hook.py#main" ;
    harness:statedBy guide:claim-before-write ; harness:existsFor inc:unclaimed-edit ;
    harness:firedBy test:test_harness:SensorsFire.test_require_claim .
ctl:big-diff a harness:Control ; rdfs:label "gate: a diff too big to review" ;
    harness:direction harness:feedback ; harness:execution harness:computational ;
    harness:stage harness:preCommit ; harness:category harness:maintainability ;
    harness:outcome harness:warn ; harness:enforcedAt "tools/gonk.py#check_diff" ;
    harness:firedBy test:test_harness:SensorsFire.test_big_diff .
ctl:never-fires a harness:Control ; rdfs:label "gate: secrets in a commit" ;
    harness:direction harness:feedback ; harness:execution harness:computational ;
    harness:stage harness:preCommit ; harness:category harness:authoritySafety ;
    harness:outcome harness:refuse ; harness:enforcedAt "hooks/pre-commit#secrets" ;
    harness:noFiringTest "the scanner is a third party's" .
"#;

/// Its firing segment: a refusal and two warnings over two days.
const FIRINGS: &str = "# ikigai-log v1
@prefix harness: <https://ikigai-rs.dev/ns/harness#> .
@prefix log:     <https://ikigai-rs.dev/ns/log#> .
@name     urn:log:harness:tree-fixture
@instance urn:iki:harness:tree:fixture
@level    log:info
@started  2026-11-01T00:00:00.000Z
@prev     genesis

2026-11-01T01:00:00.000Z harness:Refusal urn:iki:harness:fixture:control:require-claim seq=1 cmd=sha256:000000000001
2026-11-02T01:00:00.000Z harness:Warning urn:iki:harness:fixture:control:big-diff seq=2 cmd=sha256:000000000002
2026-11-02T02:00:00.000Z harness:Warning urn:iki:harness:fixture:control:big-diff seq=3 cmd=sha256:000000000002
";

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
    // Ledger #3721: a harness in the shape a gonk-starter repository carries, and its firing log.
    std::fs::create_dir_all(repository.join(".gonk/harness")).unwrap();
    std::fs::create_dir_all(repository.join(".gonk/state/harness")).unwrap();
    std::fs::write(repository.join(".gonk/harness/harness.ttl"), HARNESS).unwrap();
    std::fs::write(repository.join(".gonk/state/harness/firings.log"), FIRINGS).unwrap();
    // Where the guides are stated, so a guide's page links into browse at its section.
    std::fs::write(
        repository.join("AGENTS.md"),
        "# Agents\n\n## 1. Claim the item before you write\n\nClaim it.\n\n## 3. Reproduce first\n",
    )
    .unwrap();
    let commit = git(&["rev-parse", "HEAD"]);
    let blob = git(&["rev-parse", "HEAD:example.rs"]);
    let graph = ikigai_gonk::browse::Graph::chosen();
    let (store, _shared) =
        DurableStore::in_memory_shared_declaring(graph.sharer_writes()).expect("a store");
    let browsed = ikigai_gonk::browse::wire(
        vec![("fixture".to_string(), repository.clone())],
        store.shared_writer().expect("the store's writer"),
        None,
        None,
        &graph,
        None,
    );
    let repositories = vec![git_object::Repository {
        name: "fixture".into(),
        dir: repository,
        commit: Some("https://code.example/commit/{sha}".into()),
        blob: Some("https://code.example/blob/{commit}/{path}".into()),
        file: None,
        browse: true,
    }];
    let views = ikigai_gonk::views::Views::new();
    let hub = Arc::new(compose_serving(
        store,
        Some(Arc::new(browsed.space)),
        Vec::new(),
        vec![Arc::new(git_object::space("configured", &repositories))],
        None,
        None,
        None,
        Some(&views),
    ));
    views.start(&hub, Capability::root()).unwrap();
    let saved = ikigai_gonk::views_page::declaration(
        &[ikigai_gonk::views_page::Rule {
            kind: "exact",
            name: "urn:iki:gonk:view:triage".into(),
            target: "urn:iki:gonk:page:ledger:default".into(),
            at: "root",
            original: None,
            filters: [
                ("projects", "work,overlap"),
                ("ledgers", "default"),
                ("labels", "needs-person"),
                ("without", "blocked"),
                ("priority", "1,none"),
                ("status", "open"),
                ("text", ""),
                ("sort", "priority,-number"),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        }],
        8,
    );
    block_on(
        hub.issue(
            Request::new(
                Verb::Sink,
                Iri::parse(ikigai_gonk::views::DECLARATION).unwrap(),
            )
            .with_arg("content", ArgRef::Inline(saved.into_bytes())),
            &Capability::root(),
        ),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while views.dynamic().status().installs == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "saved fixture did not bind"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
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
            // Ledger #3344: the body's qualified references link into `crew`, which the site
            // holds, and `nosuch#2` stays text, so axe reads both in every theme. Ledger
            // #3345: the body is Markdown, so axe reads its rendered heading, strikethrough,
            // table, task list and code block, and the links inside them, in every theme too.
            ArgRef::Inline(
                b"An item for the checker to read\n\n# Plan\n\n~~See nothing.~~ See crew#1, \
                  crew #1 and crew:#1, not nosuch#2; and ledger #2.\n\n\
                  | Step | Where |\n|---|---|\n| one | https://example.com/a |\n\n\
                  - [x] done in ledger #2\n- [ ] next, <b>not bold</b>\n\n\
                  ```\nlet x = 1 < 2;\n```"
                    .to_vec(),
            ),
        )
        .with_arg(
            "labels",
            ArgRef::Inline(b"needs-person,book:ikigai".to_vec()),
        )
        .with_arg("kind", ArgRef::Inline(b"urn:example:Task".to_vec())),
        &Capability::root(),
    ))
    .expect("the scratch item");
    let crew = block_on(Kernel::issue(
        &hub,
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:crew:append").expect("an IRI"),
        )
        .with_arg("content", ArgRef::Inline(b"A crew item".to_vec()))
        .with_arg("labels", ArgRef::Inline(b"crew-label".to_vec())),
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
    // a self-link and unavailable/external IRIs. #3217: it also blocks the handed-back item
    // (#3), which mirrors of a crew blocker and an unreadable private one block as well, so
    // #3's row carries a blocked badge naming all three and its page an inbound panel.
    let record = block_on(Kernel::issue(
        &hub,
        Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
            .with_arg("content", ArgRef::Inline(b"A live flight plan".to_vec()))
            .with_arg("labels", ArgRef::Inline(b"flight-plan".to_vec())),
        &Capability::root(),
    ))
    .expect("the flight plan");
    let record = appended_iri(&record.bytes, "default");
    let crew = appended_iri(&crew.bytes, "crew");
    let links = format!(
        "@prefix ledger: <https://ikigai-rs.dev/ns/ledger#> .\n\
         <{record}> ledger:related <{flying}>, <{waiting}>, <{crew}> ; \
         ledger:parent <{record}> ; ledger:blocks <urn:iki:ledger:private:item:01unreadable>, <{waiting}> ; \
         ledger:about <urn:external:record:abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz>, <urn:git:commit:{commit}>, <urn:git:blob:{blob}> .\n\
         <{waiting}:mirror:blocks:crew> a ledger:Mirror ; ledger:onItem <{waiting}> ; \
         ledger:linkType ledger:blocks ; ledger:mirrorOf <{crew}> ; ledger:linkPresent true ; \
         ledger:subjectOpen true .\n\
         <{waiting}:mirror:blocks:private> a ledger:Mirror ; ledger:onItem <{waiting}> ; \
         ledger:linkType ledger:blocks ; ledger:mirrorOf <urn:iki:ledger:private:item:01unreadable> ; \
         ledger:linkPresent true ; ledger:subjectOpen true .",
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
    let closed = block_on(Kernel::issue(
        &hub,
        Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
            .with_arg("content", ArgRef::Inline(b"A finished book group".to_vec()))
            .with_arg("labels", ArgRef::Inline(b"book:closed".to_vec())),
        &Capability::root(),
    ))
    .unwrap();
    let closed = appended_iri(&closed.bytes, "default");
    block_on(Kernel::issue(
        &hub,
        Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:close").unwrap())
            .with_arg("item", ArgRef::Inline(closed.into_bytes())),
        &Capability::root(),
    ))
    .unwrap();
    let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string(), "crew".to_string()],
        browse_roots: vec!["fixture".to_string()].into(),
        passkeys: Arc::clone(&passkeys),
        // Ledger #365: include deployment CURIE bindings in the browser fixture.
        rules: format!(
            "{}\n{}",
            ikigai_gonk::rules::DEFAULT_RULES,
            r#"
@prefix render: <urn:iki:gonk:render#> .
<urn:example:curie> a render:Rule ; render:matchKind "uri" ;
    render:matchPrefix "https://example.org/vocab#" ; render:curie "example" .
<urn:example:link> a render:Rule ; render:matchKind "uri" ;
    render:matchPrefix "https://example.org/vocab#" ; render:link "/inspect?value={value}" ;
    render:label "Inspect {value}" .
<urn:example:long> a render:Rule ; render:matchKind "uri" ;
    render:matchPrefix "https://example.org/a-deployment-with-a-long-namespace/another-long-segment/vocabulary#" ;
    render:curie "deployment" .
"#
        ).into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        display: ikigai_gonk::display::Rules::parse(r#"
@prefix d: <urn:iki:gonk:display#> .
<urn:iki:gonk:display-rule:label:book> a d:Rule; d:prefix "book:"; d:badge "outline"; d:order 1; d:counts false; d:group true .
<urn:iki:gonk:display-rule:label:person> a d:Rule; d:label "needs-person"; d:badge "strong"; d:order -5 .
<urn:iki:gonk:display-rule:level:task> a d:Rule; d:level "urn:example:Task"; d:badge "strong" .
<urn:iki:gonk:display-rule:state:filed> a d:Rule; d:state "filed"; d:badge "outline" .
<urn:iki:gonk:display-rule:ledger:default> a d:Rule; d:ledger "default"; d:badge "strong" .
"#).unwrap(),
        page_assets: Default::default(),
        projects: ikigai_gonk::projects::Projects::parse(&[
            "work crew,private".into(), "overlap default,crew".into(), "empty".into(),
            "catalog sample-1,sample-2,sample-3,sample-4,sample-5,sample-6,sample-7".into(),
        ]).expect("fixture projects"),
        toolbox: false,
        // The add-on row (ledger #3279) on every page below.
        faces: vec!["demo".to_string(), "intake".to_string()],
    });
    let http = Arc::new(doors::http_kernel_serving(
        hub,
        ikigai_gonk::layers_view::bind(web::space(Arc::clone(&face)), &face, None),
        Vec::new(),
        None,
    ));
    let door = doors::HttpDoor {
        anonymous: anonymous_http_grants(
            &[
                "default", "crew", "sample-1", "sample-2", "sample-3", "sample-4", "sample-5",
                "sample-6", "sample-7",
            ]
            .map(str::to_string),
        )
        .expect("the anonymous grant")
        .into_iter()
        .chain([
            ikigai_gonk::views::read_grant(),
            ikigai_gonk::views::write_grant(),
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
    // Used bindings are visible once each, including a linked CURIE; the long namespace
    // exercises legend wrapping at phone width in the CI axe job.
    let query = "SELECT ?value WHERE { VALUES ?value { <https://example.org/vocab#open> <https://example.org/vocab#closed> <https://example.org/a-deployment-with-a-long-namespace/another-long-segment/vocabulary#ready> <https://ikigai-rs.dev/ns/ledger#open> } }";
    let (status, body) = get(
        addr,
        &format!("/sparql?query={}", ikigai_fn::html::url_segment(query)),
    );
    assert_eq!(status, 200, "the CURIE fixture");
    bodies.push((
        "sparql-curies.html".into(),
        "text/html; charset=utf-8",
        body,
    ));
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
        (
            "union-facet.fragment",
            "/l/default/facet?ledgers=default,crew",
        ),
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
    // A face's page in gonk's bar (ledger #3279), framed by the code that frames a served one.
    let roster = ["demo".to_string(), "intake".to_string()];
    let framed = ikigai_gonk::face_frame::Frame::new("demo", &roster)
        .apply(
            b"<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
              <title>Demo add-on</title></head><body><main><h1>Demo add-on</h1>\
              <p>A deployment's own page.</p></main></body></html>",
        )
        .expect("the fixture has a body tag");
    bodies.push((
        "face-demo.html".to_string(),
        "text/html; charset=utf-8",
        framed,
    ));
    // A browser asks for one on every first load; the checker fails on a 404 in the console.
    bodies.push(("favicon.ico".to_string(), "image/x-icon", Vec::new()));
    let files: Vec<(&str, &str, &[u8])> = bodies
        .iter()
        .map(|(name, media, body)| (name.as_str(), *media, body.as_slice()))
        .collect();
    ikigai_ui::testkit::site::write(&out, &files).expect("the site");
    println!("{} pages in {}", PAGES.len(), out.display());
}
