//! `ikigai-conformance` over the kernel this binary serves — and over a door onto it.
//!
//! # This crate binds ZERO endpoints
//!
//! Everything gonk serves is `ikigai-store`'s (thirteen resources) or `ikigai-ledger`'s
//! (fourteen), and each of those crates walks its own suite. What is gonk's to get wrong is
//! the composition and the doors, so this file checks exactly those:
//!
//! - [`the_served_catalog_is_exactly_the_store_and_the_ledger`] — linkage-gating as a
//!   machine-checked fact: a dependency bump that puts one more resource behind the ports is
//!   a red test, and so is one that silently binds one fewer.
//! - [`the_hub_conforms`] — the suite over [`ikigai_gonk::compose`], the function `main`
//!   calls, with the ledger's own fixtures.
//! - [`a_door_kernel_conforms_like_the_hub`] — the same walk through
//!   [`ikigai_gonk::doors::door_kernel`], which is what the socket and QUIC transports
//!   actually serve. A forwarding space that lost a contract, a verb or a declared
//!   capability would be clean on the hub and dirty here.
//! - [`every_entry_answers_meta_in_json_through_a_door`] — a mounting client reads every
//!   contract through the JSON Meta face, and when that fails it degrades silently.
//!
//! ⚠ **This file walks the composition with NO browse roots** — the store and the ledgers,
//! which is what `main` builds when nothing names a root. The browse composition adds twenty
//! more resources and a shared store handle, and it has its own file (`tests/browse.rs`):
//! keeping them apart is what lets this one keep asserting the catalog exactly.
//!
//! The store's thirteen resources are opted out of the invoking checks for the reason the
//! ledger's suite gives: they are walked with real SPARQL by `ikigai-store`'s own suite, and
//! the synthesized `"x"` this suite would hand them is not a query.

use std::collections::BTreeSet;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_conformance::{Check, Fixture, Report, Suite};
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::{compose, doors};
use ikigai_store::DurableStore;

const STORE_IDS: [&str; 13] = [
    "store-select",
    "store-ask",
    "store-construct",
    "store-describe",
    "store-graph-select",
    "store-graph-ask",
    "store-graph-construct",
    "store-graph-describe",
    "store-info",
    "store-graphs",
    "store-update",
    "store-graph-update",
    "store-load",
];

const LEDGER_IDS: [&str; 14] = [
    "ledger-append",
    "ledger-claim",
    "ledger-close",
    "ledger-comment",
    "ledger-defer",
    "ledger-item",
    "ledger-items",
    "ledger-label",
    "ledger-ledgers",
    "ledger-link",
    "ledger-next",
    "ledger-policy",
    "ledger-purge",
    "ledger-reopen",
];

fn hub() -> Arc<Kernel> {
    Arc::new(compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ))
}

fn issue(kernel: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let repr = block_on(kernel.issue(request, &Capability::root()))
        .unwrap_or_else(|e| panic!("{verb:?} {iri} {args:?}: {e}"));
    String::from_utf8_lossy(&repr.bytes).into_owned()
}

/// Three items, filed through `kernel`, and their ids — A for most fixtures, B as a link
/// target, C for purge.
fn seed(kernel: &Kernel) -> (String, String, String) {
    let mut ids = [
        "The fixture item\n\nWhat the walk writes to.",
        "A link target",
        "A purge target",
    ]
    .into_iter()
    .map(|content| {
        let answer = issue(
            kernel,
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", content)],
        );
        let iri = answer
            .split_whitespace()
            .nth(1)
            .unwrap_or_else(|| panic!("append answers `#N <iri>`: {answer}"));
        iri.rsplit(':')
            .next()
            .expect("an item IRI ends in its id")
            .to_string()
    });
    (
        ids.next().unwrap(),
        ids.next().unwrap(),
        ids.next().unwrap(),
    )
}

/// `ikigai-ledger`'s own fixtures, pointed at the seeded items.
fn fixtures(a: &str, b: &str, c: &str) -> Suite {
    let item = format!("urn:iki:ledger:default:item:{a}");
    let target = format!("urn:iki:ledger:default:item:{b}");
    let purge_target = format!("urn:iki:ledger:default:item:{c}");
    STORE_IDS
        .iter()
        .fold(Suite::new(), |suite, id| {
            suite.opt_out(
                *id,
                None,
                "ikigai-store's own conformance suite walks it with real SPARQL; it is bound \
                 here because every ledger read and write composes over it",
            )
        })
        .namespace("https://ikigai-rs.dev/ns/ledger#")
        .fixture(Fixture::new("ledger-item", Verb::Source).binding("id", a))
        .fixture(Fixture::new("ledger-item", Verb::Exists).binding("id", a))
        .fixture(
            Fixture::new("ledger-item", Verb::Sink)
                .binding("id", a)
                .arg("content", "An edited title\n\nAnd an edited body."),
        )
        .fixture(Fixture::new("ledger-item", Verb::Delete).binding("id", a))
        .fixture(Fixture::new("ledger-policy", Verb::Source).binding("name", "leverage"))
        .fixture(Fixture::new("ledger-policy", Verb::Exists).binding("name", "leverage"))
        .fixture(
            Fixture::new("ledger-comment", Verb::Sink)
                .arg("item", &item)
                .arg("content", "a comment from the conformance walk"),
        )
        .fixture(Fixture::new("ledger-close", Verb::Sink).arg("item", &item))
        .fixture(Fixture::new("ledger-reopen", Verb::Sink).arg("item", &item))
        .fixture(
            Fixture::new("ledger-claim", Verb::Sink)
                .arg("item", &item)
                .arg("content", "the conformance walk"),
        )
        .fixture(Fixture::new("ledger-claim", Verb::Delete).arg("item", &item))
        .fixture(Fixture::new("ledger-defer", Verb::Sink).arg("item", &item))
        .fixture(Fixture::new("ledger-defer", Verb::Delete).arg("item", &item))
        .fixture(
            Fixture::new("ledger-link", Verb::Sink)
                .arg("item", &item)
                .arg("content", &target),
        )
        .fixture(
            Fixture::new("ledger-link", Verb::Delete)
                .arg("item", &item)
                .arg("content", &target),
        )
        .fixture(
            Fixture::new("ledger-label", Verb::Sink)
                .arg("item", &item)
                .arg("content", "conformance"),
        )
        .fixture(
            Fixture::new("ledger-label", Verb::Delete)
                .arg("item", &item)
                .arg("content", "conformance"),
        )
        .fixture(Fixture::new("ledger-purge", Verb::Delete).arg("content", &purge_target))
}

/// The fixtures plus what the reads PROMISE about caching — for a kernel that caches.
fn suite(a: &str, b: &str, c: &str) -> Suite {
    fixtures(a, b, c)
        .cacheable("ledger-items")
        .cacheable("ledger-item")
        .cacheable("ledger-next")
        .pure("ledger-policy")
}

/// Every non-kernel entry's description id.
fn served_ids(kernel: &Kernel) -> BTreeSet<String> {
    kernel
        .entries()
        .expect("an enumerable root")
        .iter()
        .filter(|entry| !entry.pattern.starts_with("urn:kernel:"))
        .map(|entry| {
            kernel
                .describe_pattern(&entry.pattern)
                .unwrap_or_else(|| panic!("`{}` describes itself", entry.pattern))
                .id
        })
        .collect()
}

fn walked_ledger_ids(report: &Report) -> Vec<&str> {
    let mut walked: Vec<&str> = report
        .walked
        .iter()
        .map(String::as_str)
        .filter(|id| id.starts_with("ledger-"))
        .collect();
    walked.sort_unstable();
    walked.dedup();
    walked
}

#[test]
fn the_served_catalog_is_exactly_the_store_and_the_ledger() {
    let expected: BTreeSet<String> = STORE_IDS
        .iter()
        .chain(LEDGER_IDS.iter())
        .map(|id| id.to_string())
        .collect();
    let hub = hub();
    assert_eq!(
        served_ids(&hub),
        expected,
        "the served catalog changed: a linked crate gained or lost a resource, and it is now \
         behind every door this binary opens"
    );
    let door = doors::door_kernel(Arc::clone(&hub));
    assert_eq!(
        served_ids(&door),
        expected,
        "a door serves exactly the hub's catalog"
    );
}

#[test]
fn the_hub_conforms() {
    let hub = hub();
    let (a, b, c) = seed(&hub);
    let report = suite(&a, &b, &c).run_blocking(&hub);
    println!("--- hub ---\n{report}");
    assert!(report.is_clean(), "{report}");
    assert_eq!(walked_ledger_ids(&report), LEDGER_IDS, "{report}");
}

/// The reads whose representations are cacheable — cached by the HUB.
const CACHED_READS: [&str; 5] = [
    "ledger-ledgers",
    "ledger-policy",
    "ledger-items",
    "ledger-item",
    "ledger-next",
];

/// The same walk through a door. Every check runs except `CACHEABLE` on the five reads,
/// and that one is waived because the door is BUILT not to cache: `CACHEABLE` watches the
/// walked kernel's own trace for a hit, and a door kernel stores nothing by design
/// ([`doors::NoCache`]) — its caching happens one hop in, in the hub, which
/// [`the_hub_conforms`] walks with the check on and `tests/doors.rs` proves is invalidated
/// by a write through a door.
#[test]
fn a_door_kernel_conforms_like_the_hub() {
    let door = doors::door_kernel(hub());
    let (a, b, c) = seed(&door);
    let report = CACHED_READS
        .iter()
        .fold(fixtures(&a, &b, &c), |suite, id| {
            suite.opt_out_check(
                *id,
                Check::Cacheable,
                "a door kernel stores nothing by design; the hub it forwards to caches, and \
                 the hub walk runs this check",
            )
        })
        .run_blocking(&door);
    println!("--- door ---\n{report}");
    assert!(report.is_clean(), "{report}");
    assert_eq!(walked_ledger_ids(&report), LEDGER_IDS, "{report}");
}

/// The HTTP door's own resources: bound only in [`doors::http_kernel`].
const WEB_IDS: [&str; 19] = [
    "gonk-k",
    "gonk-browse-page",
    "gonk-browse-roots",
    "gonk-page-queue",
    "gonk-fragment-queue",
    "gonk-queue-decide",
    "gonk-queue-batch",
    "gonk-queue-badge",
    "gonk-page-home",
    "gonk-page-ledger",
    "gonk-fragment-items",
    "gonk-page-item",
    "gonk-fragment-item",
    "gonk-act",
    "gonk-sparql",
    "gonk-fragment-sparql",
    "gonk-passkey",
    "gonk-asset",
    "gonk-render-rules",
];

fn http_door(hub: Arc<Kernel>, config: &std::path::Path) -> Kernel {
    let passkeys = Arc::new(ikigai_gonk::identity::Passkeys::new(
        ikigai_gonk::quic::Layout::in_config_home(config),
        1060,
    ));
    let face = Arc::new(ikigai_gonk::web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new(),
        passkeys,
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
    });
    doors::http_kernel(hub, ikigai_gonk::web::space(face))
}

/// ★ The HTTP door serves the hub's catalog PLUS its pages, and nothing else — and the
/// socket and QUIC doors do not serve the pages at all.
#[test]
fn the_http_door_adds_exactly_its_pages() {
    let config = tempfile::tempdir().unwrap();
    let hub = hub();
    let expected: BTreeSet<String> = STORE_IDS
        .iter()
        .chain(LEDGER_IDS.iter())
        .chain(WEB_IDS.iter())
        .map(|id| id.to_string())
        .collect();
    assert_eq!(
        served_ids(&http_door(Arc::clone(&hub), config.path())),
        expected
    );
    assert!(
        served_ids(&doors::door_kernel(hub))
            .iter()
            .all(|id| !id.starts_with("gonk-")),
        "the pages are the HTTP door's alone"
    );
}

/// The suite over the HTTP door's kernel: the hub's resources as in the door walk, and the
/// ten page resources walked for real.
#[test]
fn the_http_door_conforms() {
    let config = tempfile::tempdir().unwrap();
    let door = http_door(hub(), config.path());
    let (a, b, c) = seed(&door);
    let report = CACHED_READS
        .iter()
        .fold(fixtures(&a, &b, &c), |suite, id| {
            suite.opt_out_check(
                *id,
                Check::Cacheable,
                "the HTTP door's kernel stores nothing by design; the hub it forwards to \
                 caches, and the hub walk runs this check",
            )
        })
        .fixture(Fixture::new("gonk-act", Verb::Sink).arg(
            "content",
            "_ledger=default&_action=append&_then=items&content=Filed+by+the+walk",
        ))
        // ★ The `/k/` adapter, walked through a target this composition actually binds. Its
        // command grammar is `ikigai-browse`'s, so a fixture is the only way the walk can
        // resolve it at all — the IRI it reads is inside an argument, not in its own name.
        //
        // ⚠ Its SINK is left unprobed here, deliberately and with the reason the report asks
        // for: the one family a sink may reach is `urn:iki:annotation`, which this
        // composition does not bind (no browse root, by design — `the_http_door_adds_exactly
        // _its_pages` pins that catalog). The AUTHORITY question it would answer — can a
        // caller holding no grants mutate through an adapter that declares no `requires`? —
        // is answered where the family IS bound and over real HTTP, by
        // `tests/browse.rs::a_cross_site_post_cannot_annotate_through_the_adapter` (an empty
        // capability writes nothing) and `…::the_adapter_sinks_the_annotation_family_and_
        // nothing_else` (the surface it can reach at all).
        .fixture(Fixture::new("gonk-k", Verb::Source).arg("c", "source urn:iki:ledger:items"))
        // ★ Three waivers, and they are ONE fact about pass-through adapters rather than
        // three excuses: this resource's capability floor, its faces and its cache validity
        // all belong to whatever the command names, which is not known until the command is
        // read. Reported to the hub, because `gonk-act`, a mount's forwarding endpoint and
        // this are the same shape and the suite cannot currently tell any of them from an
        // endpoint that enforces what it did not declare.
        .opt_out_check(
            "gonk-k",
            Check::Enforced,
            "a pass-through adapter enforces NOTHING of its own: the refusal the walk sees \
             is the kernel refusing the TARGET's declared floor, one hop in, under the \
             caller's own capability. Declaring a floor here would be a promise about every \
             resource a command could name — and `gonk-act` can declare one only because \
             every action it can reach shares it",
        )
        .opt_out_check(
            "gonk-k",
            Check::Outputs,
            "its face is its target's face. `source urn:repo:…:file:… as=text/html` serves \
             HTML and `source urn:iki:ledger:items` serves plain text, from ONE action, so \
             any declared list would be a face list for resources this endpoint does not \
             own. The sink is unprobed for a second reason: the one family it may reach \
             (`urn:iki:annotation`) is not bound in this composition — see `tests/browse.rs`, \
             where it is, and where an empty capability is shown to write nothing through it",
        )
        .opt_out_check(
            "gonk-k",
            Check::Cacheable,
            "the same waiver as the five reads above, reached the other way: the adapter \
             forwards the target's representation WHOLE — its bytes, its media type, its \
             cache validity and its golden threads — so a cacheable read stays cacheable to \
             the hub that caches it, and recomputes here because this door kernel stores \
             nothing by design",
        )
        .fixture(
            Fixture::new("gonk-browse-page", Verb::Source).binding("start", "urn:repo:demo:tree"),
        )
        // ★ The Queue's form adapter, walked in a composition that binds NO browse family
        // (this door has no root, by design — `the_http_door_adds_exactly_its_pages` pins
        // that catalog). So the fixture's finding resolves to nothing, and what the walk
        // observes is the half that matters here: the action DECLARES `urn:cap:annotate`, so
        // the ENFORCED check sees the kernel refuse it under an empty capability before the
        // target is ever consulted. What it could not observe — the face a successful
        // decision serves — the report lists as unprobed, which is the honest word for it.
        .fixture(Fixture::new("gonk-queue-decide", Verb::Sink).arg(
            "content",
            "id=0123456789abcdef01234567&decision=publish&severity=minor",
        ))
        .opt_out_check(
            "gonk-queue-decide",
            Check::Outputs,
            "every call this adapter can make is `Sink urn:iki:finding:{id}`, and this \
             composition binds NO browse family — `the_http_door_adds_exactly_its_pages` pins \
             that catalog — so its target is absent and the minimal resolution is a typed \
             NotFound rather than a face. The half that IS observable here is the one worth \
             checking, and it ran: the action declares `urn:cap:annotate`, so ENFORCED saw \
             the kernel refuse it under no grants before the target was ever consulted. The \
             face a successful decision serves is exercised where a browse root exists, by \
             `tests/queue.rs::a_planted_finding_renders_the_contracts_menu_and_publishes`",
        )
        // The batch decline (ledger #506), on the same terms as the single decision above: no
        // browse family is bound here, so its target is absent; what IS observable is that the
        // action declares `urn:cap:annotate`, and ENFORCED sees the kernel refuse it first.
        .fixture(
            Fixture::new("gonk-queue-batch", Verb::Sink)
                .arg("content", "member=0123456789abcdef01234567&reason=whenever"),
        )
        .opt_out_check(
            "gonk-queue-batch",
            Check::Outputs,
            "every call this adapter makes is `Sink urn:iki:finding:{id}`, and this composition \
             binds NO browse family, so the minimal resolution is a typed NotFound rather than \
             a face. Its action declares `urn:cap:annotate` and the browse wildcard, so ENFORCED \
             saw the kernel refuse it under no grants. The face a batch serves is exercised \
             where a browse root exists, by `tests/queue.rs`'s batch tests",
        )
        .fixture(Fixture::new("gonk-page-item", Verb::Source).binding("id", &a))
        .fixture(Fixture::new("gonk-fragment-item", Verb::Source).binding("id", &a))
        // The RDF faces of the SPARQL page answer a CONSTRUCT, so that is what it is walked
        // with; the SELECT faces are the store's own, walked by ikigai-store's suite.
        .fixture(
            Fixture::new("gonk-sparql", Verb::Source).arg("query", "CONSTRUCT WHERE { ?s ?p ?o }"),
        )
        .fixture(
            Fixture::new("gonk-fragment-sparql", Verb::Source)
                .arg("query", "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"),
        )
        // ★ `urn:iki:gonk:render#` is gonk's own, and gonk SERVES it: the rule table at
        // `urn:iki:gonk:render-rules` is both the data and its documentation. It is
        // deliberately not in `ikigai-vocab` — it says nothing about work, only about how
        // this face renders a result cell, and a published vocabulary is a promise to
        // everyone rather than to one server's stylesheet.
        .namespace(ikigai_gonk::rules::RENDER_NS)
        .opt_out_check(
            "gonk-passkey",
            Check::Authority,
            "a sign-in door is public by design: minting a challenge and presenting an \
             assertion are what a caller with no grant does to get one. What it can write is \
             bounded — a challenge that expires, or an enrolment that needs a one-time invite \
             code — and every other mutation on this door is gated by the ledger it reaches",
        )
        .run_blocking(&door);
    println!("--- http door ---\n{report}");
    assert!(report.is_clean(), "{report}");
    let walked: BTreeSet<&str> = report
        .walked
        .iter()
        .map(String::as_str)
        .filter(|id| id.starts_with("gonk-"))
        .collect();
    assert_eq!(walked, WEB_IDS.iter().copied().collect(), "{report}");
}

#[test]
fn every_entry_answers_meta_in_json_through_a_door() {
    let door = doors::door_kernel(hub());
    let entries = door.entries().expect("an enumerable root");
    let concrete: Vec<&str> = entries
        .iter()
        .map(|entry| entry.pattern.as_str())
        .filter(|pattern| !pattern.starts_with("urn:kernel:") && !pattern.contains('{'))
        .collect();
    assert!(concrete.len() >= STORE_IDS.len(), "{concrete:?}");
    for pattern in concrete {
        let id = door.describe_pattern(pattern).expect("described").id;
        let body = issue(&door, Verb::Meta, pattern, &[("as", "application/json")]);
        assert!(
            body.trim_start().starts_with('{') && body.contains(&id),
            "`{pattern}`: the JSON Meta face must be JSON naming `{id}`:\n{body}"
        );
    }
}
