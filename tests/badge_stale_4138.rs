//! Ledger #4138: **the Queue badge answers from its last count while a recount runs.**
//!
//! The badge counts pending findings across every readable root through the Queue's memo,
//! stamped with each root's epoch. Several events move EVERY root's epoch at once — a write
//! through the store's door that can reach browse's graph is one — and on `main` the next
//! poll then re-read every root before it answered: ~0.85 s a root on the live dataset, 8 at
//! a time, the 19 s outliers in the door log.
//!
//! - [`a_poll_after_a_write_to_every_root_answers_from_the_last_count`] — on a runtime (the
//!   HTTP door's case) the poll after such a write answers at once with the count it held,
//!   says how old that count is, and the recount it started lands for a later poll.
//! - [`off_a_runtime_the_badge_still_reads_before_it_answers`] — with nowhere to run a
//!   recount in the background, nothing changes: the poll reads, and is fresh.
//! - [`a_recount_after_a_write_to_every_root_is_timed`] (ignored; run with `--release
//!   --include-ignored --nocapture`) — the before and after, measured on a scratch store.
//!
//! Every store here is in memory, so these run beside a live gonk.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::config::QueuePolicy;
use ikigai_gonk::grants::{browse_graph_grants, grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::queue;
use ikigai_gonk::trigger::Trigger;
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_store::DurableStore;
use tempfile::TempDir;

/// N scratch repositories, one file each, and the badge's door over all of them, watched.
struct Fixture {
    door: Kernel,
    _roots: Vec<TempDir>,
    _spaces: TempDir,
    _config: TempDir,
    _watch: ikigai_gonk::watch::RootWatch,
}

fn name(n: usize) -> String {
    format!("root{n:02}")
}

fn fixture(n: usize) -> Fixture {
    let dirs: Vec<TempDir> = (0..n)
        .map(|_| {
            let dir = tempfile::tempdir().expect("a temp dir");
            std::fs::create_dir(dir.path().join("src")).expect("src/");
            std::fs::write(dir.path().join("src/lib.rs"), "the first version\n").expect("lib.rs");
            dir
        })
        .collect();
    let roots: Vec<(String, PathBuf)> = dirs
        .iter()
        .enumerate()
        .map(|(i, d)| (name(i), d.path().to_path_buf()))
        .collect();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };
    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let (watch, refused) = ikigai_gonk::watch::RootWatch::start(&roots);
    assert!(refused.is_empty(), "{refused:?}");
    let wired = browse::wire(
        roots.clone(),
        store.shared_writer().expect("a writer"),
        Some(&watch),
        None,
        &graph,
        None,
    );
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        ikigai_gonk::trigger::space(
            &trigger,
            Arc::new(ikigai_gonk::trigger::Activity::default()),
            false,
            QueuePolicy::default(),
        ),
        None,
        None,
        None,
    ));
    let config = tempfile::tempdir().expect("a config home");
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: roots
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>()
            .into(),
        passkeys: Arc::new(Passkeys::new(
            quic::Layout::in_config_home(config.path()),
            1060,
        )),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: QueuePolicy::default(),
        epochs: Some(watch.epochs()),
        holder_aliases: Default::default(),
        display: Default::default(),
        page_assets: Default::default(),
        projects: Default::default(),
        toolbox: false,
        faces: Vec::new(),
        inquiry: None,
    });
    Fixture {
        door: doors::http_kernel(Arc::clone(&hub), web::space(face)),
        _roots: dirs,
        _spaces: spaces,
        _config: config,
        _watch: watch,
    }
}

/// A grant that can read every root and write browse's graph.
fn reviewer() -> Capability {
    let mut scopes = vec![
        ikigai_browse::CAP_WILDCARD.to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ];
    scopes.extend(grants_for_all(&["default".to_string()], Authority::Write).expect("tokens"));
    scopes.extend(browse_graph_grants(Authority::Write).expect("a named browse graph"));
    Capability::scoped(scopes)
}

fn request(iri: &str, verb: Verb, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).expect("an IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

/// A serious severity word, asked of the finding contract.
fn serious(door: &Kernel) -> String {
    let declared = queue::one_of(
        door,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        "severity",
    )
    .expect("the finding Sink declares its severity set");
    let policy = QueuePolicy::default();
    declared
        .into_iter()
        .find(|w| policy.is_serious(w))
        .expect("a serious word")
}

/// `count` pending findings on `root`, in ONE write through the store's door — which is
/// observed, and names no root: it moves every root's epoch.
async fn plant(door: &Kernel, root: &str, first: usize, count: usize, word: &str) {
    let graph = browse::Graph::chosen()
        .named()
        .expect("a named browse graph")
        .as_str()
        .to_string();
    let mut data = String::new();
    for n in first..first + count {
        let id = format!("aaaabbbbccccdddd{n:08}");
        data.push_str(&format!(
            r#"  <urn:iki:finding:{id}> a prov:Entity ;
    dcterms:description "Claim {n}." ;
    dcterms:creator "a-test-reviewer" ;
    dcterms:created "2026-11-24T12:00:00Z"^^xsd:dateTime ;
    prov:wasGeneratedBy <urn:ikigai:browse:review:{root}:src/lib.rs> ;
    sh:resultSeverity <urn:iki:severity:{word}> ;
    ik:annotates <urn:repo:{root}:file:src/lib.rs> ;
    ik:repo "{root}" ;
    ik:path "src/lib.rs" ;
    ik:contentHash "sha256:planted" ;
    oa:hasSelector <urn:iki:finding:{id}:selector:quote> .
  <urn:iki:finding:{id}:selector:quote> a oa:TextQuoteSelector ;
    oa:exact "first version" .
"#
        ));
    }
    let update = format!(
        r#"PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX sh: <http://www.w3.org/ns/shacl#>
PREFIX oa: <http://www.w3.org/ns/oa#>
PREFIX ik: <https://ikigai-rs.dev/ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
INSERT DATA {{ GRAPH <{graph}> {{
{data}}} }}"#
    );
    door.issue(
        request(
            "urn:iki:store:graph-update",
            Verb::Sink,
            &[("graph", &graph), ("content", &update)],
        ),
        &reviewer(),
    )
    .await
    .expect("the browse graph's write token plants the findings");
}

async fn badge(door: &Kernel) -> String {
    let answer = door
        .issue(request(queue::BADGE_IRI, Verb::Source, &[]), &reviewer())
        .await
        .unwrap_or_else(|e| panic!("the badge: {e}"));
    String::from_utf8(answer.bytes).expect("utf-8")
}

/// The badge's serious count, as drawn.
fn serious_count(markup: &str) -> usize {
    let at = markup.find("class='serious'>").expect("the serious count") + 16;
    markup[at..]
        .split('<')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a number: {markup}"))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("a runtime")
}

/// Poll until the badge draws `want`, or panic after a generous deadline.
async fn poll_until(door: &Kernel, want: usize) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let markup = badge(door).await;
        if serious_count(&markup) == want {
            return markup;
        }
        assert!(
            Instant::now() < deadline,
            "the recount never landed: {markup}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// ★★ **The poll after a write that moved every root answers from the last count, and says
/// how old it is; the recount it started lands for a later poll.** On `main` this poll read
/// all three roots again before it answered, and drew the new count.
#[test]
fn a_poll_after_a_write_to_every_root_answers_from_the_last_count() {
    let fixture = fixture(3);
    let door = &fixture.door;
    let word = serious(door);
    runtime().block_on(async {
        for r in 0..3 {
            plant(door, &name(r), r * 10, 1, &word).await;
        }
        // Cold: nothing held, so the first poll reads, and draws no age.
        let cold = badge(door).await;
        assert_eq!(serious_count(&cold), 3, "{cold}");
        assert!(
            !cold.contains("Counted "),
            "a fresh count has no age: {cold}"
        );

        // One more finding through the store's door: every root's epoch moves.
        plant(door, &name(0), 100, 1, &word).await;
        let stale = badge(door).await;
        assert_eq!(
            serious_count(&stale),
            3,
            "the poll answered from the count it held instead of re-reading every root: {stale}"
        );
        assert!(
            stale.contains("Counted ") && stale.contains("ago; a recount is running."),
            "and said how old that count is: {stale}"
        );

        // The recount it started lands, and the next fresh poll draws it with no age.
        let fresh = poll_until(door, 4).await;
        assert!(!fresh.contains("Counted "), "{fresh}");
    });
}

/// Off a runtime there is nowhere for a recount to run in the background, so the badge reads
/// before it answers, exactly as before (every in-process test elsewhere relies on it).
#[test]
fn off_a_runtime_the_badge_still_reads_before_it_answers() {
    let fixture = fixture(2);
    let door = &fixture.door;
    let word = serious(door);
    futures::executor::block_on(async {
        plant(door, &name(0), 0, 1, &word).await;
        assert_eq!(serious_count(&badge(door).await), 1);
        plant(door, &name(1), 10, 1, &word).await;
        let markup = badge(door).await;
        assert_eq!(serious_count(&markup), 2, "{markup}");
        assert!(!markup.contains("Counted "), "{markup}");
    });
}

/// The before and after on a scratch store: 47 roots (the live count), each with findings,
/// then one write through the store's door and the time of the next poll.
///
/// `cargo test --release --test badge_stale_4138 -- --include-ignored --nocapture`
#[test]
#[ignore = "a measurement; run with --release --include-ignored --nocapture"]
fn a_recount_after_a_write_to_every_root_is_timed() {
    let size = |var: &str, default: usize| {
        std::env::var(var)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let (roots, per_root) = (size("GONK_4138_ROOTS", 47), size("GONK_4138_PER_ROOT", 40));
    let fixture = fixture(roots);
    let door = &fixture.door;
    let word = serious(door);
    runtime().block_on(async {
        for r in 0..roots {
            plant(door, &name(r), r * per_root, per_root, &word).await;
        }
        let started = Instant::now();
        let cold = badge(door).await;
        let cold_took = started.elapsed();
        assert_eq!(serious_count(&cold), roots * per_root);
        let mut after = Vec::new();
        for round in 0..5 {
            plant(door, &name(round), 100_000 + round, 1, &word).await;
            let started = Instant::now();
            let _ = badge(door).await;
            after.push(started.elapsed());
            poll_until(door, roots * per_root + round + 1).await;
        }
        after.sort();
        println!(
            "{roots} roots x {per_root} findings: cold poll {cold_took:?}; \
             poll after a write to every root: median {:?}, max {:?}",
            after[after.len() / 2],
            after[after.len() - 1]
        );
    });
}

/// ★ **A change to the set of roots touches only the roots it changed** (ledger #4138, one of
/// the every-root sources). Mounting a root beside others used to move every epoch, so the
/// next poll re-read every root; now a root that stayed at the same directory keeps its
/// epoch, and one that arrived, left or moved has its epoch moved.
#[test]
fn tracking_the_roots_touches_only_those_that_changed() {
    use ikigai_gonk::watch::Epochs;
    let at = |dir: &str| PathBuf::from(dir);
    let epochs = Epochs::default();
    epochs.track(&[("a".into(), at("/a")), ("b".into(), at("/b"))]);
    let (a, b) = (epochs.stamp("a"), epochs.stamp("b"));
    assert!(a.is_some() && b.is_some());

    // The same set again: nothing moved.
    epochs.track(&[("a".into(), at("/a")), ("b".into(), at("/b"))]);
    assert_eq!((epochs.stamp("a"), epochs.stamp("b")), (a, b));

    // `c` arrives: `a` and `b` stay where they were.
    epochs.track(&[
        ("a".into(), at("/a")),
        ("b".into(), at("/b")),
        ("c".into(), at("/c")),
    ]);
    assert_eq!((epochs.stamp("a"), epochs.stamp("b")), (a, b));
    let c = epochs.stamp("c").expect("a counter for the new root");

    // `b` moves to another directory and `c` leaves: both move, `a` does not.
    epochs.track(&[("a".into(), at("/a")), ("b".into(), at("/elsewhere"))]);
    assert_eq!(epochs.stamp("a"), a);
    assert_ne!(epochs.stamp("b"), b, "a root at another directory moved");
    let left = epochs.stamp("c").expect("a removed root keeps its counter");
    assert_ne!(left, c, "a root that left moved");

    // `c` comes back: past every stamp it held before it left.
    epochs.track(&[
        ("a".into(), at("/a")),
        ("b".into(), at("/elsewhere")),
        ("c".into(), at("/c")),
    ]);
    assert!(
        epochs.stamp("c") > Some(left),
        "a re-added root moved again"
    );
    assert_eq!(epochs.stamp("a"), a);
}
