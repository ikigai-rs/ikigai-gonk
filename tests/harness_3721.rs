//! Ledger #3721: a repository's harness and its firings, served as resources and drawn on the
//! Harness page, read under the caller's grant.
//!
//! The repository is a scratch one in the shape every gonk-starter repository has:
//! `.gonk/harness/harness.ttl` and a git-ignored `.gonk/state/harness/firings.log`.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
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

inc:unclaimed-edit a harness:Incident ;
    rdfs:label "an edit landed with no claim" .

inc:lost-lease a harness:Incident ;
    rdfs:label "a lease ran out mid-run" .

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
"#;

const CONTROL: &str = "urn:iki:harness:demo:control:";

fn stamp(ms: u64) -> String {
    // The fixed-width UTC column, from milliseconds: days to a civil date (Hinnant).
    let (secs, milli) = (ms / 1000, ms % 1000);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// A firing segment: `(ms, class, control id, cmd)` per line.
fn segment(lines: &[(u64, &str, &str, &str)]) -> String {
    let mut out = "# ikigai-log v1\n@prefix harness: <https://ikigai-rs.dev/ns/harness#> .\n\
         @prefix log:     <https://ikigai-rs.dev/ns/log#> .\n\
         @name     urn:log:harness:tree-demo\n@instance urn:iki:harness:tree:demo\n\
         @level    log:info\n@started  2026-11-01T00:00:00.000Z\n@prev     genesis\n\n"
        .to_string();
    for (seq, (ms, class, id, cmd)) in lines.iter().enumerate() {
        out.push_str(&format!(
            "{} harness:{class} {CONTROL}{id} seq={} cmd=sha256:{cmd}\n",
            stamp(*ms),
            seq + 1
        ));
    }
    out
}

const DAY_MS: u64 = 86_400_000;

fn scratch(firings: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".gonk/harness")).unwrap();
    std::fs::create_dir_all(dir.path().join(".gonk/state/harness")).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "/.gonk/state/\n").unwrap();
    std::fs::write(dir.path().join(".gonk/harness/harness.ttl"), HARNESS).unwrap();
    std::fs::write(dir.path().join(".gonk/state/harness/firings.log"), firings).unwrap();
    dir
}

struct Served {
    hub: Arc<Kernel>,
    http: Kernel,
    _watch: RootWatch,
}

fn served(dir: &TempDir) -> Served {
    let roots = vec![("demo".to_string(), dir.path().to_path_buf())];
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes()).unwrap();
    let (watch, refused) = RootWatch::start(&roots);
    assert!(refused.is_empty(), "{refused:?}");
    let wired = browse::wire(
        roots,
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
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: vec!["demo".to_string()].into(),
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
    });
    Served {
        http: doors::http_kernel(Arc::clone(&hub), web::space(face)),
        hub,
        _watch: watch,
    }
}

impl Served {
    fn read(&self, iri: &str, face: &str, cap: &Capability) -> Result<String, Error> {
        let request = Request::new(Verb::Source, Iri::parse(iri).unwrap())
            .with_arg("as", ArgRef::Inline(face.as_bytes().to_vec()));
        // A composition nests further than a debug build fits in a test thread's stack.
        let (http, cap) = (&self.http, cap.clone());
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

    /// A session summary in `default`'s session graph (where `ikigai-ledger` 0.23.0 records one,
    /// ledger #3661): the session `id` worked on item `item`.
    fn summarize(&self, id: &str, item: u64) {
        let graph = ikigai_ledger::Ledger::parse("default")
            .unwrap()
            .session_graph();
        let update = format!(
            "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>\n\
             INSERT DATA {{ GRAPH <{graph}> {{ <urn:agent:session:id:{id}> a ledger:SessionSummary ; \
             ledger:sessionId \"{id}\" ; \
             ledger:sessionItem <urn:iki:ledger:default:item:{item}> }} }}"
        );
        let request = Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:store:graph-update").unwrap(),
        )
        .with_arg("graph", ArgRef::Inline(graph.into_bytes()))
        .with_arg("content", ArgRef::Inline(update.into_bytes()));
        block_on(Kernel::issue(&self.hub, request, &Capability::root())).unwrap();
    }

    fn json(&self, iri: &str, cap: &Capability) -> Value {
        serde_json::from_str(&self.read(iri, "application/json", cap).unwrap()).unwrap()
    }
}

/// Reads the root and the ledger.
fn reader() -> Capability {
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.push(format!("{}demo", ikigai_browse::CAP_PREFIX));
    Capability::scoped(scopes)
}

/// Reads the ledger, and no repository.
fn ledger_only() -> Capability {
    Capability::scoped(grants_for("default", Authority::Read).unwrap())
}

fn rows(answer: &Value) -> usize {
    answer["results"]["bindings"].as_array().unwrap().len()
}

/// ★ The page draws every control under its stage and category, with its guide, incident,
/// proof and code site, and the coverage gaps, each a link to the question that answers it.
#[test]
fn a_repository_harness_renders_with_its_controls_and_gaps() {
    let now = now_ms();
    let dir = scratch(&segment(&[
        (now - 60_000, "Refusal", "require-claim", "aaa"),
        (now - 30_000, "Warning", "big-diff", "bbb"),
    ]));
    let s = served(&dir);
    let page = s
        .read("urn:iki:gonk:page:harness", "text/html", &reader())
        .unwrap();
    for needle in [
        "<h1 id=\"harness-title\">Harness</h1>",
        "Stage: tool-call",
        "Stage: pre-commit",
        "<caption>authority/safety, at tool-call</caption>",
        "hook: no claim, no write",
        "gate: secrets in a commit",
        // Its guide, its incident, its proof, its code site.
        ">no claim, no write</a>",
        ">an edit landed with no claim</a>",
        "fired by a test: SensorsFire.test_require_claim",
        "<code>tools/claim_hook.py#main</code>",
        "cannot fire in a test: the scanner is a third party&#39;s",
        // A gap is a number, linked to its question's answer.
        "<th scope=\"row\">Sensors without a guide</th><td><a href=\"/harness/coverage/sensors-without-guide?root=demo&amp;as=text%2Fplain\">2</a>",
        "<th scope=\"row\">Guides without a check</th><td><a href=\"/harness/coverage/guides-without-sensor?root=demo&amp;as=text%2Fplain\">1</a>",
        "<th scope=\"row\">Incidents without a sensor</th><td><a href=\"/harness/coverage/incidents-without-sensor?root=demo&amp;as=text%2Fplain\">1</a>",
        "<th scope=\"row\">Sensors never fired</th><td><a href=\"/harness/coverage/sensors-never-fired?root=demo&amp;as=text%2Fplain\">1</a>",
        "In the last 7 days the harness refused 1 action",
    ] {
        // The shell is written with single quotes, the body with double ones.
        let at = page.replace('\'', "\"");
        assert!(at.contains(needle), "`{needle}` is not on the page:\n{page}");
    }
    // Each gap's number is its question's row count.
    for (name, expected) in [
        ("sensors-without-guide", 2),
        ("guides-without-sensor", 1),
        ("incidents-without-sensor", 1),
        ("sensors-never-fired", 1),
    ] {
        let answer = s.json(&format!("urn:iki:harness:coverage:query:{name}"), &reader());
        assert_eq!(rows(&answer), expected, "{name}: {answer}");
    }
    // One control, in each face; the JSON says how often it fired and never which command.
    let control = s.json("urn:iki:harness:control:id:require-claim", &reader());
    assert_eq!(control["firings"], 1, "{control}");
    assert_eq!(
        control["properties"]["label"][0],
        "hook: no claim, no write"
    );
    let turtle = s
        .read(
            "urn:iki:harness:control:id:require-claim",
            "text/turtle",
            &reader(),
        )
        .unwrap();
    assert!(turtle.contains("harness:entryCount 1"), "{turtle}");
    let guide = s.json("urn:iki:harness:guide:id:claim-before-write", &reader());
    assert_eq!(guide["statedByBy"][0], format!("{CONTROL}require-claim"));
    let incident = s.json("urn:iki:harness:incident:id:lost-lease", &reader());
    assert_eq!(incident["existsForBy"].as_array().unwrap().len(), 0);
    for iri in [
        "urn:iki:gonk:page:harness",
        "urn:iki:harness:coverage:query:repeated-warnings",
        "urn:iki:harness:control:id:require-claim",
    ] {
        for face in ["text/html", "application/json", "text/turtle", "text/plain"] {
            if let Ok(text) = s.read(iri, face, &reader()) {
                assert!(!text.contains("sha256:"), "{iri} as {face} shows a digest");
            }
        }
    }
    let query = s
        .read(
            "urn:iki:harness:coverage:query:sensors-never-fired",
            "application/sparql-query",
            &reader(),
        )
        .unwrap();
    assert!(query.starts_with("# QUESTION:"), "{query}");
}

/// ★ A day's resource counts each control's refusals and warnings that UTC day, and the page's
/// days are the same numbers.
#[test]
fn firings_aggregate_per_day() {
    let day = 1_794_700_800_000; // 2026-11-15T00:00:00Z
    let dir = scratch(&segment(&[
        (day + 1_000, "Refusal", "require-claim", "a1"),
        (day + 2_000, "Refusal", "require-claim", "a2"),
        (day + 3_000, "Warning", "big-diff", "b1"),
        (day + 4_000, "Warning", "big-diff", "b1"),
        (day - DAY_MS + 5_000, "Refusal", "require-claim", "a3"),
    ]));
    let s = served(&dir);
    let today = s.json("urn:iki:harness:firing:date:2026-11-15", &reader());
    let mut got: Vec<(String, String, i64)> = today["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["control"].as_str().unwrap().to_string(),
                r["outcome"].as_str().unwrap().to_string(),
                r["count"].as_i64().unwrap(),
            )
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (format!("{CONTROL}big-diff"), "warn".to_string(), 2),
            (format!("{CONTROL}require-claim"), "refuse".to_string(), 2),
        ]
    );
    let yesterday = s.json("urn:iki:harness:firing:date:2026-11-14", &reader());
    assert_eq!(
        yesterday["rows"].as_array().unwrap().len(),
        1,
        "{yesterday}"
    );
    let turtle = s
        .read(
            "urn:iki:harness:firing:date:2026-11-15",
            "text/turtle",
            &reader(),
        )
        .unwrap();
    assert!(turtle.contains("harness:entryCount 2"), "{turtle}");
    let by_day = s.json("urn:iki:harness:coverage:query:firings-by-day", &reader());
    assert_eq!(rows(&by_day), 3, "{by_day}");
    let repeated = s.json(
        "urn:iki:harness:coverage:query:repeated-warnings",
        &reader(),
    );
    assert_eq!(rows(&repeated), 1, "{repeated}");
    let page = s
        .read("urn:iki:gonk:page:harness", "text/html", &reader())
        .unwrap();
    let href = "/harness/firing/2026-11-15?root=demo&amp;as=text%2Fplain";
    assert!(
        page.contains(&format!(
            "<th scope=\"row\">2026-11-15</th><td><a href=\"{href}\">2</a></td><td><a href=\"{href}\">2</a></td>"
        )),
        "{page}"
    );
    assert!(s
        .read(
            "urn:iki:harness:firing:date:2026-1-15",
            "application/json",
            &reader()
        )
        .is_err());
}

/// ★ A caller whose grant reads no repository sees no control, no count and no gap: the page
/// says what is missing, and every resource refuses.
#[test]
fn a_caller_without_the_read_grant_sees_nothing() {
    let dir = scratch(&segment(&[(now_ms(), "Refusal", "require-claim", "a")]));
    let s = served(&dir);
    let page = s
        .read("urn:iki:gonk:page:harness", "text/html", &ledger_only())
        .unwrap();
    assert!(page.contains("Your passkey"), "{page}");
    for absent in ["hook: no claim, no write", "Coverage", "Stage:", "refused"] {
        assert!(!page.contains(absent), "`{absent}` is on the page: {page}");
    }
    for iri in [
        "urn:iki:harness:control:id:require-claim",
        "urn:iki:harness:guide:id:claim-before-write",
        "urn:iki:harness:incident:id:unclaimed-edit",
        "urn:iki:harness:coverage:query:sensors-never-fired",
        "urn:iki:harness:firing:date:2026-11-15",
    ] {
        match s.read(iri, "application/json", &ledger_only()) {
            Err(Error::Denied(_)) => {}
            other => panic!("{iri}: {other:?}"),
        }
        // Naming the root does not help.
        let named = Request::new(Verb::Source, Iri::parse(iri).unwrap())
            .with_arg("root", ArgRef::Inline(b"demo".to_vec()));
        let answer = block_on(Kernel::issue(&s.http, named, &ledger_only()));
        assert!(matches!(answer, Err(Error::Denied(_))), "{iri}: {answer:?}");
    }
}

/// ★ An edit to harness.ttl shows on the next read, and a new firing too: no restart.
#[test]
fn an_edit_to_the_harness_shows_on_the_next_read() {
    let dir = scratch(&segment(&[(now_ms(), "Refusal", "require-claim", "a")]));
    let s = served(&dir);
    let iri = "urn:iki:harness:control:id:require-claim";
    let before = s.json(iri, &reader());
    assert_eq!(before["properties"]["label"][0], "hook: no claim, no write");
    assert_eq!(before["firings"], 1);
    // Read twice, so the answer is in the cache before the edit.
    s.json(iri, &reader());
    std::fs::write(
        dir.path().join(".gonk/harness/harness.ttl"),
        HARNESS.replace("hook: no claim, no write", "hook: claim first"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".gonk/state/harness/firings.log"),
        segment(&[
            (now_ms(), "Refusal", "require-claim", "a"),
            (now_ms(), "Refusal", "require-claim", "b"),
        ]),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let after = s.json(iri, &reader());
        if after["properties"]["label"][0] == "hook: claim first" {
            assert_eq!(after["firings"], 2, "{after}");
            break;
        }
        assert!(Instant::now() < deadline, "the edit never showed: {after}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let page = s
        .read("urn:iki:gonk:page:harness", "text/html", &reader())
        .unwrap();
    assert!(page.contains("hook: claim first"), "{page}");
}

/// ★ A refusal joins to the ledger item its session worked on, through the session summaries of
/// the ledgers the caller may read; the page says how many, never which session.
#[test]
fn firings_join_to_ledger_items_through_session_summaries() {
    let now = now_ms();
    let session = |id: &str| format!("x session=urn:agents:session:{id}");
    let (s1, s2, s3) = (session("s1"), session("s2"), session("s3"));
    let dir = scratch(&segment(&[
        (now - 4_000, "Refusal", "require-claim", &s1),
        (now - 3_000, "Refusal", "require-claim", &s1),
        (now - 2_000, "Refusal", "never-fires", &s2),
        (now - 1_000, "Refusal", "require-claim", &s3),
        (now - 500, "Warning", "big-diff", &s1),
        (now - 9 * DAY_MS, "Refusal", "require-claim", &s2),
    ]));
    let s = served(&dir);
    s.summarize("s1", 1);
    s.summarize("s2", 2);
    let page = s
        .read("urn:iki:gonk:page:harness", "text/html", &reader())
        .unwrap();
    let sentence = "In the last 7 days the harness refused 4 actions, 3 of them on 2 ledger items you may read.";
    assert!(page.contains(sentence), "{page}");
    assert!(!page.contains("urn:agents:session"), "{page}");
    // A reader whose grant was minted before ikigai-ledger 0.23.0 lacks the session graph's
    // token: the page still renders with the counts, joining nothing it may not read.
    let session_token = ikigai_store::cap_read_graph(
        &ikigai_ledger::Ledger::parse("default")
            .unwrap()
            .session_graph(),
    );
    let mut older: Vec<String> = grants_for("default", Authority::Read)
        .unwrap()
        .into_iter()
        .filter(|t| *t != session_token)
        .collect();
    older.push(format!("{}demo", ikigai_browse::CAP_PREFIX));
    let page = s
        .read(
            "urn:iki:gonk:page:harness",
            "text/html",
            &Capability::scoped(older),
        )
        .unwrap();
    assert!(
        page.contains(
            "refused 4 actions, none of them in a session a ledger you may read has a summary of."
        ),
        "{page}"
    );
    // A caller who reads the repository and no ledger has the counts and no join.
    let browse_only = Capability::scoped(vec![format!("{}demo", ikigai_browse::CAP_PREFIX)]);
    let page = s
        .read("urn:iki:gonk:page:harness", "text/html", &browse_only)
        .unwrap();
    assert!(
        page.contains(
            "refused 4 actions, none of them in a session a ledger you may read has a summary of."
        ),
        "{page}"
    );
}

/// ★ The firing log is git-ignored, so no watch cuts a thread for it: a new firing alone, with
/// `harness.ttl` untouched, still shows on the next read, because nothing that reads the log is
/// cached.
#[test]
fn a_new_firing_alone_shows_on_the_next_read() {
    let dir = scratch(&segment(&[(now_ms(), "Refusal", "require-claim", "a")]));
    let s = served(&dir);
    let control = "urn:iki:harness:control:id:require-claim";
    let fired = "urn:iki:harness:coverage:query:firings-by-control";
    assert_eq!(s.json(control, &reader())["firings"], 1);
    assert_eq!(rows(&s.json(fired, &reader())), 1);
    s.json(control, &reader());
    std::fs::write(
        dir.path().join(".gonk/state/harness/firings.log"),
        segment(&[
            (now_ms(), "Refusal", "require-claim", "a"),
            (now_ms(), "Warning", "big-diff", "b"),
            (now_ms(), "Refusal", "require-claim", "c"),
        ]),
    )
    .unwrap();
    assert_eq!(s.json(control, &reader())["firings"], 2);
    assert_eq!(rows(&s.json(fired, &reader())), 2);
}
