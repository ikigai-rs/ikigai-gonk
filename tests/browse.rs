//! The browse family over gonk's ONE dataset: what it serves, where its quads land, what
//! the shared handle costs, and what the watcher buys.
//!
//! Five things are gonk's to get wrong here, and each is a test:
//!
//! - [`the_served_catalog_is_exactly_what_this_manifest_links`] — linking two more crates put
//!   twenty more resources behind the doors; the catalog is pinned id by id, as it was.
//! - [`a_browse_write_touches_no_reserved_graph`] — the PROMISE `main` makes to
//!   `ikigai-store`, checked at the boundary with the store's own tripwire rather than
//!   trusted in a comment.
//! - [`a_browse_read_touches_no_reserved_graph`] — the same tripwire around a READ, which is
//!   the reason the manifest takes `ikigai-browse` 0.4.0: before it, a read could relocate
//!   another writer's quads (ledger #265).
//! - [`a_named_graph_choice_moves_the_promise_with_it`] — the graph this server CHOSE, walked
//!   end to end: browse's quads land in it, none in the default graph, and the promise moved
//!   with the choice rather than beside it (ledger #282).
//! - [`the_browse_graphs_scoped_reads_are_not_cached_and_the_ledgers_still_are`] — obligation
//!   3 of the graph decision, asked of the kernel rather than of the promise.
//! - [`a_scoped_token_reads_browse_and_still_cannot_join`] — obligation 2: what
//!   `urn:cap:store:read:graph:<browse graph>` buys, and the boundary it does not cross.
//! - [`a_ledger_item_joins_an_annotation_on_a_repo_file`] — the point of one dataset, with
//!   both halves naming their graph (obligation 4).
//! - [`the_shared_handle_costs_the_ledger_nothing`] — the naive composition beside the one
//!   this server builds, and the default-graph composition beside the named one, with the
//!   read timings printed.
//! - [`a_watched_file_read_recomputes_after_the_file_changes_on_disk`] — the cache that
//!   watcher makes safe, driven through the watcher's own notification path.
//!
//! Every store here is an `in_memory_*` one, which carries the SAME coverage as its durable
//! twin — `in_memory_shared_declaring` for the composition this server builds, plain
//! `in_memory_shared` for the naive one it refused — and takes no RocksDB lock, so these run
//! beside a live gonk holding `~/.ikigai/store`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Fallback, Iri, Kernel, Representation, Request, Space, SystemClock, Verb,
};
use ikigai_gonk::config::ExplainTiers;
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::mount::{self, Mount};
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use oxigraph::model::{Literal, NamedNode, Term};
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

/// The composition `main` builds when `gonk.browse.root` names a root: a shared store that
/// DECLARES where its sharer writes, and the browse family over the same dataset.
fn served(dir: &TempDir) -> (Arc<Kernel>, RootWatch) {
    let served = served_with(dir, None);
    (served.hub, served.watch)
}

/// ★ The same, plus a `gonk.mount` — pointed at a socket that does not exist.
///
/// That is not a shortcut around a fixture: **binding the explanation families is a function
/// of the CONFIG, never of whether the peer answers**, which is the whole of `prefer` on a
/// server whose other job must not depend on a model. A test that needed a live peer could
/// not run in CI, and would be testing the peer rather than this composition.
fn served_explaining(dir: &TempDir) -> (Arc<Kernel>, RootWatch) {
    let served = served_with(dir, Some(dead_mount()));
    (served.hub, served.watch)
}

/// A mount whose peer can never answer: a Unix socket path that is not there, so the dial
/// fails immediately rather than waiting out a network timeout.
fn dead_mount() -> Mount {
    mount::parse(
        "prefer urn:llm:=/nonexistent/ikigai-gonk-tests/llm.sock",
        std::path::Path::new("/home/nobody"),
    )
    .expect("a mount line")
}

/// ⚠ The third return is the SAME store, cloned before it was composed — a `DurableStore` is
/// a handle, so the clone reads the one dataset the kernel serves. It exists for the two
/// tripwire tests, which need `reserved_graphs_fingerprint` after `compose_with` has taken
/// the store by value; every other test drops it.
///
/// The fourth is the sharer's own handle, cloned before it was wired: planting a decoy in
/// another writer's graph is not something any bound endpoint of this server does, and
/// [`a_browse_read_touches_no_reserved_graph`] has to be able to do it.
fn served_with(dir: &TempDir, mount: Option<Mount>) -> Served {
    served_in(dir, mount, browse::Graph::chosen())
}

/// The whole composition, parameterized by the ONE thing that decides where browse's quads
/// live and what the store is promised about them.
///
/// ★ `graph` is used **twice and constructed once**, exactly as `main` does it: once as
/// `sharer_writes()` on the store open, once as browse's mount. Passing it in rather than
/// calling `Graph::chosen()` twice here is the point of the test fixture as much as of the
/// server — a fixture that made the two statements separately could not fail the way the
/// server would.
fn served_in(dir: &TempDir, mount: Option<Mount>, graph: browse::Graph) -> Served {
    // ★ `_declaring`, matching `main`: the promise that the sharer (`ikigai-browse`) writes
    // where `graph` says and nowhere else is what keeps every scoped read — which is every
    // read the ledger makes — cacheable under the store's own write threads. Plain
    // `in_memory_shared` here would test a composition this server does not build, and would
    // do it by being slower and passing.
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store that declares where its sharer writes");
    let sharer = Arc::clone(&handle);
    let (watch, refused) = RootWatch::start(&roots(dir));
    assert!(refused.is_empty(), "{refused:?}");
    let tiers = ExplainTiers::default();
    let wired = browse::wire(
        roots(dir),
        handle,
        watch.watched(),
        mount.is_some().then_some(&tiers),
        &graph,
    );
    let mounted = mount.iter().map(mount::space).collect();
    let hub = Arc::new(compose_with(
        store.clone(),
        Some(Arc::new(wired.space)),
        mounted,
        None,
    ));
    Served {
        hub,
        watch,
        store,
        sharer,
    }
}

/// What [`served_in`] hands back.
struct Served {
    hub: Arc<Kernel>,
    watch: RootWatch,
    /// The kernel's own store, as a second handle — for `reserved_graphs_fingerprint`.
    store: DurableStore,
    /// The SHARER's handle, for planting quads no endpoint of this server would write.
    sharer: Arc<ikigai_store::Store>,
}

/// ★ The composition this arc REFUSED: `open_shared` with NO declaration — the handle leaves
/// and the store is told nothing about where its holder writes, so it must assume the worst
/// and make every read `Expiry::Always`. It exists so the cost of the easy path is a number
/// in the test output rather than an assertion about a counterfactual.
fn naive(dir: &TempDir) -> Arc<Kernel> {
    let (store, handle) = DurableStore::in_memory_shared().expect("a shared in-memory store");
    // No watcher either: a host that accepts the blanket has no reason to run one, and
    // `ikigai-browse`'s reads are live and uncacheable exactly as it declares them.
    let wired = browse::wire(roots(dir), handle, &[], None, &browse::Graph::chosen());
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

/// Every resource the browse composition puts behind the doors WITHOUT a mount. `explain`,
/// `review` and the PR-derived layers are deliberately absent: they derive through
/// `urn:llm:*`, which nothing binds until a `gonk.mount` line names the peer that serves it,
/// and an action the kernel can never satisfy is an over-offer.
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

/// The five the mount adds, over seven patterns per root (`explain`, `explain:{path}`,
/// `explain-versions`, `explain-versions:{path}`, `review:{path}`, `pr:{n}:explain`,
/// `pr:{n}:review`). Each derives through `urn:llm:{provider}:ask`; each therefore declares
/// `urn:cap:net:*` on top of the browse root grant, and `browse-review` and
/// `browse-pr-review` declare `urn:cap:annotate` as well, because their findings are minted
/// as annotations in this dataset.
const EXPLAIN_IDS: [&str; 5] = [
    "browse-explain",
    "browse-explain-versions",
    "browse-pr-explain",
    "browse-pr-review",
    "browse-review",
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

/// ★ **A mount line is what binds the explanation families, and nothing else is.** The same
/// composition with a `gonk.mount` gains exactly five description ids and loses none — so a
/// gonk with no peer configured cannot offer an action no kernel in this process could
/// satisfy, and a gonk with one offers every row that peer makes satisfiable.
///
/// The peer here is unreachable on purpose (see [`served_explaining`]): binding is a
/// property of the config, reachability of the moment.
#[test]
fn a_mount_binds_the_explanation_families_and_nothing_else_does() {
    let dir = scratch_root();
    let mut everything: Vec<String> = BROWSE_IDS
        .iter()
        .chain(REPO_IDS.iter())
        .chain(EXPLAIN_IDS.iter())
        .map(|id| id.to_string())
        .collect();
    everything.sort();

    let (unmounted, _watch) = served(&dir);
    let found = new_family_ids(&unmounted, &everything);
    for id in EXPLAIN_IDS {
        assert!(
            !found.contains(&id.to_string()),
            "`{id}` is an over-offer without a mount: {found:?}"
        );
    }

    let (mounted, _watch) = served_explaining(&dir);
    assert_eq!(
        new_family_ids(&mounted, &everything),
        everything,
        "a mounted gonk serves the browse composition plus the five derived families"
    );
    let door = ikigai_gonk::doors::door_kernel(Arc::clone(&mounted));
    assert_eq!(new_family_ids(&door, &everything), everything);
}

// ------------------------------------------------------------- the spend gate

/// ★★ **The spend gate, per capability — the centre of this arc.**
///
/// Deriving an explanation calls a model, and this server's pages are meant to be browsed.
/// So the question is not whether explain works but who can make it spend, and the answer is
/// a capability rather than a door check:
///
/// - **Anonymous HTTP** holds per-ledger tokens only. It reaches no browse row at all, so it
///   can neither derive an explanation nor read an archived one, and the manifold does not
///   offer it either.
/// - **A browse grant with no net grant** — the shape of a `grants.json` entry for a client
///   that may read a repository — reads every browse row and `explain-versions` (which
///   derives nothing and declares no network), and is DENIED on `explain` itself.
/// - **Root** (the socket door) may derive.
///
/// The second case is the interesting one, and the last assertion is the uncomfortable
/// truth in it: `version=` addresses an archived entry and provably derives nothing, but it
/// is the same action and carries the same requirement, so "may read what was already paid
/// for" is not grantable today. That is browse's grain, not gonk's, and it is what the HTTP
/// browse face will have to answer (#258).
#[test]
fn deriving_needs_a_net_grant_and_listing_the_archive_does_not() {
    let dir = scratch_root();
    let (hub, _watch) = served_explaining(&dir);
    let anonymous = Capability::scoped(
        grants_for_all(&["default".to_string()], Authority::Write).expect("the ledger tokens"),
    );
    let reader = Capability::scoped(["urn:cap:browse:read:demo"]);
    let deriver = Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"]);

    let explain = "urn:repo:demo:explain:src/lib.rs";
    let versions = "urn:repo:demo:explain-versions:src/lib.rs";

    for (who, capability) in [("anonymous", &anonymous), ("a browse reader", &reader)] {
        let answer = block_on(Kernel::issue(
            &hub,
            request(Verb::Source, explain, &[]),
            capability,
        ));
        assert!(
            matches!(answer, Err(ikigai_core::Error::Denied(_))),
            "{who} must not be able to make this server spend a token: {answer:?}"
        );
        // …and the archived entry is behind the same wall, for the same reason.
        let archived = block_on(Kernel::issue(
            &hub,
            request(
                Verb::Source,
                explain,
                &[("version", "code-v1@qwen3-coder:30b")],
            ),
            capability,
        ));
        assert!(
            matches!(archived, Err(ikigai_core::Error::Denied(_))),
            "{who} on an archived entry: {archived:?}"
        );
    }

    // Anonymous cannot even ask what the archive holds — it holds no browse grant.
    assert!(matches!(
        block_on(Kernel::issue(
            &hub,
            request(Verb::Source, versions, &[]),
            &anonymous
        )),
        Err(ikigai_core::Error::Denied(_))
    ));
    // A browse reader can: listing the archive derives nothing and declares no network.
    let listed = block_on(Kernel::issue(
        &hub,
        request(Verb::Source, versions, &[]),
        &reader,
    ))
    .expect("explain-versions requires no net grant");
    assert!(listed.bytes.is_empty(), "nothing is archived yet");

    // The manifold tells the same story it enforces: no explain row for a caller who could
    // not invoke one, and the free row for the one who can.
    let offered = |capability: &Capability| -> String {
        String::from_utf8_lossy(
            &block_on(Kernel::issue(
                &hub,
                request(Verb::Source, "urn:kernel:actions", &[]),
                capability,
            ))
            .expect("the manifold answers any capability about itself")
            .bytes,
        )
        .into_owned()
    };
    let to_anonymous = offered(&anonymous);
    assert!(!to_anonymous.contains("explain"), "{to_anonymous}");
    let to_reader = offered(&reader);
    assert!(
        to_reader.contains("urn:repo:demo:explain-versions"),
        "the free row is offered: {to_reader}"
    );
    assert!(
        !to_reader.contains("urn:repo:demo:explain:"),
        "the spending row is not: {to_reader}"
    );
    assert!(
        offered(&deriver).contains("urn:repo:demo:explain:"),
        "a net grant is what puts the spending row on offer"
    );
}

/// ★ **Explain is an addition, never a dependency.** With the peer unreachable, a caller who
/// MAY derive gets a TRANSIENT failure — not a denial, not an unresolved name — and every
/// other resource this server exists for answers exactly as before.
#[test]
fn a_peer_that_is_down_costs_explain_and_nothing_else() {
    let dir = scratch_root();
    let (hub, _watch) = served_explaining(&dir);
    let filed = text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "the ledger does not depend on a model")],
    );
    assert!(filed.contains("urn:iki:ledger:default:item:"), "{filed}");
    assert!(text(&hub, Verb::Source, "urn:iki:ledger:items", &[]).contains("model"));
    assert_eq!(
        text(&hub, Verb::Source, "urn:repo:demo:file:src/lib.rs", &[]),
        "the first version\n",
        "a browse read is untouched by an absent peer"
    );

    let derived = block_on(Kernel::issue(
        &hub,
        request(Verb::Source, "urn:repo:demo:explain:src/lib.rs", &[]),
        &Capability::root(),
    ));
    let error = derived.expect_err("there is no peer to derive against");
    assert!(
        error.is_transient(),
        "an absent peer is transient — a Retry or a Failover above this mount can act on it, \
         and a caller can tell it from `the model refused`: {error:?}"
    );
    // The mount answered; the name is not unbound. (`Unresolved` would say the opposite, and
    // would be what a caller saw if the mount space were composed behind the local ones.)
    assert!(
        !matches!(error, ikigai_core::Error::Unresolved(_)),
        "{error:?}"
    );
    // And the model face itself, asked directly, says the same thing.
    let asked = block_on(Kernel::issue(
        &hub,
        request(Verb::Source, "urn:llm:ask", &[("in", "hello")]),
        &Capability::root(),
    ));
    assert!(
        asked
            .as_ref()
            .err()
            .is_some_and(ikigai_core::Error::is_transient),
        "{asked:?}"
    );
}

// ------------------------------------------------- where browse's quads land

/// ★ The PROMISE `main` makes when it hands the handle out — the [`browse::Graph`] choice
/// read as `SharerWrites` — pinned at the boundary with `ikigai-store`'s own tripwire.
/// Every quad `ikigai-browse` writes goes into the graph this server chose (and, before
/// 2026-09-16, into the default one), so no graph this store RESERVES from the sharer can
/// move when browse writes; and a graph that cannot move is a graph whose scoped reads stay
/// cacheable, which is every read the ledger makes.
///
/// ★ Since the graph decision the promise names one graph, so the browse graph is not among
/// the reserved ones and the ledger's still is — the assertion on `before.len()` below is
/// that sentence, and it would have been 2 with the promise left at the bare default.
///
/// ⚠ **The fingerprint is over QUADS, not over the set of graph names**, and that is the
/// difference that matters: this server wrote the name-set version by hand until
/// `ikigai-store` 0.2.4, and it would have missed a sharer writing into a named graph that
/// already existed — which is exactly the case a running gonk is in, where the ledger's graph
/// is always there. It also *errors* on a store that declared nothing, rather than passing
/// vacuously.
///
/// ⚠ Both fingerprints are taken around the SHARER's write and nothing else. A write through
/// `ikigai-store`'s own endpoints changes a reserved graph legitimately — it cut a thread when
/// it did — and would read here as a broken promise.
///
/// The day browse writes a graph the promise does not name, this test is red HERE, where the
/// promise is made, instead of silently stale in the cache of a running server.
#[test]
fn a_browse_write_touches_no_reserved_graph() {
    let dir = scratch_root();
    let Served {
        hub, store, watch, ..
    } = served_with(&dir, None);
    let _watch = watch;
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so a reserved graph exists at all")],
    );
    let before = store
        .reserved_graphs_fingerprint()
        .expect("this store declares where its sharer writes");
    assert_eq!(
        before.len(),
        1,
        "the ledger's graph is reserved from the sharer, and it is the only named one — a \
         fingerprint over nothing would pass whatever browse did"
    );

    annotate(
        &hub,
        "n1",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "a note",
    );
    // The annotation IS in the dataset — the broad read sees it, so the tripwire below is
    // measuring a write that really happened…
    //
    // ⚠ `GRAPH ?g` rather than a bare pattern, and that is obligation 4 in one line: browse
    // writes a NAMED graph now, so a bare `{ ?a ik:annotates ?t }` reads the default graph and
    // finds nothing. It would not have failed — it would have passed the tripwire below
    // vacuously, measuring a write it could not see.
    let answer = text(
        &hub,
        Verb::Source,
        "urn:iki:store:select",
        &[(
            "query",
            "SELECT ?a WHERE { GRAPH ?g { ?a <https://ikigai-rs.dev/ns#annotates> ?t } }",
        )],
    );
    assert!(answer.contains("urn:iki:annotation:n1"), "{answer}");
    // …and it moved no reserved graph, which is why a scoped read of one stays cacheable.
    let changed = store
        .reserved_graphs_fingerprint()
        .expect("still declaring")
        .changed_since(&before);
    assert!(
        changed.is_empty(),
        "a browse write moved {changed:?}, which `main` promised the sharer would not touch — \
         every scoped read of those graphs is now cached against threads that write never cut"
    );
}

/// The graph an annotation-shaped decoy is planted in: a name no part of this server writes,
/// standing in for another writer's tenancy.
const ANOTHER_WRITER: &str = "urn:iki:graph:another-writer";

/// Plant a quad browse would recognize as one of its own — in someone else's graph, through
/// the sharer's handle, because no bound endpoint of this server would ever write there.
fn plant_decoy(sharer: &ikigai_store::Store, graph: &str, id: &str) {
    let ik = |t: &str| NamedNode::new(format!("https://ikigai-rs.dev/ns#{t}")).expect("an IRI");
    let oa = |t: &str| NamedNode::new(format!("http://www.w3.org/ns/oa#{t}")).expect("an IRI");
    let graph = NamedNode::new(graph).expect("a graph IRI");
    let decoy = NamedNode::new(id).expect("an annotation IRI");
    for (p, o) in [
        (
            NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#type").expect("rdf:type"),
            Term::NamedNode(oa("Annotation")),
        ),
        (
            oa("bodyValue"),
            Term::Literal(Literal::new_simple_literal("not this server's note")),
        ),
        (
            ik("repo"),
            Term::Literal(Literal::new_simple_literal("demo")),
        ),
        (
            ik("path"),
            Term::Literal(Literal::new_simple_literal("src/lib.rs")),
        ),
        (
            ik("annotates"),
            Term::NamedNode(NamedNode::new("urn:repo:demo:file:src/lib.rs").expect("a file IRI")),
        ),
        // ⚠ A quote that is NOT in the file, so browse's drift re-anchoring has something to
        // do. That is the half that makes a read able to WRITE: `annotate::refresh` rewrites
        // an annotation whose anchor has moved, during a Source.
        (
            oa("exact"),
            Term::Literal(Literal::new_simple_literal("a line this file never had")),
        ),
    ] {
        sharer
            .insert(oxigraph::model::Quad::new(decoy.clone(), p, o, graph.clone()).as_ref())
            .expect("the decoy plants");
    }
}

/// ★ **The other end of the same promise: a browse READ moves no reserved graph either.**
///
/// This is why the manifest takes `ikigai-browse` 0.4.0 rather than a version number's worth
/// of tidiness. Through 0.3.2 browse's annotation reads passed no graph at all — which in
/// `quads_for_pattern` means EVERY graph — and `annotate::refresh` rewrites a drifted
/// annotation *during a Source*. Together those made a plain read destructive across a graph
/// boundary: it re-anchored an annotation-shaped quad sitting in another writer's named graph
/// and PERSISTED the move (measured against 0.3.2 in ledger #265).
///
/// gonk is exactly the host with something to lose there — one dataset, browse beside a
/// graph-scoped ledger — and the loss would be silent twice over: the other writer's quads
/// relocate, AND this server's coverage promise becomes false, so every scoped read of the
/// robbed graph goes on being served from cache against threads that write never cut.
///
/// ⚠ The mechanism is deliberately the SAME one [`a_browse_write_touches_no_reserved_graph`]
/// uses — `reserved_graphs_fingerprint`, over quads — rather than a new assertion shape. The
/// promise is about what the sharer writes; a read that writes is still a write, and it is
/// the fingerprint's business whichever verb caused it. What is new here is only the verb
/// between the two fingerprints.
///
/// `ikigai-browse`'s own `a_default_mount_does_not_read_another_graph` covers the VISIBILITY
/// half (asserted below too, cheaply). Nothing stated the write-back half until this test —
/// ledger #265 asked for it in so many words.
#[test]
fn a_browse_read_touches_no_reserved_graph() {
    let dir = scratch_root();
    let Served {
        hub,
        store,
        sharer,
        watch,
    } = served_with(&dir, None);
    let _watch = watch;
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so a reserved graph exists at all")],
    );
    // One of this server's own annotations, so the read has real work to do and is not
    // answering out of an empty archive.
    annotate(
        &hub,
        "ours",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "this server's own note",
    );
    plant_decoy(&sharer, ANOTHER_WRITER, "urn:iki:annotation:theirs");

    let before = store
        .reserved_graphs_fingerprint()
        .expect("this store declares where its sharer writes");
    assert_eq!(
        before.len(),
        2,
        "the ledger's graph and the decoy's, both reserved from the sharer — a fingerprint \
         over nothing would pass whatever the read did"
    );

    // Every browse read that touches the annotation overlay, plain and through the faces that
    // fold it in. The blunt argument test in `browse::cached_reads` means the faces are not
    // cached, so each of these really reaches the store.
    for (iri, args) in [
        ("urn:repo:demo:annotations", &[][..]),
        ("urn:repo:demo:annotations:src/lib.rs", &[]),
        (
            "urn:repo:demo:file:src/lib.rs",
            &[("annotations", "include")][..],
        ),
        ("urn:repo:demo:file:src/lib.rs", &[("as", "text/html")][..]),
    ] {
        let listed = text(&hub, Verb::Source, iri, args);
        assert!(
            !listed.contains("urn:iki:annotation:theirs"),
            "a read of {iri} reached into <{ANOTHER_WRITER}>: {listed}"
        );
    }

    // ★ And the half nothing stated before: it did not MOVE them either.
    let changed = store
        .reserved_graphs_fingerprint()
        .expect("still declaring")
        .changed_since(&before);
    assert!(
        changed.is_empty(),
        "a browse READ moved {changed:?}. On `ikigai-browse` 0.3.2 that is <{ANOTHER_WRITER}>, \
         whose decoy the read re-anchored into the default graph and persisted — another \
         writer's quads relocated by a Source, and this server's `SharerWrites` promise false \
         from that moment on"
    );
    // The decoy is still whole, said plainly: the fingerprint above is a digest, and a reader
    // should not have to trust that a digest of five quads means five quads.
    let still = text(
        &hub,
        Verb::Source,
        "urn:iki:store:select",
        &[(
            "query",
            &format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{ANOTHER_WRITER}> \
                 {{ <urn:iki:annotation:theirs> ?p ?o }} }}"
            ),
        )],
    );
    assert!(still.contains("\"value\":\"6\""), "{still}");
}

/// ★ **The derivation on its other arm: choose a named graph and the promise goes with it.**
///
/// [`browse::Graph`] is one value read two ways — `sharer_writes()` for `ikigai-store` and
/// `Mount::graph` for `ikigai-browse` — and the whole claim of ledger #282's fix is that
/// those cannot disagree. `src/browse.rs::graph_choice_and_promise_cannot_disagree` checks
/// that the two READINGS match; this checks that they match *the store*, with a real write
/// through a real kernel.
///
/// ★ Since 2026-09-16 this is the arm the server SHIPS, and the test takes the shipped
/// choice rather than an IRI of its own: it is now the end-to-end statement that a real write
/// through a real kernel lands in the graph an operator was told to migrate into, and in no
/// other. (It was written a version earlier, against a made-up IRI, so that the day the
/// decision was taken the promise would already be known to move with it rather than be
/// discovered to. It was.)
///
/// It also prices obligation 3 on `Graph`'s docs, from the store's own mouth rather than by
/// argument: a promised graph is NOT covered, so its scoped reads stop being cacheable. That
/// is the ~1000× this server measured, and it is the cost of putting browse inside the
/// tenancy boundary.
#[test]
fn a_named_graph_choice_moves_the_promise_with_it() {
    const LEDGER_GRAPH: &str = "urn:iki:ledger:graph:default";

    let dir = scratch_root();
    let graph = browse::Graph::chosen();
    let browse_graph: String = graph
        .named()
        .expect("this server chooses a named browse graph")
        .as_str()
        .to_string();
    let Served {
        hub,
        store,
        sharer,
        watch,
    } = served_in(&dir, None, graph);
    let _watch = watch;
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so the ledger's graph exists")],
    );
    plant_decoy(&sharer, ANOTHER_WRITER, "urn:iki:annotation:theirs");
    let before = store
        .reserved_graphs_fingerprint()
        .expect("this store declares where its sharer writes");
    assert_eq!(
        before.len(),
        2,
        "the ledger's graph and the decoy's are reserved — the BROWSE graph is not, because \
         the promise derived from this choice names it"
    );

    annotate(
        &hub,
        "n1",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "a note in its own graph",
    );

    // Where it landed: the named graph, and not the default one.
    let count = |query: &str| {
        let answer = text(
            &hub,
            Verb::Source,
            "urn:iki:store:select",
            &[("query", query)],
        );
        let json: serde_json::Value = serde_json::from_str(&answer).expect("SPARQL results JSON");
        json["results"]["bindings"][0]["n"]["value"]
            .as_str()
            .unwrap_or_default()
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("a count: {answer}"))
    };
    let annotates = "<https://ikigai-rs.dev/ns#annotates>";
    assert_eq!(
        count(&format!(
            "SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{browse_graph}> {{ ?a {annotates} ?t }} }}"
        )),
        1,
        "the mount took the choice: browse's quads are in <{browse_graph}>"
    );
    assert_eq!(
        count(&format!(
            "SELECT (COUNT(*) AS ?n) WHERE {{ ?a {annotates} ?t }}"
        )),
        0,
        "…and none stayed in the default graph, which is the whole of what a migration has \
         to move for a store that already has some"
    );

    // The promise took the choice too: the browse graph moved, and it was allowed to.
    let changed = store
        .reserved_graphs_fingerprint()
        .expect("still declaring")
        .changed_since(&before);
    assert!(
        changed.is_empty(),
        "the sharer wrote {changed:?}, outside the graph this choice promised it"
    );

    // ★ And what that costs, from `ikigai-store` rather than from a comment: a graph the
    // sharer may write is not covered, so a scoped read of it can no longer be cached.
    assert!(
        !store.read_is_covered(Some(&browse_graph)),
        "obligation 3 on `browse::Graph`: naming a graph forfeits its cacheability, and the \
         host owes a fresh freshness argument for it"
    );
    assert!(
        store.read_is_covered(Some(LEDGER_GRAPH)),
        "…and the LEDGER's exemption is untouched, which is why this is a cost and not a cliff"
    );
    assert!(
        store.read_is_covered(Some(ANOTHER_WRITER)),
        "as is every other graph in the dataset"
    );
}

/// The point of one dataset: a ledger item that is ABOUT a repo file joins an annotation on
/// that same file, in one query, with no federation.
///
/// ★ **Obligation 4, in the one query this repo owns.** Both halves name their graph now —
/// before the graph decision the browse half was a bare pattern, because browse's quads were
/// in the default graph. The failure mode of forgetting is an empty result set, never an
/// error, which is why the README's migration steps say it out loud for an operator's own
/// saved queries.
///
/// ⚠ Still under a ROOT capability, and that is NOT for want of the graph decision — it is
/// the store's shape. `urn:iki:store:graph-select` takes ONE graph
/// (`ikigai-store`'s `confine` sets the query's whole dataset to it), so a two-graph join
/// cannot be expressed through the narrow door however many per-graph tokens the caller
/// holds. What the decision bought is that each HALF is now grantable; what it did not buy is
/// the join. See `a_scoped_token_reads_browse_and_still_cannot_join`.
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

    let chosen = browse::Graph::chosen();
    // ⚠ `.as_str()`: `NamedNode`'s Display brackets the IRI itself, so `<{node}>` writes
    // `<<urn:…>>` and the query fails to parse.
    let browse_graph = chosen.named().expect("a named browse graph").as_str();
    let answer = text(
        &hub,
        Verb::Source,
        "urn:iki:store:select",
        &[(
            "query",
            &format!(
                "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
             PREFIX ik: <https://ikigai-rs.dev/ns#>
             PREFIX oa: <http://www.w3.org/ns/oa#>
             SELECT ?item ?file ?note WHERE {{
               GRAPH <urn:iki:ledger:graph:default> {{ ?item ledger:about ?file }}
               GRAPH <{browse_graph}> {{ ?annotation ik:annotates ?file ; oa:bodyValue ?note }}
             }}"
            ),
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

/// ★ **Obligation 3, from the running kernel rather than from the store's promise.**
///
/// `a_named_graph_choice_moves_the_promise_with_it` asks `ikigai-store` whether the browse
/// graph is covered. This asks the KERNEL what it actually did with two reads — the one thing
/// an operator would notice — and the pair is the whole of the freshness redo:
///
/// - a scoped read of the BROWSE graph is `Expiry::Always` and leaves nothing in the cache,
///   because the sharer (`ikigai-browse`) may write it behind the store's back;
/// - a scoped read of the LEDGER graph is cacheable and is cached, exactly as before the
///   graph decision — which is the ~1000× this server refuses to give up.
///
/// ⚠ Both reads are the same IRI with different arguments, so `urn:kernel:cache`'s rows
/// cannot tell them apart: the count of rows for that IRI is what moves, and it moves once.
#[test]
fn the_browse_graphs_scoped_reads_are_not_cached_and_the_ledgers_still_are() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let chosen = browse::Graph::chosen();
    let browse_graph = chosen.named().expect("a named browse graph").as_str();
    let ledger_graph = "urn:iki:ledger:graph:default";

    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so the ledger's graph has quads")],
    );
    annotate(
        &hub,
        "n1",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "a note",
    );
    let any = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1";
    let rows = |kernel: &Kernel| {
        cached(kernel)
            .iter()
            .filter(|iri| *iri == "urn:iki:store:graph-select")
            .count()
    };
    // The writes above already read the ledger's graph through the scoped door, so the
    // baseline is whatever they left behind: what this test measures is which of the two
    // reads below ADDS a row.
    let baseline = rows(&hub);

    let ledger = issue(
        &hub,
        Verb::Source,
        "urn:iki:store:graph-select",
        &[("graph", ledger_graph), ("query", any)],
    );
    assert_ne!(
        ledger.expiry,
        ikigai_core::Expiry::Always,
        "the ledger's graph is outside the sharer's promise, so its scoped reads stay \
         cacheable — this is the exemption the graph decision must not have cost"
    );
    let after_the_ledger = rows(&hub);
    assert_eq!(
        after_the_ledger,
        baseline + 1,
        "…and it is in the cache: {:?}",
        cached(&hub)
    );

    let browse_read = issue(
        &hub,
        Verb::Source,
        "urn:iki:store:graph-select",
        &[("graph", browse_graph), ("query", any)],
    );
    assert_eq!(
        browse_read.expiry,
        ikigai_core::Expiry::Always,
        "obligation 3: a graph the sharer may write cannot be cached, and `ikigai-store` \
         answers that per read from the promise `main` derived — there is no declaration in \
         this crate that could have got it wrong, and none that could have got it right"
    );
    assert_eq!(
        rows(&hub),
        after_the_ledger,
        "the browse read added no cache entry: {:?}",
        cached(&hub)
    );
}

/// ★ **Obligation 2, end to end: what a per-graph token over browse's data now buys — and
/// the boundary it does not cross.**
///
/// Before the graph decision this test could not have been written. Browse's quads were in
/// the default graph, which has no IRI, so no `urn:cap:store:read:graph:` token could name
/// them and every read of an annotation or an archived explanation needed root.
///
/// The four assertions are the grant's shape:
///
/// 1. the token READS browse's quads — the archive without the spend, which is what the
///    `urn:cap:net:*` on `urn:repo:{root}:explain` makes impossible through the browse row;
/// 2. it reaches no other graph, so it is not a ledger grant in disguise;
/// 3. it reaches no browse ENDPOINT — no file, no tree, no `gh`;
/// 4. ⚠ **and it cannot run the join.** Not because of anything gonk chose:
///    `urn:iki:store:graph-select` confines a query to ONE graph, so two graphs in one query
///    is not expressible through the narrow door at all, whatever tokens are held. The join
///    stays a root query, and that is a gap in `ikigai-store`'s scoped read face rather than
///    an unfinished obligation here (reported to the hub).
#[test]
fn a_scoped_token_reads_browse_and_still_cannot_join() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let chosen = browse::Graph::chosen();
    let browse_graph = chosen.named().expect("a named browse graph").as_str();
    let file = "urn:repo:demo:file:src/lib.rs";
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "lib.rs needs a doc comment"), ("about", file)],
    );
    annotate(&hub, "n1", file, "first", "this line is the one");

    // EXACTLY the tokens `ikigai-gonk grants --browse-graph read` prints, and nothing else.
    let reader = Capability::scoped(
        ikigai_gonk::grants::browse_graph_grants(Authority::Read).expect("a named browse graph"),
    );
    let scoped = |graph: &str, query: &str| {
        block_on(Kernel::issue(
            &hub,
            request(
                Verb::Source,
                "urn:iki:store:graph-select",
                &[("graph", graph), ("query", query)],
            ),
            &reader,
        ))
    };

    // 1. The archive, as data, to a caller holding one narrow token.
    let note = scoped(
        browse_graph,
        "PREFIX oa: <http://www.w3.org/ns/oa#>
         SELECT ?note WHERE { ?a oa:bodyValue ?note }",
    )
    .expect("the browse graph's own token reads the browse graph");
    let note = String::from_utf8_lossy(&note.bytes).into_owned();
    assert!(note.contains("this line is the one"), "{note}");

    // 2. One graph, not the dataset.
    assert!(
        matches!(
            scoped(
                "urn:iki:ledger:graph:default",
                "SELECT ?s WHERE { ?s ?p ?o }"
            ),
            Err(ikigai_core::Error::Denied(_))
        ),
        "a browse-graph token must not read the ledger's graph"
    );
    for broad in ["urn:iki:store:select", "urn:iki:store:info"] {
        assert!(
            matches!(
                block_on(Kernel::issue(
                    &hub,
                    request(
                        Verb::Source,
                        broad,
                        &[("query", "SELECT ?s WHERE { ?s ?p ?o }")]
                    ),
                    &reader,
                )),
                Err(ikigai_core::Error::Denied(_))
            ),
            "{broad} must stay root-only: it can see every graph, including the default one"
        );
    }

    // 3. The data, not the family: no file, no tree, no `gh`, no annotation endpoint.
    for denied in [
        file,
        "urn:repo:demo:tree",
        "urn:repo:demo:annotations",
        "urn:iki:annotation:n1",
        "urn:repo:status",
    ] {
        assert!(
            matches!(
                block_on(Kernel::issue(
                    &hub,
                    request(Verb::Source, denied, &[]),
                    &reader
                )),
                Err(ikigai_core::Error::Denied(_))
            ),
            "`{denied}` is the browse FAMILY, which this grant does not carry"
        );
    }

    // 4. ⚠ The join is still out of reach, and the reason is the door's shape. Naming the
    // ledger's graph inside a query scoped to the browse graph matches NOTHING — no error,
    // no rows — because `confine` sets the query's available named graphs to the one it was
    // issued for. A caller holding BOTH graphs' tokens gets the same empty answer.
    let joined = scoped(
        browse_graph,
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
         PREFIX ik: <https://ikigai-rs.dev/ns#>
         SELECT ?item WHERE {
           GRAPH <urn:iki:ledger:graph:default> { ?item ledger:about ?file }
           ?a ik:annotates ?file .
         }",
    )
    .expect("the query is legal; it simply cannot see the other graph");
    let joined = String::from_utf8_lossy(&joined.bytes).into_owned();
    let json: serde_json::Value = serde_json::from_str(&joined).expect("SPARQL results JSON");
    assert!(
        json["results"]["bindings"]
            .as_array()
            .expect("bindings")
            .is_empty(),
        "a scoped read is ONE graph: the ledger half is invisible, so the join returns \
         nothing rather than failing — {joined}"
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
/// Three kernels over the same 247-item corpus: the owned store this server composed before
/// it had a browse face, the shared store with no declaration (what calling `open_shared` and
/// accepting the blanket would have shipped), and the composition `main` builds — shared, and
/// declaring where the sharer writes.
#[test]
fn the_shared_handle_costs_the_ledger_nothing() {
    const ITEMS: usize = 247;
    let dir = scratch_root();
    let (served, _watch) = served(&dir);
    let naive = naive(&dir);
    let owned = owned();
    // ★ The CONTROL for the graph decision: the same composition with the choice this server
    // shipped until 2026-09-16. `served` above is the named graph — so the two rows printed
    // below are exactly "before" and "after" for the ledger's hot read, and the claim that
    // naming a graph cost the ledger nothing is a number rather than an argument.
    let before_the_decision = served_in(&dir, None, browse::Graph::TheDefault);
    let default_graph = before_the_decision.hub;
    let _watch = before_the_decision.watch;
    for kernel in [&owned, &naive, &served, &default_graph] {
        seed_items(kernel, ITEMS);
    }

    println!("--- {ITEMS} items, mean of 20 reads ---");
    for (label, kernel) in [
        ("owned     (open — no browse face)", &owned),
        ("shared    (open_shared, undeclared)", &naive),
        ("declared  (browse in the DEFAULT graph)", &default_graph),
        ("declared  (browse in its NAMED graph — shipped)", &served),
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
        "the declared composition must cache a ledger read: {hot:?}"
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
    // ★ And the control, asserted rather than only printed: the ledger's read is cached on
    // BOTH sides of the graph decision. If naming the browse graph had de-cached the ledger —
    // the regression obligation 3 exists to prevent — this is where it would show, with the
    // timings above as the size of it.
    let before = cached(&default_graph);
    assert!(
        before.iter().any(|iri| iri == "urn:iki:ledger:items")
            && before.iter().any(|iri| iri == "urn:iki:store:graph-select"),
        "the composition as it shipped before the graph decision: {before:?}"
    );
    // And the broad face stays uncacheable in BOTH, by name: it can see the default graph,
    // where browse's invisible writes land.
    for (label, kernel) in [("declared", &served), ("naive", &naive)] {
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
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    // Wired with an EMPTY watched set — what `main` builds for a root whose platform watcher
    // refused to start.
    let wired = browse::wire(roots(&dir), handle, &[], None, &graph);
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        None,
    ));
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
