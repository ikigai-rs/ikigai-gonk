//! The browse family over gonk's ONE dataset: what it serves, where its quads land, what
//! the shared handle costs, and what the watcher buys.
//!
//! Five things are gonk's to get wrong here, and each is a test:
//!
//! - [`the_served_catalog_is_exactly_what_this_manifest_links`] — linking two more crates put
//!   twenty more resources behind the doors; the catalog is pinned id by id, as it was.
//! - [`a_browse_write_touches_no_named_graph`] — the ASSUMPTION [`ikigai_gonk::freshness`]
//!   rests on, checked at the boundary rather than trusted in a comment.
//! - [`a_ledger_item_joins_an_annotation_on_a_repo_file`] — the point of one dataset.
//! - [`the_shared_handle_costs_the_ledger_nothing`] — the naive composition beside the one
//!   this server builds, with the read timings printed.
//! - [`a_watched_file_read_recomputes_after_the_file_changes_on_disk`] — the cache that
//!   watcher makes safe, driven through the watcher's own notification path.
//!
//! Every store here is `in_memory_shared`, which carries the SAME coverage flag as the
//! durable `open_shared` (`covered: false`) and takes no RocksDB lock — so these run beside
//! a live gonk holding `~/.ikigai/store`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Fallback, Iri, Kernel, Representation, Request, Space, SystemClock, Verb,
};
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use tempfile::TempDir;

/// A scratch repository: two files, no git.
fn scratch_root() -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir(dir.path().join("src")).expect("src/");
    std::fs::write(dir.path().join("src/lib.rs"), "the first version\n").expect("lib.rs");
    std::fs::write(dir.path().join("README.md"), "# scratch\n").expect("README.md");
    dir
}

fn roots(dir: &TempDir) -> Vec<(String, PathBuf)> {
    vec![("demo".to_string(), dir.path().to_path_buf())]
}

/// The composition `main` builds when `gonk.browse.root` names a root: a shared store, the
/// browse family over the same dataset, and the freshness recovery.
fn served(dir: &TempDir) -> (Arc<Kernel>, RootWatch) {
    let (store, handle) = DurableStore::in_memory_shared().expect("a shared in-memory store");
    let (watch, refused) = RootWatch::start(&roots(dir));
    assert!(refused.is_empty(), "{refused:?}");
    let wired = browse::wire(roots(dir), handle, watch.watched());
    let hub = Arc::new(compose_with(store, Some(Arc::new(wired.space))));
    (hub, watch)
}

/// ★ The composition this arc REFUSED: `open_shared`, everything else as before, and no
/// freshness recovery. It exists so the cost of the easy path is a number in the test output
/// rather than an assertion about a counterfactual.
fn naive(dir: &TempDir) -> Arc<Kernel> {
    let (store, handle) = DurableStore::in_memory_shared().expect("a shared in-memory store");
    // No watcher either: a host that accepts the blanket has no reason to run one, and
    // `ikigai-browse`'s reads are live and uncacheable exactly as it declares them.
    let wired = browse::wire(roots(dir), handle, &[]);
    let space = Fallback::new(vec![
        Arc::new(ikigai_store::space(store)) as Arc<dyn Space>,
        Arc::new(ikigai_ledger::space()) as Arc<dyn Space>,
        Arc::new(wired.space) as Arc<dyn Space>,
    ]);
    Arc::new(
        Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer))
            .with_clock(Arc::new(SystemClock)),
    )
}

/// Today's gonk without a browse face: an OWNED store, whose reads `ikigai-store` declares
/// cacheable itself.
fn owned() -> Arc<Kernel> {
    Arc::new(ikigai_gonk::compose(
        DurableStore::in_memory().expect("an owned in-memory store"),
    ))
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn issue(kernel: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Representation {
    block_on(Kernel::issue(
        kernel,
        request(verb, iri, args),
        &Capability::root(),
    ))
    .unwrap_or_else(|e| panic!("{verb:?} {iri} {args:?}: {e}"))
}

fn text(kernel: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
    String::from_utf8_lossy(&issue(kernel, verb, iri, args).bytes).into_owned()
}

/// The dataset's named graphs, by the broad read door (root only).
fn named_graphs(kernel: &Kernel) -> Vec<String> {
    let answer = text(
        kernel,
        Verb::Source,
        "urn:iki:store:select",
        &[(
            "query",
            "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } } ORDER BY ?g",
        )],
    );
    let json: serde_json::Value = serde_json::from_str(&answer).expect("SPARQL results JSON");
    json["results"]["bindings"]
        .as_array()
        .expect("bindings")
        .iter()
        .map(|row| row["g"]["value"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The IRIs the kernel currently holds cached, from `urn:kernel:cache`.
fn cached(kernel: &Kernel) -> Vec<String> {
    text(kernel, Verb::Source, "urn:kernel:cache", &[])
        .lines()
        .skip(3) // `cache`, `entries`, `size`
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect()
}

/// File one annotation on a repo file, through the kernel.
fn annotate(kernel: &Kernel, id: &str, target: &str, quote: &str, note: &str) {
    text(
        kernel,
        Verb::Sink,
        &format!("urn:iki:annotation:{id}"),
        &[("target", target), ("exact", quote), ("body", note)],
    );
}

// ---------------------------------------------------------------- the catalog

/// Every resource the browse composition puts behind the doors. `explain`, `review` and the
/// PR-derived layers are deliberately absent: they derive through `urn:llm:*`, which this
/// binary does not link, and an action the kernel can never satisfy is an over-offer.
const BROWSE_IDS: [&str; 10] = [
    "annotation",
    "browse-annotations",
    "browse-file",
    "browse-hash",
    "browse-pr",
    "browse-prs",
    "browse-prs-scoped",
    "browse-state",
    "browse-style",
    "browse-tree",
];

/// `ikigai-repo`'s facades — bound only alongside a browse face, because that is what needs
/// them (browse's PR rows resolve `urn:repo:pr:*` through the kernel).
const REPO_IDS: [&str; 10] = [
    "repo-branch",
    "repo-list",
    "repo-log",
    "repo-pr-checks",
    "repo-pr-diff",
    "repo-pr-files",
    "repo-pr-list",
    "repo-pr-view",
    "repo-status",
    "system-exec",
];

/// The new families' description ids in a kernel's catalog, sorted and deduplicated (browse
/// enumerates per-root rows, so one endpoint appears under several patterns).
fn new_family_ids(kernel: &Kernel, expected: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = kernel
        .entries()
        .expect("an enumerable root")
        .iter()
        .filter(|entry| !entry.pattern.starts_with("urn:kernel:"))
        .filter_map(|entry| kernel.describe_pattern(&entry.pattern).map(|d| d.id))
        .filter(|id| expected.contains(id))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[test]
fn the_served_catalog_is_exactly_what_this_manifest_links() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let mut expected: Vec<String> = BROWSE_IDS
        .iter()
        .chain(REPO_IDS.iter())
        .map(|id| id.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        new_family_ids(&hub, &expected),
        expected,
        "the browse composition's catalog changed: a dependency bump gained or lost a \
         resource, and it is now behind every door this binary opens"
    );
    // The socket and QUIC doors forward to the hub, so they serve the same twenty — a
    // forwarding space that dropped a row would be clean above and dirty here.
    let door = ikigai_gonk::doors::door_kernel(Arc::clone(&hub));
    assert_eq!(new_family_ids(&door, &expected), expected);
}

// ------------------------------------------------- where browse's quads land

/// ★ The assumption [`ikigai_gonk::freshness`] is built on, pinned at the boundary: every
/// quad `ikigai-browse` writes goes into the store's DEFAULT graph, so no named-graph read —
/// which is every read the ledger makes and every read a scoped capability can reach — can
/// see a write made through the handle that left `ikigai-store`.
///
/// The day browse writes a named graph, this test is red HERE, where the assumption is made,
/// instead of silently stale in the cache of a running server.
#[test]
fn a_browse_write_touches_no_named_graph() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so a named graph exists at all")],
    );
    let before = named_graphs(&hub);
    assert_eq!(
        before,
        ["urn:iki:ledger:graph:default"],
        "the ledger's graph is the only named one"
    );

    annotate(
        &hub,
        "n1",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "a note",
    );
    // The annotation IS in the dataset — the broad read sees it…
    let answer = text(
        &hub,
        Verb::Source,
        "urn:iki:store:select",
        &[(
            "query",
            "SELECT ?a WHERE { ?a <https://ikigai-rs.dev/ns#annotates> ?t }",
        )],
    );
    assert!(answer.contains("urn:iki:annotation:n1"), "{answer}");
    // …and it is in no named graph, which is why a scoped read cannot see it.
    assert_eq!(
        named_graphs(&hub),
        before,
        "a browse write must not create or touch a named graph"
    );
}

/// The point of one dataset: a ledger item that is ABOUT a repo file joins an annotation on
/// that same file, in one query, with no federation.
///
/// ⚠ Under a ROOT capability, and that is not an accident of the test. The join has to read
/// the default graph (browse's quads) and a named graph (the ledger's) at once, which only
/// the broad `urn:iki:store:select` can do — and this server hands the broad store tokens to
/// nobody. The socket door is root; the HTTP door's anonymous caller holds per-graph tokens
/// and cannot run this query at all.
#[test]
fn a_ledger_item_joins_an_annotation_on_a_repo_file() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let file = "urn:repo:demo:file:src/lib.rs";
    let filed = text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "lib.rs needs a doc comment"), ("about", file)],
    );
    assert!(filed.contains("urn:iki:ledger:default:item:"), "{filed}");
    annotate(&hub, "n1", file, "first", "this line is the one");

    let answer = text(
        &hub,
        Verb::Source,
        "urn:iki:store:select",
        &[(
            "query",
            "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
             PREFIX ik: <https://ikigai-rs.dev/ns#>
             PREFIX oa: <http://www.w3.org/ns/oa#>
             SELECT ?item ?file ?note WHERE {
               GRAPH <urn:iki:ledger:graph:default> { ?item ledger:about ?file }
               ?annotation ik:annotates ?file ; oa:bodyValue ?note .
             }",
        )],
    );
    println!("--- the join ---\n{answer}");
    let json: serde_json::Value = serde_json::from_str(&answer).expect("SPARQL results JSON");
    let rows = json["results"]["bindings"].as_array().expect("bindings");
    assert_eq!(rows.len(), 1, "one item, one annotation, one row: {answer}");
    assert_eq!(rows[0]["file"]["value"], file);
    assert_eq!(rows[0]["note"]["value"], "this line is the one");
    assert!(
        rows[0]["item"]["value"]
            .as_str()
            .unwrap_or_default()
            .starts_with("urn:iki:ledger:default:item:"),
        "{answer}"
    );
}

// ------------------------------------------------------------ what it costs

/// A live-sized corpus: 247 items, the size of the ledger this server actually serves.
fn seed_items(kernel: &Kernel, n: usize) {
    for i in 0..n {
        text(
            kernel,
            Verb::Sink,
            "urn:iki:ledger:append",
            &[(
                "content",
                &format!("Item {i}\n\nA body long enough to be a real row in the graph."),
            )],
        );
    }
}

/// Time `n` reads of `iri`, after one warm-up read.
fn time_reads(kernel: &Kernel, iri: &str, n: u32) -> Duration {
    issue(kernel, Verb::Source, iri, &[]);
    let start = Instant::now();
    for _ in 0..n {
        issue(kernel, Verb::Source, iri, &[]);
    }
    start.elapsed() / n
}

/// ★ Sharing the store handle must not de-cache the ledger — measured, because the types are
/// identical either way and 68 passing tests is exactly what the cms-web regression looked
/// like.
///
/// Three kernels over the same 247-item corpus: the owned store this server composed before,
/// the shared store with no recovery (what calling `open_shared` and accepting the blanket
/// would have shipped), and the composition `compose_with` builds.
#[test]
fn the_shared_handle_costs_the_ledger_nothing() {
    const ITEMS: usize = 247;
    let dir = scratch_root();
    let (served, _watch) = served(&dir);
    let naive = naive(&dir);
    let owned = owned();
    for kernel in [&owned, &naive, &served] {
        seed_items(kernel, ITEMS);
    }

    println!("--- {ITEMS} items, mean of 20 reads ---");
    for (label, kernel) in [
        ("owned     (open — no browse face)", &owned),
        ("shared    (open_shared, no recovery)", &naive),
        ("recovered (open_shared + freshness)", &served),
    ] {
        let items = time_reads(kernel, "urn:iki:ledger:items", 20);
        let next = time_reads(kernel, "urn:iki:ledger:next", 20);
        println!("  {label:38}  items {items:>10.2?}  next {next:>10.2?}");
    }
    // The browse half: the same reads on the root this server watches (cached) and on the
    // composition that declares them live (uncached), which is `ikigai-browse`'s own default.
    println!("--- the watched root's filesystem reads ---");
    for (label, kernel) in [("live (unwatched)", &naive), ("watched", &served)] {
        let tree = time_reads(kernel, "urn:repo:demo:tree", 20);
        let file = time_reads(kernel, "urn:repo:demo:file:src/lib.rs", 20);
        println!("  {label:38}  tree  {tree:>10.2?}  file {file:>10.2?}");
    }

    // The assertion is not the timing — it is WHETHER the read is in the cache at all, which
    // is what the timing follows from and what cannot flake on a loaded machine.
    let hot = cached(&served);
    assert!(
        hot.iter().any(|iri| iri == "urn:iki:ledger:items"),
        "the recovered composition must cache a ledger read: {hot:?}"
    );
    assert!(
        hot.iter().any(|iri| iri == "urn:iki:store:graph-select"),
        "…and the scoped store read under it: {hot:?}"
    );
    let cold = cached(&naive);
    assert!(
        !cold.iter().any(|iri| iri == "urn:iki:ledger:items"),
        "the naive composition caches nothing store-derived — that is the cost this arc \
         refused to pay: {cold:?}"
    );
    // And the broad face stays uncacheable in BOTH, by name: it can see the default graph,
    // where browse's invisible writes land.
    for (label, kernel) in [("recovered", &served), ("naive", &naive)] {
        issue(
            kernel,
            Verb::Source,
            "urn:iki:store:select",
            &[("query", "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1")],
        );
        assert!(
            !cached(kernel)
                .iter()
                .any(|iri| iri == "urn:iki:store:select"),
            "{label}: the broad read door must not be cached over a shared store"
        );
    }
}

// ------------------------------------------------- what each door can reach

/// ★ The capability argument, re-made for the families this arc links, as a test rather than
/// as a README paragraph.
///
/// An anonymous loopback caller on the HTTP door holds exactly the configured ledgers' narrow
/// read+write tokens (`doors::HttpDoor::anonymous`). Browse and the facades are now COMPILED
/// IN and reachable — but reachable is a function of the grant, and that grant carries
/// neither `urn:cap:browse:read:*` nor `urn:cap:exec:*`. Both halves are checked, because
/// they fail differently: the manifold must not OFFER what the caller cannot invoke
/// (over-offer), and the invocation must be DENIED rather than served.
#[test]
fn the_anonymous_http_caller_is_offered_and_allowed_nothing_from_the_new_families() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let anonymous = Capability::scoped(
        grants_for_all(&["default".to_string()], Authority::Write).expect("the default ledger"),
    );

    let manifold = String::from_utf8_lossy(
        &block_on(Kernel::issue(
            &hub,
            request(Verb::Source, "urn:kernel:actions", &[]),
            &anonymous,
        ))
        .expect("the manifold answers any capability about itself")
        .bytes,
    )
    .into_owned();
    // The manifold names the templated rows (`{ledger}` is an argument of the action, not a
    // separate offer), and the store's SCOPED faces — which is exactly the pair of grants
    // `grants_for` hands out.
    for offered in [
        "urn:iki:ledger:{ledger}:items",
        "urn:iki:store:graph-select",
    ] {
        assert!(
            manifold.contains(offered),
            "`{offered}` is what this caller was granted: {manifold}"
        );
    }
    for absent in [
        "urn:repo:",
        "urn:iki:annotation",
        "urn:system:exec",
        "urn:iki:store:select",
    ] {
        assert!(
            !manifold.contains(absent),
            "`{absent}` must not be offered to an anonymous loopback caller:\n{manifold}"
        );
    }

    for denied in [
        "urn:repo:demo:tree",
        "urn:repo:demo:file:src/lib.rs",
        "urn:repo:status",
        "urn:system:exec",
    ] {
        let answer = block_on(Kernel::issue(
            &hub,
            request(Verb::Source, denied, &[]),
            &anonymous,
        ));
        assert!(
            matches!(answer, Err(ikigai_core::Error::Denied(_))),
            "`{denied}` must be a typed Denied for an anonymous caller, got {answer:?}"
        );
    }
}

// ----------------------------------------------------------- what it watches

/// ★ A cached filesystem read must recompute when the file changes on disk — driven through
/// the watcher's OWN notification path rather than a sleep, so a slow platform fails loudly
/// instead of passing by luck.
///
/// The stale read in the middle is the premise, not a wish: it proves the recompute at the
/// end is the cut's doing and not an uncached endpoint's. This is ledger #246 in miniature,
/// with the halves the other way round — here the thread is declared only because something
/// cuts it.
#[test]
fn a_watched_file_read_recomputes_after_the_file_changes_on_disk() {
    let dir = scratch_root();
    let (hub, watch) = served(&dir);
    let file = "urn:repo:demo:file:src/lib.rs";
    assert_eq!(text(&hub, Verb::Source, file, &[]), "the first version\n");

    // std::fs on purpose: this IS the out-of-band edit the kernel cannot see.
    std::fs::write(dir.path().join("src/lib.rs"), "the second version\n").expect("the edit");
    assert_eq!(
        text(&hub, Verb::Source, file, &[]),
        "the first version\n",
        "the cached read must hold until its thread is cut"
    );

    let thread = "urn:iki:gonk:browse:root:demo".to_string();
    let mut cut = false;
    for _ in 0..64 {
        match watch.apply_next(&hub, Duration::from_secs(10)) {
            Some(threads) if threads.contains(&thread) => {
                cut = true;
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(
        cut,
        "the watcher never reported {thread} within its deadline"
    );
    assert_eq!(
        text(&hub, Verb::Source, file, &[]),
        "the second version\n",
        "a cut thread must make the next read recompute"
    );
}

/// The fail-closed half: a root nobody is watching is served live and uncached, so there is
/// no composition in which a stale file read can be served at all.
#[test]
fn an_unwatched_roots_reads_are_not_cached() {
    let dir = scratch_root();
    let (store, handle) = DurableStore::in_memory_shared().expect("a shared in-memory store");
    // Wired with an EMPTY watched set — what `main` builds for a root whose platform watcher
    // refused to start.
    let wired = browse::wire(roots(&dir), handle, &[]);
    let hub = Arc::new(compose_with(store, Some(Arc::new(wired.space))));
    let file = "urn:repo:demo:file:src/lib.rs";
    assert_eq!(text(&hub, Verb::Source, file, &[]), "the first version\n");
    assert!(
        !cached(&hub).iter().any(|iri| iri == file),
        "an unwatched root must not be cached"
    );
    std::fs::write(dir.path().join("src/lib.rs"), "the second version\n").expect("the edit");
    assert_eq!(
        text(&hub, Verb::Source, file, &[]),
        "the second version\n",
        "…and its reads are therefore never stale"
    );
}
