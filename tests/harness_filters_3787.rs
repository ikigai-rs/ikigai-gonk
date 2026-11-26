//! Ledger #3787: the Harness page chooses which rules to look at: a repository picker, filters by
//! pack, category, stage, direction, execution, enforcement point, firing state, coverage gap and
//! words, each choice with its count, and the coverage, guides and days over the filtered rules.
//!
//! The fixture is `tests/harness_3721.rs`'s harness, extended: a second graph (the pack `ops`,
//! adopted by the repository), guides and controls in every category and at more stages, a guide
//! left unchecked on purpose (`harness:uncheckedBecause`, vocabulary 0.5.0), and firings on two
//! days. Four roots: `demo` (that harness), `other` (a small harness of its own), `secret` (a
//! harness this caller may not read) and `packs` (the pack's file, and no harness).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{anonymous_http_grants, grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::packs::Config;
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_store::DurableStore;
use serde_json::Value;
use tempfile::TempDir;

const HARNESS: &str = r#"@prefix harness: <https://ikigai-rs.dev/ns/harness#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ctl:     <urn:iki:harness:demo:control:> .
@prefix guide:   <urn:iki:harness:demo:guide:> .
@prefix inc:     <urn:iki:harness:demo:incident:> .
@prefix test:    <urn:iki:harness:demo:test:> .

<> harness:adopts <urn:iki:harness:pack:name:ops> .

guide:claim-before-write a harness:Guide ;
    rdfs:label "no claim, no write" ;
    harness:statedIn "AGENTS.md" ; harness:section "1. Claim the item before you write" ;
    harness:direction harness:feedforward ; harness:execution harness:inferential ;
    harness:stage harness:toolCall ; harness:category harness:authoritySafety .

guide:reproduce-first a harness:Guide ;
    rdfs:label "reproduce before you fix" ;
    harness:statedIn "AGENTS.md" ; harness:section "3. Reproduce first" ;
    harness:direction harness:feedforward ; harness:execution harness:inferential ;
    harness:stage harness:toolCall ; harness:category harness:behavior .

guide:small-commits a harness:Guide ;
    rdfs:label "small commits" ;
    harness:statedIn "AGENTS.md" ; harness:section "4. Commit small" ;
    harness:direction harness:feedforward ; harness:execution harness:inferential ;
    harness:stage harness:preCommit ; harness:category harness:maintainability ;
    harness:uncheckedBecause "a reviewer judges what small means" .

inc:unclaimed-edit a harness:Incident ;
    rdfs:label "an edit landed with no claim" .

ctl:require-claim a harness:Control ;
    rdfs:label "hook: no claim, no write" ;
    harness:direction harness:feedback ; harness:execution harness:computational ;
    harness:stage harness:toolCall ; harness:category harness:authoritySafety ;
    harness:outcome harness:refuse ;
    harness:enforcedAt "tools/claim_hook.py#main" ;
    harness:statedBy guide:claim-before-write ;
    harness:existsFor inc:unclaimed-edit ;
    harness:firedBy test:test_harness:SensorsFire.test_require_claim .

ctl:big-diff a harness:Control ;
    rdfs:label "gate: a diff too big to review" ;
    harness:direction harness:feedback ; harness:execution harness:computational ;
    harness:stage harness:preCommit ; harness:category harness:maintainability ;
    harness:outcome harness:warn ;
    harness:enforcedAt "tools/gonk.py#check_diff" ;
    harness:firedBy test:test_harness:SensorsFire.test_big_diff .

ctl:never-fires a harness:Control ;
    rdfs:label "gate: secrets in a commit" ;
    harness:direction harness:feedback ; harness:execution harness:computational ;
    harness:stage harness:preCommit ; harness:category harness:authoritySafety ;
    harness:outcome harness:refuse ;
    harness:enforcedAt "hooks/pre-commit#secrets" ;
    harness:noFiringTest "the scanner is a third party's" .

ctl:layering a harness:Control ;
    rdfs:label "check: the layers" ;
    rdfs:comment "Modules depend downward only." ;
    harness:direction harness:feedback ; harness:execution harness:computational ;
    harness:stage harness:ci ; harness:category harness:architectureFitness ;
    harness:outcome harness:refuse ;
    harness:enforcedAt "tools/layers.py#check" ;
    harness:noFiringTest "CI records its own" .
"#;

const PACK: &str = r#"@prefix harness: <https://ikigai-rs.dev/ns/harness#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl:     <http://www.w3.org/2002/07/owl#> .
@prefix ctl:     <urn:iki:harness:ops:control:> .
@prefix guide:   <urn:iki:harness:ops:guide:> .

<urn:iki:harness:pack:name:ops> a harness:Pack ;
    rdfs:label "ops rules" ; owl:versionInfo "2.0" .

guide:one-gate a harness:Guide ;
    rdfs:label "one gate per command" ;
    harness:statedIn "AGENTS.md" ; harness:section "5. One gate" ;
    harness:direction harness:feedforward ; harness:execution harness:inferential ;
    harness:stage harness:toolCall ; harness:category harness:behavior .

ctl:gate-newline a harness:Control ;
    rdfs:label "hook: a newline is not a gate" ;
    harness:direction harness:feedback ; harness:execution harness:computational ;
    harness:stage harness:toolCall ; harness:category harness:behavior ;
    harness:outcome harness:warn ;
    harness:enforcedAt "hooks/pre-tool#newline" ;
    harness:statedBy guide:one-gate .

ctl:daily-scan a harness:Control ;
    rdfs:label "scan: dependency drift" ;
    harness:direction harness:feedback ; harness:execution harness:computational ;
    harness:stage harness:dailyDrift ; harness:category harness:maintainability ;
    harness:outcome harness:report ;
    harness:enforcedAt "tools/drift.py#scan" .
"#;

const OTHER: &str = r#"@prefix harness: <https://ikigai-rs.dev/ns/harness#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
<urn:iki:harness:other:guide:be-kind> a harness:Guide ; rdfs:label "be kind" ;
    harness:statedIn "AGENTS.md" ; harness:section "1. Be kind" .
"#;

const SECRET: &str = r#"@prefix harness: <https://ikigai-rs.dev/ns/harness#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
<urn:iki:harness:secret:guide:hidden> a harness:Guide ; rdfs:label "the secret rule" .
"#;

/// Two days: 2026-11-14 and 2026-11-15 (UTC).
const FIRINGS: &str = "# ikigai-log v1\n@prefix harness: <https://ikigai-rs.dev/ns/harness#> .\n\
    @prefix log:     <https://ikigai-rs.dev/ns/log#> .\n@name     urn:log:harness:tree-demo\n\
    @instance urn:iki:harness:tree:demo\n@level    log:info\n@started  2026-11-01T00:00:00.000Z\n\
    @prev     genesis\n\n\
    2026-11-14T01:00:00.000Z harness:Refusal urn:iki:harness:demo:control:require-claim seq=1 cmd=sha256:a1\n\
    2026-11-14T02:00:00.000Z harness:Warning urn:iki:harness:ops:control:gate-newline seq=2 cmd=sha256:b1\n\
    2026-11-15T01:00:00.000Z harness:Refusal urn:iki:harness:demo:control:require-claim seq=3 cmd=sha256:a2\n\
    2026-11-15T02:00:00.000Z harness:Warning urn:iki:harness:demo:control:big-diff seq=4 cmd=sha256:c1\n";

const ALL: [&str; 10] = [
    "claim-before-write",
    "reproduce-first",
    "small-commits",
    "require-claim",
    "big-diff",
    "never-fires",
    "layering",
    "one-gate",
    "gate-newline",
    "daily-scan",
];

fn write(root: &Path, path: &str, text: &str) {
    let at = root.join(path);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(at, text).unwrap();
}

struct Scene {
    _dirs: Vec<TempDir>,
    roots: Vec<(String, PathBuf)>,
    home: TempDir,
}

fn scene() -> Scene {
    let mut dirs = Vec::new();
    let mut roots = Vec::new();
    for name in ["demo", "other", "packs", "secret"] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "README.md", "# a root\n");
        write(dir.path(), ".gitignore", "/.gonk/state/\n");
        roots.push((name.to_string(), dir.path().to_path_buf()));
        dirs.push(dir);
    }
    write(&roots[0].1, ".gonk/harness/harness.ttl", HARNESS);
    write(&roots[0].1, ".gonk/state/harness/firings.log", FIRINGS);
    write(&roots[1].1, ".gonk/harness/harness.ttl", OTHER);
    write(&roots[2].1, "harness/ops.ttl", PACK);
    write(&roots[3].1, ".gonk/harness/harness.ttl", SECRET);
    Scene {
        _dirs: dirs,
        roots,
        home: tempfile::tempdir().unwrap(),
    }
}

struct Served {
    http: Arc<Kernel>,
    _watch: RootWatch,
}

fn served(scene: &Scene) -> Served {
    let roots = scene.roots.clone();
    let packs = Config::parse(
        &[],
        &["ops packs:harness/ops.ttl".to_string()],
        &[],
        &roots,
        &[],
        scene.home.path(),
    )
    .unwrap();
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes()).unwrap();
    let (watch, refused) = RootWatch::start(&roots);
    assert!(refused.is_empty(), "{refused:?}");
    let wired = browse::wire(
        roots.clone(),
        store.shared_writer().unwrap(),
        Some(&watch),
        None,
        &graph,
        None,
    );
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
    ));
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let names: Vec<String> = roots.iter().map(|(n, _)| n.clone()).collect();
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: names.into(),
        passkeys: Arc::new(Passkeys::new(layout, 1062)),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        display: Default::default(),
        page_assets: Default::default(),
        projects: Default::default(),
        toolbox: false,
        faces: Vec::new(),
        inquiry: None,
    });
    Served {
        http: Arc::new(doors::http_kernel(
            Arc::clone(&hub),
            web::space_with(face, Arc::new(packs)),
        )),
        _watch: watch,
    }
}

/// Reads the ledger and every root but `secret`.
fn reader() -> Capability {
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    for root in ["demo", "other", "packs"] {
        scopes.push(format!("{}{root}", ikigai_browse::CAP_PREFIX));
    }
    Capability::scoped(scopes)
}

impl Served {
    fn issue(&self, request: Request) -> Result<String, Error> {
        let (http, cap) = (&*self.http, reader());
        std::thread::scope(|s| {
            std::thread::Builder::new()
                .stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
                .spawn_scoped(s, move || block_on(Kernel::issue(http, request, &cap)))
                .unwrap()
                .join()
                .unwrap()
        })
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
    }

    /// `iri` in `face`, with `args`: a repeated name joined by a line break, as the HTTP door
    /// joins a declared newline-separated list.
    fn read(&self, iri: &str, face: &str, args: &[(&str, &str)]) -> Result<String, Error> {
        let mut joined: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (k, v) in args {
            joined.entry(k).or_default().push(v);
        }
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap())
            .with_arg("as", ArgRef::Inline(face.as_bytes().to_vec()));
        for (k, vs) in joined {
            request = request.with_arg(k, ArgRef::Inline(vs.join("\n").into_bytes()));
        }
        self.issue(request)
    }

    /// The Harness page of `demo` with these filters.
    fn page(&self, filters: &[(&str, &str)]) -> String {
        let mut args = vec![("root", "demo")];
        args.extend_from_slice(filters);
        self.read("urn:iki:gonk:page:harness", "text/html", &args)
            .unwrap()
            .replace('\'', "\"")
    }

    /// The HTTP door wired as `main` wires it, for an anonymous loopback caller that reads the
    /// ledger and every root but `secret`.
    fn door(&self) -> std::net::SocketAddr {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let mut anonymous = anonymous_http_grants(&["default".to_string()]).unwrap();
        for root in ["demo", "other", "packs"] {
            anonymous.push(format!("{}{root}", ikigai_browse::CAP_PREFIX));
        }
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: None,
            public: Vec::new(),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
            public_origin: None,
        };
        let edge = doors::edge_config(door.clone());
        let http = Arc::clone(&self.http);
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door),
                listener,
                edge,
            ))
        });
        addr
    }
}

/// `GET path` as a browser asks it: the status line and the body.
fn browser_get(addr: std::net::SocketAddr, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html,application/xhtml+xml\r\n\
         Connection: close\r\n\r\n",
        addr.port()
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (
        head.lines().next().unwrap_or("").to_string(),
        body.replace('\'', "\""),
    )
}

/// The ids of the rules a page lists: each guide heading not marked as shown only for its
/// controls, and each control listed under the guides.
fn shown(page: &str) -> BTreeSet<String> {
    let start = page.find("<h2 id=\"harness-guides\">").expect("the guides");
    let end = page.find("<h2 id=\"harness-days\">").expect("the days");
    let part = &page[start..end];
    let mut out = BTreeSet::new();
    for section in part.split("<section class=\"harness-group\"").skip(1) {
        let section = section.split("</section>").next().unwrap();
        if let Some(rest) = section.strip_prefix(" aria-labelledby=\"harness-guide-") {
            let id = rest.split('"').next().unwrap();
            if !section.contains("This guide is not in the filtered rules") {
                out.insert(id.to_string());
            }
        }
        for item in section.split("<li><a href=\"/harness/control/").skip(1) {
            out.insert(item.split('?').next().unwrap().to_string());
        }
    }
    out
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// Each choice the page offers: `(parameter, value, count)`.
fn choices(page: &str) -> Vec<(String, String, usize)> {
    let mut out = Vec::new();
    for at in page.split("<input type=\"checkbox\" name=\"").skip(1) {
        let name = at.split('"').next().unwrap().to_string();
        let value = at
            .split("value=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .replace("&amp;", "&");
        let count = at
            .split("<span class=\"facet-count\">(")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        out.push((name, value, count));
    }
    out
}

/// ★ Each filter narrows to the rules that hold its value, filters of two kinds narrow together,
/// and the repeats of one kind widen.
#[test]
fn each_filter_narrows_and_repeats_widen() {
    let scene = scene();
    let s = served(&scene);
    assert_eq!(shown(&s.page(&[])), set(&ALL));
    for (filters, expected) in [
        (
            vec![("pack", "ops")],
            vec!["one-gate", "gate-newline", "daily-scan"],
        ),
        (
            vec![("pack", "-")],
            vec![
                "claim-before-write",
                "reproduce-first",
                "small-commits",
                "require-claim",
                "big-diff",
                "never-fires",
                "layering",
            ],
        ),
        (
            vec![("category", "behavior")],
            vec!["reproduce-first", "one-gate", "gate-newline"],
        ),
        (
            vec![("stage", "preCommit")],
            vec!["small-commits", "big-diff", "never-fires"],
        ),
        (
            vec![("direction", "feedforward")],
            vec![
                "claim-before-write",
                "reproduce-first",
                "small-commits",
                "one-gate",
            ],
        ),
        (
            vec![("execution", "computational")],
            vec![
                "require-claim",
                "big-diff",
                "never-fires",
                "layering",
                "gate-newline",
                "daily-scan",
            ],
        ),
        (
            vec![("enforced", "tools/gonk.py#check_diff")],
            vec!["big-diff"],
        ),
        (
            vec![("fired", "fired")],
            vec!["require-claim", "big-diff", "gate-newline"],
        ),
        (vec![("fired", "refused")], vec!["require-claim"]),
        (vec![("fired", "warned")], vec!["big-diff", "gate-newline"]),
        (
            vec![("fired", "never")],
            vec![
                "claim-before-write",
                "reproduce-first",
                "small-commits",
                "never-fires",
                "layering",
                "one-gate",
                "daily-scan",
            ],
        ),
        (
            vec![("gap", "unchecked-guide")],
            vec!["reproduce-first", "small-commits"],
        ),
        (
            vec![("gap", "unstated-control")],
            vec!["big-diff", "never-fires", "layering", "daily-scan"],
        ),
        (vec![("q", "secrets")], vec!["never-fires"]),
        // A description's words count too.
        (vec![("q", "Depend DOWNWARD")], vec!["layering"]),
        // Two kinds narrow (AND).
        (
            vec![("category", "maintainability"), ("stage", "preCommit")],
            vec!["small-commits", "big-diff"],
        ),
        (
            vec![("pack", "ops"), ("direction", "feedback")],
            vec!["gate-newline", "daily-scan"],
        ),
        // Repeats of one kind widen (OR).
        (
            vec![
                ("category", "behavior"),
                ("category", "architectureFitness"),
            ],
            vec!["reproduce-first", "one-gate", "gate-newline", "layering"],
        ),
        (
            vec![("q", "secrets"), ("q", "layers")],
            vec!["never-fires", "layering"],
        ),
        (
            vec![
                ("stage", "ci"),
                ("stage", "dailyDrift"),
                ("fired", "never"),
                ("fired", "warned"),
            ],
            vec!["layering", "daily-scan"],
        ),
    ] {
        let page = s.page(&filters);
        let expected = set(&expected);
        assert_eq!(shown(&page), expected, "{filters:?}");
        let status = format!(
            "<p class=\"flash\">{} of 10 rules, filtered by: ",
            expected.len()
        );
        assert!(page.contains(&status), "{filters:?}: `{status}`:\n{page}");
    }
    // A guide outside the filter is drawn for its controls that are in it, and says so.
    let page = s.page(&[("execution", "computational"), ("pack", "ops")]);
    assert!(
        page.contains("<h3 id=\"harness-guide-one-gate\">"),
        "{page}"
    );
    assert!(page.contains("This guide is not in the filtered rules"));
    // The words of the status line: the vocabulary's labels, OR within a kind.
    let page = s.page(&[
        ("category", "behavior"),
        ("category", "authoritySafety"),
        ("stage", "toolCall"),
    ]);
    assert!(
        page.contains("5 of 10 rules, filtered by: behavior or authority/safety, tool-call."),
        "{page}"
    );
    // A value outside a fixed kind's terms is refused, naming them.
    let refused = s
        .read(
            "urn:iki:gonk:page:harness",
            "text/html",
            &[("root", "demo"), ("direction", "sideways")],
        )
        .unwrap_err();
    assert!(
        refused.to_string().contains("feedforward, feedback"),
        "{refused}"
    );
}

/// ★ `gap=` shows exactly the rows of the coverage questions it names, and the coverage table and
/// the days count only the filtered rules.
#[test]
fn a_gap_is_its_coverage_questions_answer_and_the_tables_follow_the_filter() {
    let scene = scene();
    let s = served(&scene);
    for (gap, question, column) in [
        ("unchecked-guide", "guides-without-sensor", "guide"),
        ("unstated-control", "sensors-without-guide", "sensor"),
    ] {
        let answer: Value = serde_json::from_str(
            &s.read(
                &format!("urn:iki:harness:coverage:query:{question}"),
                "application/json",
                &[("root", "demo")],
            )
            .unwrap(),
        )
        .unwrap();
        let answered: BTreeSet<String> = answer["results"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                let iri = b[column]["value"].as_str().unwrap();
                iri.rsplit(':').next().unwrap().to_string()
            })
            .collect();
        assert_eq!(shown(&s.page(&[("gap", gap)])), answered, "{gap}");
    }
    // Whole, two days; filtered to the pack, the one day its control fired.
    let whole = s.page(&[]);
    assert!(
        whole.contains("<th scope=\"row\">2026-11-15</th><td><a href=\"/harness/firing/2026-11-15?root=demo\">1 refusal, 1 warning</a>"),
        "{whole}"
    );
    let ops = s.page(&[("pack", "ops")]);
    assert!(
        ops.contains("<th scope=\"row\">2026-11-14</th><td><a href=\"/harness/firing/2026-11-14?root=demo&amp;pack=ops\">1 warning</a>"),
        "{ops}"
    );
    assert!(!ops.contains("2026-11-15</th>"), "{ops}");
    assert!(ops.contains("<caption>Each coverage question, and how many answers it has among the filtered rules</caption>"));
    // Coverage counts over the filtered rules: of the pack's, one sensor has no guide.
    assert!(
        ops.contains("<th scope=\"row\">Sensors without a guide</th><td><a href=\"/harness/coverage/sensors-without-guide?root=demo&amp;pack=ops\">1 sensor</a>"),
        "{ops}"
    );
    assert!(
        whole.contains("<th scope=\"row\">Sensors without a guide</th><td><a href=\"/harness/coverage/sensors-without-guide?root=demo\">4 sensors</a>"),
        "{whole}"
    );
    // The question's page a filtered count links to lists the same rows.
    let coverage = s
        .read(
            "urn:iki:harness:coverage:query:sensors-without-guide",
            "text/html",
            &[("root", "demo"), ("pack", "ops")],
        )
        .unwrap()
        .replace('\'', "\"");
    assert!(coverage.contains("<p>1 sensor.</p>"), "{coverage}");
    assert!(coverage.contains("scan: dependency drift"));
    assert!(!coverage.contains("gate: secrets in a commit"));
    // The machine face is the whole answer, as before.
    let json: Value = serde_json::from_str(
        &s.read(
            "urn:iki:harness:coverage:query:sensors-without-guide",
            "application/json",
            &[("root", "demo"), ("pack", "ops")],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(json["results"]["bindings"].as_array().unwrap().len(), 4);
}

/// ★ Each choice says how many rules it would show with the other kinds' filters kept, and that
/// is the number of rules its page shows; a choice that matches nothing says 0.
#[test]
fn the_counts_on_the_choices_match_the_rows_they_show() {
    let scene = scene();
    let s = served(&scene);
    for base in [
        vec![],
        vec![("stage", "toolCall")],
        vec![("pack", "ops"), ("fired", "never")],
    ] {
        let page = s.page(&base);
        let offered = choices(&page);
        // Every kind is offered, each a fieldset with a legend.
        for legend in [
            "Pack",
            "Category",
            "Lifecycle stage",
            "Direction",
            "Execution",
            "Enforced at",
            "Firings in the log",
            "Coverage gap",
        ] {
            assert!(
                page.contains(&format!(
                    "<fieldset class=\"harness-facet\"><legend>{legend}</legend>"
                )),
                "{legend}"
            );
        }
        assert!(offered.len() > 20, "{offered:?}");
        for (name, value, count) in offered {
            let mut filters: Vec<(&str, &str)> =
                base.iter().filter(|(k, _)| *k != name).copied().collect();
            filters.push((&name, &value));
            assert_eq!(
                shown(&s.page(&filters)).len(),
                count,
                "{name}={value} over {base:?}"
            );
        }
    }
    // A choice that would match nothing is drawn with 0, labeled in words.
    let page = s.page(&[("stage", "toolCall")]);
    assert!(
        page.contains("<input type=\"checkbox\" name=\"category\" value=\"architectureFitness\"> <span class=\"facet-term\">architecture fitness</span> <span class=\"facet-count\">(0 rules)</span>"),
        "{page}"
    );
    // The chosen value is checked, and each choice says its count in words.
    assert!(
        page.contains("<input type=\"checkbox\" name=\"stage\" value=\"toolCall\" checked> <span class=\"facet-term\">tool-call</span> <span class=\"facet-count\">(5 rules)</span>"),
        "{page}"
    );
    assert!(page.contains("<span class=\"facet-term\">\u{201c}ops rules\u{201d} 2.0</span>"));
    assert!(page.contains("<span class=\"facet-term\">this repository&#39;s own harness</span>"));
}

/// ★ The picker offers each root this caller may read that has a harness, says which root and
/// packs are shown, and a root the caller may not read is neither offered nor reachable.
#[test]
fn the_picker_offers_readable_harnesses_and_root_cannot_be_forced() {
    let scene = scene();
    let s = served(&scene);
    let page = s.page(&[]);
    assert!(
        page.contains("<select id=\"harness-root\" name=\"root\" aria-describedby=\"harness-root-help\"><option value=\"demo\" selected>demo</option><option value=\"other\">other</option></select>"),
        "{page}"
    );
    assert!(!page.contains("value=\"secret\""), "{page}");
    assert!(
        page.contains("Showing the harness of \u{201c}demo\u{201d}: its own .gonk/harness/harness.ttl, and the pack \u{201c}ops rules\u{201d} 2.0."),
        "{page}"
    );
    let other = s
        .read(
            "urn:iki:gonk:page:harness",
            "text/html",
            &[("root", "other")],
        )
        .unwrap();
    assert!(other.contains("be kind"), "{other}");
    assert!(other.contains("<option value=\"other\" selected>other</option>"));
    for args in [
        vec![("root", "secret")],
        vec![("root", "secret"), ("category", "behavior")],
    ] {
        let page = s
            .read("urn:iki:gonk:page:harness", "text/html", &args)
            .unwrap();
        assert!(!page.contains("the secret rule"), "{page}");
        assert!(!page.contains("harness-coverage"), "{page}");
        assert!(
            page.contains("grant doesn") || page.contains("Sign in"),
            "{page}"
        );
    }
    let refused = s
        .read(
            "urn:iki:harness:guide:id:hidden",
            "application/json",
            &[("root", "secret")],
        )
        .unwrap_err();
    assert!(matches!(refused, Error::Denied(_)), "{refused:?}");
}

/// ★ A filtered view is a link: through the HTTP door, repeated parameters reach the page, every
/// detail link carries them, a detail page links back with them, and following that link draws
/// the same page. Clearing every filter draws the page as it is with none.
#[test]
fn a_filtered_link_round_trips_and_the_cleared_view_is_the_whole_page() {
    let scene = scene();
    let s = served(&scene);
    let addr = s.door();
    let query = "root=demo&category=behavior&category=authoritySafety&stage=toolCall&q=hook";
    let (status, page) = browser_get(addr, &format!("/harness?{query}"));
    assert!(status.contains("200"), "{status}\n{page}");
    assert_eq!(
        shown(&page),
        set(&["require-claim", "gate-newline"]),
        "{page}"
    );
    let carried = query.replace('&', "&amp;");
    let control = format!("/harness/control/require-claim?{carried}");
    assert!(page.contains(&format!("href=\"{control}\"")), "{page}");
    // The detail page, and its link back to this very view.
    let (status, detail) = browser_get(addr, &control.replace("&amp;", "&"));
    assert!(status.contains("200"), "{status}\n{detail}");
    let back = format!(
        "<a href=\"/harness?{carried}\">Back to the filtered harness of &quot;demo&quot;</a>"
    );
    assert!(detail.contains(&back), "{detail}");
    let (_, again) = browser_get(addr, &format!("/harness?{query}"));
    assert_eq!(shown(&again), shown(&page));
    assert!(again.contains("2 of 10 rules, filtered by: behavior or authority/safety, tool-call, the words \u{201c}hook\u{201d}."));
    // Each other detail page links back with the filters too.
    for path in [
        "/harness/guide/one-gate",
        "/harness/incident/unclaimed-edit",
        "/harness/coverage/top-recurring",
        "/harness/firing/2026-11-14",
    ] {
        let (status, detail) = browser_get(addr, &format!("{path}?{query}"));
        assert!(status.contains("200"), "{path}: {status}\n{detail}");
        assert!(detail.contains(&back), "{path}:\n{detail}");
    }
    // The one link that clears every filter, and what it draws.
    let clear = "<a href=\"/harness?root=demo\">Clear every filter</a> to show all 10 rules.";
    assert!(page.contains(clear), "{page}");
    let (_, cleared) = browser_get(addr, "/harness?root=demo");
    let (_, plain) = browser_get(addr, "/harness?root=demo&q=");
    assert_eq!(cleared, plain);
    assert_eq!(shown(&cleared), set(&ALL));
    assert!(!cleared.contains("Clear every filter"));
    assert!(!cleared.contains("filtered"), "{cleared}");
    // The cleared view keeps the page as it was: the same sections, links and words.
    for needle in [
        "<h2 id=\"harness-coverage\">Coverage</h2>",
        "<caption>Each coverage question, and how many answers it has</caption>",
        "<h2 id=\"harness-guides\">Guides and the controls that check them</h2><p>A guide is a rule the agents are told; under each are the controls that state it.</p>",
        "<h3 id=\"harness-guide-claim-before-write\"><a href=\"/harness/guide/claim-before-write?root=demo\">no claim, no write</a></h3>",
        "<h3 id=\"harness-unstated\">Controls no guide states</h3>",
        "<caption>Refusals and warnings per day (UTC), newest first</caption>",
        "<div aria-live=\"polite\" class=\"flash-area\" id=\"status\" role=\"status\"></div>",
    ] {
        assert!(cleared.contains(needle), "`{needle}`:\n{cleared}");
    }
}

/// ★ A guide left unchecked on purpose says why, on the index and on its own page; a guide with
/// no reason keeps the plain sentence, and the unchecked guides' view says which have none.
#[test]
fn an_unchecked_guide_shows_its_reason_or_the_plain_sentence() {
    let scene = scene();
    let s = served(&scene);
    let page = s.page(&[]);
    let section = |page: &str, id: &str| -> String {
        let at = page
            .find(&format!("aria-labelledby=\"harness-guide-{id}\""))
            .unwrap();
        page[at..].split("</section>").next().unwrap().to_string()
    };
    let deliberate = section(&page, "small-commits");
    assert!(
        deliberate.contains("<p>Unchecked because: a reviewer judges what small means</p>"),
        "{deliberate}"
    );
    assert!(!deliberate.contains("No control checks this guide."));
    let oversight = section(&page, "reproduce-first");
    assert!(
        oversight.contains("<p>No control checks this guide.</p>"),
        "{oversight}"
    );
    assert!(!oversight.contains("Unchecked because"));
    assert!(!oversight.contains("No reason is stated"));
    // The gap's own view tells a deliberate gap from an oversight.
    let gaps = s.page(&[("gap", "unchecked-guide")]);
    assert!(section(&gaps, "small-commits").contains("Unchecked because: a reviewer judges"));
    assert!(
        section(&gaps, "reproduce-first").contains("No reason is stated for leaving it unchecked."),
        "{gaps}"
    );
    assert!(!section(&gaps, "small-commits").contains("No reason is stated"));
    // Each guide's page.
    let guide = |id: &str| {
        s.read(
            &format!("urn:iki:harness:guide:id:{id}"),
            "text/html",
            &[("root", "demo")],
        )
        .unwrap()
        .replace('\'', "\"")
    };
    let deliberate = guide("small-commits");
    assert!(
        deliberate.contains("<h2 id=\"harness-controls\">Controls that enforce it</h2><p>Unchecked because: a reviewer judges what small means</p>"),
        "{deliberate}"
    );
    let oversight = guide("reproduce-first");
    assert!(
        oversight.contains(
            "<h2 id=\"harness-controls\">Controls that enforce it</h2><p>No control checks this guide.</p>"
        ),
        "{oversight}"
    );
    // The coverage question selects the reason, bound for one guide and not the other.
    let answer: Value = serde_json::from_str(
        &s.read(
            "urn:iki:harness:coverage:query:guides-without-sensor",
            "application/json",
            &[("root", "demo")],
        )
        .unwrap(),
    )
    .unwrap();
    let reasons: Vec<(String, Option<String>)> = answer["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["guide"]["value"].as_str().unwrap().to_string(),
                b["reason"]["value"].as_str().map(str::to_string),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        vec![
            (
                "urn:iki:harness:demo:guide:reproduce-first".to_string(),
                None
            ),
            (
                "urn:iki:harness:demo:guide:small-commits".to_string(),
                Some("a reviewer judges what small means".to_string())
            ),
        ]
    );
}

/// ★ The filters are inputs of the page resource, declared so Meta describes them: repeatable,
/// with the fixed kinds' terms as `one_of`.
#[test]
fn meta_describes_the_filters() {
    let scene = scene();
    let s = served(&scene);
    let meta = s
        .issue(Request::new(
            Verb::Meta,
            Iri::parse("urn:iki:gonk:page:harness").unwrap(),
        ))
        .unwrap();
    for name in [
        "pack",
        "category",
        "stage",
        "direction",
        "execution",
        "enforced",
        "fired",
        "gap",
        "q",
    ] {
        assert!(meta.contains(&format!("\"{name}\"")), "{name}:\n{meta}");
    }
    for term in [
        "feedforward",
        "computational",
        "refused",
        "unchecked-guide",
        "unstated-control",
    ] {
        assert!(meta.contains(term), "{term}:\n{meta}");
    }
}
