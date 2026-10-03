//! The judge's verdict on the Queue (ledger #696): ordered — and, since ledger #704, the
//! undecided refuted hidden by default, one click away.
//!
//! - [`the_queue_orders_by_verdict_and_folds_the_refuted_last`] — shown on request: confirmed
//!   first, then unjudged, then unsure, then refuted, folded and still decidable; every row
//!   says its standing in words with the judge's tag.
//! - [`the_queue_hides_the_refuted_by_default_and_one_click_shows_them`] — ledger #704: the
//!   default leaves them out and says how many, the link brings them back folded, an unjudged
//!   and an unsure row are never hidden, and nothing is written.
//! - [`a_batch_group_hides_its_refuted_members_and_says_so`] and
//!   [`a_group_with_only_refuted_members_is_not_shown_and_is_counted`] — the same in the batch
//!   view.
//! - [`each_batch_group_orders_its_members_by_verdict`] — the same order inside a group, a
//!   folded member still carrying its box.
//! - [`no_verdict_word_is_written_down_in_this_crate`] — the anti-drift guard's fourth
//!   sibling, with the triage order checked against the judge contract itself.
//!
//! Every verdict here is PLANTED, as `ikigai-browse`'s judge stores one, through the browse
//! graph's own write token — a judge needs a model, and what is tested is gonk's routing on
//! the shape browse documents, not the model.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Exact, Invocation, Iri,
    Kernel, ReprType, Representation, Request, Space, Verb,
};
use ikigai_gonk::backfill::{self, Backfill};
use ikigai_gonk::config::{ExplainTiers, QueuePolicy};
use ikigai_gonk::grants::{browse_graph_grants, grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::trigger::Activity;
use ikigai_gonk::{browse, compose_with, doors, queue, quic, verdict, web};
use ikigai_store::DurableStore;
use tempfile::TempDir;

const ROOT: &str = "demo";

/// The file every finding here quotes, one line per finding.
const LIB: &str = "fn alpha() {}\nfn beta() {}\nfn gamma() {}\nfn delta() {}\nfn omega() {}\n";

fn scratch_root() -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir(dir.path().join("src")).expect("src/");
    std::fs::write(dir.path().join("src/lib.rs"), LIB).expect("lib.rs");
    dir
}

/// The HTTP door's kernel over a hub carrying the browse family WITH its explain families
/// bound (so the judge's contract is there to read) — and no model behind them: nothing here
/// derives, so nothing here dials one.
fn door(dir: &TempDir) -> (Kernel, Arc<Kernel>, TempDir) {
    door_with(dir, &ExplainTiers::default(), Vec::new(), Vec::new(), None)
}

/// The same, with the tiers, the mounted spaces (a fake `urn:llm:`) and extra spaces (the
/// backfill) a test chooses — and, as `main` wires it, the pass counters the browse family
/// counts every review on.
fn door_with(
    dir: &TempDir,
    tiers: &ExplainTiers,
    mounted: Vec<Arc<dyn Space>>,
    extra: Vec<Arc<dyn Space>>,
    reviews: Option<Arc<Activity>>,
) -> (Kernel, Arc<Kernel>, TempDir) {
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let roots: Vec<(String, PathBuf)> = vec![(ROOT.to_string(), dir.path().to_path_buf())];
    let wired = browse::wire(roots, handle, None, Some(tiers), &graph);
    let space = match reviews {
        Some(activity) => wired.space.observing_reviews(activity),
        None => wired.space,
    };
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(space)),
        mounted,
        extra,
        None,
    ));
    let config = tempfile::tempdir().expect("a config home");
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: vec![ROOT.to_string()],
        passkeys: Arc::new(Passkeys::new(
            quic::Layout::in_config_home(config.path()),
            1060,
        )),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: QueuePolicy::default(),
        epochs: None,
    });
    (
        doors::http_kernel(Arc::clone(&hub), web::space(face)),
        hub,
        config,
    )
}

fn reviewer() -> Capability {
    let mut scopes = vec![
        ikigai_browse::CAP_WILDCARD.to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ];
    scopes.extend(grants_for_all(&["default".to_string()], Authority::Write).expect("tokens"));
    scopes.extend(browse_graph_grants(Authority::Write).expect("a named browse graph"));
    Capability::scoped(scopes)
}

fn issue(
    kernel: &Kernel,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
    cap: &Capability,
) -> ikigai_core::Result<Representation> {
    let mut request = Request::new(verb, Iri::parse(iri).expect("an IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    block_on(kernel.issue(request, cap))
}

fn page(kernel: &Kernel, args: &[(&str, &str)]) -> String {
    let answer = issue(kernel, Verb::Source, queue::QUEUE_IRI, args, &reviewer())
        .unwrap_or_else(|e| panic!("the queue page: {e}"));
    String::from_utf8(answer.bytes).expect("utf-8")
}

fn graph_iri() -> String {
    browse::Graph::chosen()
        .named()
        .expect("a named browse graph")
        .as_str()
        .to_string()
}

fn update(door: &Kernel, sparql: &str) {
    issue(
        door,
        Verb::Sink,
        "urn:iki:store:graph-update",
        &[("graph", &graph_iri()), ("content", sparql)],
        &reviewer(),
    )
    .expect("the browse graph's write token plants it");
}

/// A serious word from the finding contract — never spelled here.
fn serious_word(door: &Kernel) -> String {
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

/// One pending serious finding on `src/lib.rs`, anchored at `exact` — the quads browse's own
/// `store_annotation` writes for a finding.
fn plant_finding(door: &Kernel, id: &str, exact: &str, body: &str) {
    plant_finding_from(
        door,
        id,
        exact,
        body,
        "urn:ikigai:browse:review:demo:src/lib.rs",
        &serious_word(door),
    );
}

/// The same, minted by a named pass and rated any word — a pass IRI that names a content hash
/// neither the file nor any commit has is a finding judge-finding answers `cannot` for.
fn plant_finding_from(
    door: &Kernel,
    id: &str,
    exact: &str,
    body: &str,
    pass: &str,
    severity: &str,
) {
    // An empty word plants an UNRATED finding: no proposal at all.
    let rated = match severity {
        "" => String::new(),
        word => format!("sh:resultSeverity <urn:iki:severity:{word}> ;"),
    };
    let start = LIB.find(exact).expect("the quote is in the file");
    let end = start + exact.len();
    update(
        door,
        &format!(
            r#"PREFIX oa: <http://www.w3.org/ns/oa#>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX sh: <http://www.w3.org/ns/shacl#>
PREFIX ik: <https://ikigai-rs.dev/ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
INSERT DATA {{ GRAPH <{graph}> {{
  <urn:iki:finding:{id}> a prov:Entity ;
    dcterms:description "{body}" ;
    dcterms:creator "a-test-reviewer" ;
    dcterms:created "2026-10-01T12:00:00Z"^^xsd:dateTime ;
    prov:wasGeneratedBy <{pass}> ;
    {rated}
    ik:annotates <urn:repo:{ROOT}:file:src/lib.rs> ;
    ik:repo "{ROOT}" ;
    ik:path "src/lib.rs" ;
    ik:contentHash "sha256:planted" ;
    oa:hasSelector <urn:iki:finding:{id}:selector:quote> ,
                   <urn:iki:finding:{id}:selector:position> .
  <urn:iki:finding:{id}:selector:quote> a oa:TextQuoteSelector ;
    oa:exact "{exact}" .
  <urn:iki:finding:{id}:selector:position> a oa:TextPositionSelector ;
    oa:start "{start}"^^xsd:nonNegativeInteger ;
    oa:end "{end}"^^xsd:nonNegativeInteger .
}} }}"#,
            graph = graph_iri()
        ),
    );
}

/// The judge's tag every planted verdict carries.
const TAG: &str = "judge-v1@a-test-judge";

/// One verdict on one finding, as browse's `judge::store_verdict` writes it: the verdict node
/// keyed `{finding}:judge:{tag}`, its four answers as parts, each with a reason.
fn plant_verdict(door: &Kernel, id: &str, word: &str, answers: [(&str, &str); 4]) {
    plant_verdict_tagged(door, id, word, answers, TAG);
}

/// The same under any judge's tag — keyed exactly as browse keys it, so judge-finding's
/// Exists finds it.
fn plant_verdict_tagged(
    door: &Kernel,
    id: &str,
    word: &str,
    answers: [(&str, &str); 4],
    tag: &str,
) {
    let v = format!("urn:iki:finding:{id}:judge:{tag}");
    let questions = ["code", "disclosed", "occurs", "test"];
    let mut parts = String::new();
    for (question, (answer, reason)) in questions.iter().zip(answers) {
        let answer = answer.replace('/', "-");
        parts.push_str(&format!(
            "  <{v}> dcterms:hasPart <{v}:answer:{question}> .\n  <{v}:answer:{question}> \
             dcterms:identifier \"{question}\" ; dcterms:type <urn:iki:judge:answer:{answer}> ; \
             dcterms:description \"{reason}\" .\n"
        ));
    }
    update(
        door,
        &format!(
            r#"PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX ik: <https://ikigai-rs.dev/ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
INSERT DATA {{ GRAPH <{graph}> {{
  <{v}> a prov:Activity ;
    prov:used <urn:iki:finding:{id}> ;
    dcterms:type <urn:iki:judge:verdict:{word}> ;
    ik:versionTag "{tag}" ;
    dcterms:creator "a-test-judge" ;
    dcterms:subject <urn:iki:judge:site:code> ;
    dcterms:description "planted" ;
    dcterms:created "2026-10-02T09:00:00Z"^^xsd:dateTime .
{parts}}} }}"#,
            graph = graph_iri()
        ),
    );
}

/// The ids, in the order their rows appear on a page.
fn order_on(html: &str, ids: &[&str]) -> Vec<String> {
    let mut at: Vec<(usize, String)> = ids
        .iter()
        .map(|id| {
            let pos = html
                .find(&format!("value='{id}'"))
                .unwrap_or_else(|| panic!("`{id}` is not on the page:\n{html}"));
            (pos, (*id).to_string())
        })
        .collect();
    at.sort();
    at.into_iter().map(|(_, id)| id).collect()
}

const UPHELD: &str = "aaaaaaaaaaaaaaaaaaaa0001";
const NOBODY: &str = "aaaaaaaaaaaaaaaaaaaa0002";
const UNSURE: &str = "aaaaaaaaaaaaaaaaaaaa0003";
const REFUTED: &str = "aaaaaaaaaaaaaaaaaaaa0004";

/// Four serious findings, one per standing, planted so that browse's own order (by position
/// in the file) is the REVERSE of the judge's: the refuted one quotes the first line.
fn plant_four(door: &Kernel) {
    let [first, middle, last] = verdict::triage();
    plant_finding(door, REFUTED, "fn alpha() {}", "alpha is wrong");
    plant_finding(door, UNSURE, "fn beta() {}", "beta is wrong");
    plant_finding(door, NOBODY, "fn gamma() {}", "gamma is wrong");
    plant_finding(door, UPHELD, "fn delta() {}", "delta is wrong");
    plant_verdict(
        door,
        UPHELD,
        first,
        [
            ("yes", "it does that"),
            ("no", "nothing says so"),
            ("yes", "it would"),
            ("no", "not a test"),
        ],
    );
    plant_verdict(
        door,
        UNSURE,
        middle,
        [
            ("no", "not quite"),
            ("no", "nothing says so"),
            ("yes", "maybe"),
            ("no", "not a test"),
        ],
    );
    plant_verdict(
        door,
        REFUTED,
        last,
        [
            ("no", "it does not"),
            ("yes", "the comment above says so"),
            ("no", "it would not"),
            ("no", "not a test"),
        ],
    );
}

/// The page argument that lists the refuted rows — the verdict module's own name and value.
fn shown() -> [(&'static str, &'static str); 1] {
    [(verdict::REFUTED_ARG, verdict::SHOW)]
}

/// ★★ **Ordered** (Brian, 2026-10-02), as the page draws it when asked to SHOW the refuted
/// (ledger #704): confirmed, then unjudged, then unsure, then refuted — the refuted one FOLDED,
/// its answers inside, its decision form intact — and every row saying its standing in words
/// with the judge's tag.
#[test]
fn the_queue_orders_by_verdict_and_folds_the_refuted_last() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    plant_four(&door);
    let [first, middle, last] = verdict::triage();

    // browse's own order is by position: the refuted one first.
    let rows: Vec<serde_json::Value> = serde_json::from_slice(
        &issue(
            &door,
            Verb::Source,
            &format!("urn:repo:{ROOT}:findings"),
            &[("as", "application/json")],
            &reviewer(),
        )
        .expect("the findings read")
        .bytes,
    )
    .expect("json rows");
    assert_eq!(rows[0]["id"], REFUTED, "the fixture is the reverse order");
    assert_eq!(
        rows[0]["judge"]["verdict"], last,
        "browse reads the planted verdict"
    );

    let html = page(&door, &shown());
    assert_eq!(
        order_on(&html, &[UPHELD, NOBODY, UNSURE, REFUTED]),
        [UPHELD, NOBODY, UNSURE, REFUTED],
        "confirmed, unjudged, unsure, refuted:\n{html}"
    );
    // Every row says where it stands, in words, with the judge's tag.
    for word in [first, middle, last] {
        assert!(
            html.contains(&format!("judge: {word} · {TAG}")),
            "the `{word}` row's label:\n{html}"
        );
    }
    assert!(html.contains("judge: not yet judged"), "{html}");
    // The page says the list is ordered, and how many stand where.
    assert!(
        html.contains(&format!("1 {first} first, 1 not yet judged, 1 {middle}")),
        "{html}"
    );
    // The refuted row is folded — closed — with its answers and reasons inside and OPEN, and
    // its decision form still there: decidable, never hidden.
    let fold = html.find("class='verdict-fold'").expect("a folded row");
    let refuted = html.find(&format!("value='{REFUTED}'")).expect("its form");
    assert!(fold < refuted, "the form is inside the fold:\n{html}");
    let inside = &html[fold..];
    assert!(inside.contains("the comment above says so"), "{inside}");
    assert!(inside.contains("disclosed: yes"), "{inside}");
    assert!(
        inside.contains("open='open'"),
        "the answers are open inside the fold:\n{inside}"
    );
    assert_eq!(
        html.matches("class='verdict-fold'").count(),
        1,
        "only the refuted row is folded:\n{html}"
    );
    // Nothing hidden: all four are drawn, and the count is browse's four.
    assert!(
        html.contains("4 serious pending findings in demo"),
        "{html}"
    );
    // ★ And the page says it is showing them, with the one click that hides them again.
    assert!(
        html.contains(&format!(
            "1 undecided finding the judge {last} is shown, folded last, because this page was \
             asked to show it."
        )),
        "{html}"
    );
    assert!(
        html.contains("href='/queue?state=pending'") && html.contains(">hide it</a>"),
        "{html}"
    );
}

/// ★★ **Hidden by default, one click away** (Brian, 2026-10-02, ledger #704). The default Queue
/// leaves out the undecided row the judge refuted, says how many it left out with the link
/// that lists them, and never hides the unjudged or the unsure; the link brings it back folded
/// with its form, and the mode rides the form so a decision's re-render keeps it. Nothing is
/// written: the row is still pending, undecided, with its verdict.
#[test]
fn the_queue_hides_the_refuted_by_default_and_one_click_shows_them() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    plant_four(&door);
    let [first, middle, last] = verdict::triage();

    let html = page(&door, &[]);
    assert!(
        !html.contains(&format!("value='{REFUTED}'")),
        "the refuted row is not drawn:\n{html}"
    );
    // The unjudged and the unsure are never hidden; the confirmed leads.
    assert_eq!(
        order_on(&html, &[UPHELD, NOBODY, UNSURE]),
        [UPHELD, NOBODY, UNSURE],
        "{html}"
    );
    assert!(!html.contains("class='verdict-fold'"), "{html}");
    // How many, in words, and the link that lists them.
    assert!(
        html.contains(&format!(
            "1 undecided finding the judge {last} is hidden. Nothing was recorded: it is still \
             waiting and decidable"
        )),
        "the line says how many:\n{html}"
    );
    let show = format!(
        "/queue?state=pending&amp;{}={}",
        verdict::REFUTED_ARG,
        verdict::SHOW
    );
    assert!(
        html.contains(&format!("href='{show}'")) && html.contains(">show it</a>"),
        "the one click:\n{html}"
    );
    // The order sentence names them as hidden, never as absent; the count is what is listed.
    assert!(
        html.contains(&format!(
            "1 {first} first, 1 not yet judged, 1 {middle}, 1 {last}, hidden (see above)"
        )),
        "{html}"
    );
    assert!(!html.contains("Nothing is hidden"), "{html}");
    assert!(
        html.contains("3 serious pending findings in demo"),
        "{html}"
    );
    // Every severity view hides it too.
    let all = page(&door, &[("severity", "all")]);
    assert!(!all.contains(&format!("value='{REFUTED}'")), "{all}");
    assert!(all.contains(">show it</a>"), "{all}");

    // ★ Nothing was written: the finding is still pending, undecided, with its verdict.
    let rows: Vec<serde_json::Value> = serde_json::from_slice(
        &issue(
            &door,
            Verb::Source,
            &format!("urn:repo:{ROOT}:findings"),
            &[("as", "application/json")],
            &reviewer(),
        )
        .expect("the findings read")
        .bytes,
    )
    .expect("json rows");
    let row = rows
        .iter()
        .find(|r| r["id"] == REFUTED)
        .expect("still pending");
    assert!(row["decision"].is_null(), "{row}");
    assert_eq!(row["judge"]["verdict"], last);

    // The click: the row is back, folded, decidable — and its form carries the mode, so the
    // re-render after a decision on THIS page keeps showing them.
    let back = page(&door, &shown());
    assert!(back.contains(&format!("value='{REFUTED}'")), "{back}");
    assert!(back.contains("class='verdict-fold'"), "{back}");
    let field = verdict::form_field();
    assert!(
        back.contains(&format!(
            "name='{field}' type='hidden' value='{}'",
            verdict::SHOW
        )),
        "the forms carry the mode:\n{back}"
    );
    let decided = issue(
        &door,
        Verb::Sink,
        queue::DECIDE_IRI,
        &[(
            "content",
            &format!(
                "id={UNSURE}&decision=publish&_state=pending&_repo=&_severity=serious&{field}={}",
                verdict::SHOW
            ),
        )],
        &reviewer(),
    )
    .expect("the decide adapter");
    let after = String::from_utf8(decided.bytes).expect("utf-8");
    assert!(
        after.contains(&format!("value='{REFUTED}'")),
        "the re-render keeps the refuted shown:\n{after}"
    );
}

/// A value the argument does not declare is refused by name, never read as the default.
#[test]
fn an_undeclared_refuted_value_is_refused() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    let err = issue(
        &door,
        Verb::Source,
        queue::QUEUE_IRI,
        &[(verdict::REFUTED_ARG, "maybe")],
        &reviewer(),
    )
    .expect_err("refused");
    assert!(
        matches!(&err, ikigai_core::Error::InvalidArgument { name, .. } if name == verdict::REFUTED_ARG),
        "{err:?}"
    );
}

/// A DECIDED refuted finding is a record, and a record is never folded away.
#[test]
fn a_decided_refuted_finding_is_not_folded() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    plant_four(&door);
    let decided = issue(
        &door,
        Verb::Sink,
        queue::DECIDE_IRI,
        &[(
            "content",
            &format!("id={REFUTED}&decision=publish&_state=pending&_repo=&_severity=serious"),
        )],
        &reviewer(),
    )
    .expect("the decide adapter");
    let html = String::from_utf8(decided.bytes).expect("utf-8");
    assert!(!html.contains("class='verdict-fold'"), "{html}");
    let published = page(&door, &[("state", "published")]);
    let [_, _, last] = verdict::triage();
    assert!(
        published.contains(&format!("judge: {last} · {TAG}")),
        "the record still says the judge's word:\n{published}"
    );
    assert!(!published.contains("class='verdict-fold'"), "{published}");
}

/// A group kind whose grouped read puts every one of `ids` in one group (the whole file) —
/// read from the findings contract and browse's own answer, never named here.
fn kind_holding(door: &Kernel, ids: &[&str]) -> String {
    let kinds = queue::one_of(
        door,
        &format!("urn:repo:{ROOT}:findings"),
        Verb::Source,
        ikigai_gonk::batch::GROUP_ARG,
    )
    .expect("the group kinds");
    kinds
        .into_iter()
        .find(|kind| {
            let answer = issue(
                door,
                Verb::Source,
                &format!("urn:repo:{ROOT}:findings"),
                &[("as", "application/json"), ("group", kind)],
                &reviewer(),
            )
            .expect("the grouped read");
            let body: serde_json::Value = serde_json::from_slice(&answer.bytes).expect("json");
            body["groups"].as_array().is_some_and(|groups| {
                groups.iter().any(|g| {
                    let members: Vec<&str> = g["members"]
                        .as_array()
                        .map(|m| m.iter().filter_map(|r| r["id"].as_str()).collect())
                        .unwrap_or_default();
                    ids.iter().all(|id| members.contains(id))
                })
            })
        })
        .expect("a kind that groups the whole file")
}

/// The same order inside a batch group, a refuted member folded with its box OUTSIDE the fold.
#[test]
fn each_batch_group_orders_its_members_by_verdict() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    plant_four(&door);
    let ids = [UPHELD, NOBODY, UNSURE, REFUTED];
    let kind = kind_holding(&door, &ids);
    let html = page(&door, &[("group", &kind), shown()[0]]);
    assert_eq!(
        order_on(&html, &ids),
        ids,
        "the group's members in the judge's order:\n{html}"
    );
    let fold = html.find("class='verdict-fold'").expect("a folded member");
    let tick = html
        .find(&format!("value='{REFUTED}'"))
        .expect("the refuted member's box");
    assert!(tick < fold, "the box sits outside the fold:\n{html}");
}

/// ★ The batch view hides a refuted member by default (ledger #704): it is not drawn, so no
/// batch here can decline it; the group says how many it lost and the page says how many in
/// all, with the link that lists them. The unjudged and the unsure members stay.
#[test]
fn a_batch_group_hides_its_refuted_members_and_says_so() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    plant_four(&door);
    let [_, _, last] = verdict::triage();
    let kind = kind_holding(&door, &[UPHELD, NOBODY, UNSURE, REFUTED]);
    let html = page(&door, &[("group", &kind)]);
    assert!(
        !html.contains(&format!("value='{REFUTED}'")),
        "the refuted member has no box:\n{html}"
    );
    assert_eq!(
        order_on(&html, &[UPHELD, NOBODY, UNSURE]),
        [UPHELD, NOBODY, UNSURE],
        "{html}"
    );
    assert!(
        html.contains(&format!(
            "1 undecided finding in this group the judge {last} is hidden"
        )),
        "the group says what it lost:\n{html}"
    );
    assert!(
        html.contains(&format!(
            "1 undecided finding the judge {last} is left out of these groups, so no batch \
             here declines it."
        )),
        "{html}"
    );
    assert!(
        html.contains(&format!(
            "href='/queue?group={kind}&amp;{}={}'",
            verdict::REFUTED_ARG,
            verdict::SHOW
        )),
        "the one click:\n{html}"
    );
    // The batch form carries the mode for its re-render (empty: the default).
    assert!(
        html.contains(&format!(
            "name='{}' type='hidden' value=''",
            verdict::form_field()
        )),
        "{html}"
    );
}

/// ★ A group whose every member the filter left out is NOT shown — a group of nothing is not a
/// proposal — and the line above the groups counts it, so it is not silently gone; the
/// empty-view sentence does not claim that nothing is waiting.
#[test]
fn a_group_with_only_refuted_members_is_not_shown_and_is_counted() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    let [_, _, last] = verdict::triage();
    let other = "aaaaaaaaaaaaaaaaaaaa0005";
    plant_finding(&door, REFUTED, "fn alpha() {}", "alpha is wrong");
    plant_finding(&door, other, "fn beta() {}", "beta is wrong");
    let answers = [
        ("no", "it does not"),
        ("yes", "the comment above says so"),
        ("no", "it would not"),
        ("no", "not a test"),
    ];
    plant_verdict(&door, REFUTED, last, answers);
    plant_verdict(&door, other, last, answers);
    let kind = kind_holding(&door, &[REFUTED, other]);
    let html = page(&door, &[("group", &kind)]);
    for id in [REFUTED, other] {
        assert!(!html.contains(&format!("value='{id}'")), "{html}");
    }
    assert!(
        html.contains(&format!(
            "2 undecided findings the judge {last} are left out of these groups, so no batch \
             here declines them. 1 group had no other member and is not shown."
        )),
        "{html}"
    );
    assert!(
        html.contains("every member left was one the line above hides"),
        "{html}"
    );
    assert!(!html.contains("Nothing here is waiting"), "{html}");
    // Shown, the group is back with both members folded.
    let back = page(&door, &[("group", &kind), shown()[0]]);
    for id in [REFUTED, other] {
        assert!(back.contains(&format!("value='{id}'")), "{back}");
    }
}

/// ★ The anti-drift guard's fourth sibling. The verdict words have no `one_of` in browse's
/// contract — the judge states them only in the summary of `urn:repo:{repo}:judge:{path}` —
/// so `src/verdict.rs` spells them ONCE, and `main` checks them against that summary at start.
/// This asserts both halves: the check passes against the contract this build links, and no
/// other page file spells a verdict word.
///
/// ⚠ One word is also the NAME of a decision field browse 0.14.0 computes (`confirmed`, read
/// with `.get(…)`), so a quoted spelling is allowed exactly where it is a field read.
#[test]
fn no_verdict_word_is_written_down_in_this_crate() {
    let dir = scratch_root();
    let (_door, hub, _config) = door(&dir);
    let checked = verdict::check_verdicts(&hub, ROOT)
        .expect("the judge contract states every word")
        .expect("the judge is bound with the explain families");
    assert_eq!(checked, verdict::triage());

    let xsl = include_str!("../web/gonk.xsl");
    let start = xsl.find("the review queue -->").expect("the queue section");
    let end = start + xsl[start..].find("a ledger -->").expect("the next section");
    for (what, source) in [
        ("src/queue.rs", include_str!("../src/queue.rs")),
        ("src/batch.rs", include_str!("../src/batch.rs")),
        ("src/walk.rs", include_str!("../src/walk.rs")),
        ("web/gonk.xsl (the review queue)", &xsl[start..end]),
        ("web/gonk.js", include_str!("../web/gonk.js")),
    ] {
        for word in verdict::triage() {
            let field_read = format!(".get(\"{word}\")");
            let source = source.replace(&field_read, "");
            for spelled in [
                format!("\"{word}\""),
                format!("'{word}'"),
                format!("={word}"),
            ] {
                assert!(
                    !source.contains(&spelled),
                    "`{what}` spells the verdict `{word}` as {spelled}. The words live in \
                     src/verdict.rs, checked against the judge contract."
                );
            }
        }
    }
}

// ------------------------------------------------------------------ the backfill (PR B)

/// The judge provider the backfill tests configure, served by [`FakeJudge`].
const FAKE: &str = "urn:llm:fake:ask";
/// What `urn:llm:fake:model` answers, so the judge's tag is `judge-v1@fake-model`.
const FAKE_MODEL: &str = "fake-model";

/// A judge that answers every claim "confirmed" by the four answers, counts its calls, and —
/// when armed with a backfill — asks that backfill to stop on its first call, so a test can
/// end a run part-way through without a clock.
struct FakeJudge {
    calls: Arc<AtomicUsize>,
    stop_on_first: Arc<Mutex<Option<Arc<Backfill>>>>,
}

#[async_trait]
impl Endpoint for FakeJudge {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        if let Some(backfill) = self.stop_on_first.lock().expect("lock").take() {
            backfill.stop();
        }
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"CODE: yes - it does\nDISCLOSED: no - nothing says so\nOCCURS: yes - it would\nTEST: no - not a test\n"
                .to_vec(),
        ))
    }
    fn name(&self) -> &str {
        "fake-judge"
    }
    fn describe(&self) -> Description {
        let optional = |name: &str| ArgSpec::new(name).optional();
        Description::new("fake-judge")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(optional("prompt"))
            .input(optional("system"))
            .input(optional("temperature"))
            .input(optional("max_tokens"))
    }
}

struct FakeModel;

#[async_trait]
impl Endpoint for FakeModel {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            FAKE_MODEL.as_bytes().to_vec(),
        ))
    }
    fn name(&self) -> &str {
        "fake-model"
    }
    fn describe(&self) -> Description {
        Description::new("fake-model")
            .verb(Verb::Source)
            .verb(Verb::Meta)
    }
}

/// A served gonk with the fake judge mounted and the backfill bound.
struct Backfilled {
    door: Kernel,
    hub: Arc<Kernel>,
    backfill: Arc<Backfill>,
    calls: Arc<AtomicUsize>,
    stop_on_first: Arc<Mutex<Option<Arc<Backfill>>>>,
    activity: Arc<Activity>,
    _config: TempDir,
}

fn backfilled(dir: &TempDir, judge: Option<&str>) -> Backfilled {
    backfilled_with(dir, judge, None)
}

/// The same, with the REVIEW tier asking `slow` — a reviewer that holds its call open until
/// the test lets it go, so a pass can be in flight for as long as a test needs.
fn backfilled_with(dir: &TempDir, judge: Option<&str>, slow: Option<SlowReview>) -> Backfilled {
    let calls = Arc::new(AtomicUsize::new(0));
    let stop_on_first = Arc::new(Mutex::new(None));
    let mut space = EndpointSpace::new()
        .bind(
            Exact::new(FAKE),
            FakeJudge {
                calls: Arc::clone(&calls),
                stop_on_first: Arc::clone(&stop_on_first),
            },
        )
        .bind(Exact::new("urn:llm:fake:model"), FakeModel);
    let mut tiers = ExplainTiers {
        judge: judge.map(str::to_string),
        ..ExplainTiers::default()
    };
    if let Some(slow) = slow {
        tiers.review.provider = SLOW.to_string();
        space = space
            .bind(Exact::new(SLOW), slow)
            .bind(Exact::new("urn:llm:slow:model"), FakeModel);
    }
    let llm: Arc<dyn Space> = Arc::new(space);
    let activity = Arc::new(Activity::default());
    let backfill = Arc::new(
        Backfill::new(
            vec![ROOT.to_string()],
            QueuePolicy::default(),
            judge.map(str::to_string),
            vec![
                ikigai_browse::CAP_WILDCARD.to_string(),
                "urn:cap:net:localhost".to_string(),
            ],
            Arc::clone(&activity),
            None,
        )
        .with_pause(Duration::from_millis(20)),
    );
    let (door, hub, config) = door_with(
        dir,
        &tiers,
        vec![llm],
        vec![backfill.space()],
        Some(Arc::clone(&activity)),
    );
    backfill.attach(&hub, false);
    Backfilled {
        door,
        hub,
        backfill,
        calls,
        stop_on_first,
        activity,
        _config: config,
    }
}

/// The backfill's status as JSON, through the kernel under root (the socket door's grant).
fn backfill_status(kernel: &Kernel) -> serde_json::Value {
    let answer = issue(
        kernel,
        Verb::Source,
        backfill::BACKFILL,
        &[("as", "application/json")],
        &Capability::root(),
    )
    .expect("the backfill status");
    serde_json::from_slice(&answer.bytes).expect("json")
}

fn sink_backfill(kernel: &Kernel, word: &str) -> ikigai_core::Result<Representation> {
    issue(
        kernel,
        Verb::Sink,
        backfill::BACKFILL,
        &[("content", word)],
        &Capability::root(),
    )
}

/// Wait for the run to leave its live phases, or fail after ten seconds.
fn settled(kernel: &Kernel) -> serde_json::Value {
    let began = Instant::now();
    loop {
        let status = backfill_status(kernel);
        if !matches!(
            status["phase"].as_str(),
            Some("listing" | "running" | "yielding" | "stopping")
        ) {
            return status;
        }
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "the run never settled: {status}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

const JUDGED_TAG: &str = "judge-v1@fake-model";
const FRESH_1: &str = "bbbbbbbbbbbbbbbbbbbb0001";
const FRESH_2: &str = "bbbbbbbbbbbbbbbbbbbb0002";
const ALREADY: &str = "bbbbbbbbbbbbbbbbbbbb0003";
const LOST: &str = "bbbbbbbbbbbbbbbbbbbb0004";
const MILD: &str = "bbbbbbbbbbbbbbbbbbbb0005";

/// Two serious findings with no verdict, one already judged under the configured judge's tag,
/// one whose reviewed version is lost (its pass names a hash nothing has), and one rated below
/// the serious set.
fn plant_backfill(door: &Kernel) {
    plant_finding(door, FRESH_1, "fn alpha() {}", "alpha is wrong");
    plant_finding(door, FRESH_2, "fn beta() {}", "beta is wrong");
    plant_finding(door, ALREADY, "fn gamma() {}", "gamma is wrong");
    plant_verdict_tagged(
        door,
        ALREADY,
        verdict::triage()[0],
        [("yes", "a"), ("no", "b"), ("yes", "c"), ("no", "d")],
        JUDGED_TAG,
    );
    let lost_pass = format!(
        "urn:ikigai:browse:review:{ROOT}:sha256:{}:review-v5@x:src/lib.rs",
        "0".repeat(64)
    );
    plant_finding_from(
        door,
        LOST,
        "fn delta() {}",
        "delta is wrong",
        &lost_pass,
        &serious_word(door),
    );
    let declared = queue::one_of(
        door,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        "severity",
    )
    .expect("severities");
    let mild = declared
        .into_iter()
        .find(|w| !QueuePolicy::default().is_serious(w))
        .expect("a word below the serious set");
    plant_finding_from(
        door,
        MILD,
        "fn omega() {}",
        "omega is wrong",
        "urn:ikigai:browse:review:demo:src/lib.rs",
        &mild,
    );
}

/// ★ **A run judges each unjudged serious finding ONCE, skips the judged, counts the lost.**
/// The finding already judged under the configured judge's tag costs no call (Exists first);
/// the one whose reviewed version is gone is counted as `cannot` with browse's reason, and the
/// Queue says so on its row; the finding below the serious set is not walked at all. A second
/// run makes no model call: every verdict it would ask for is archived, and the lost one is
/// asked once more and counted again — never looped on.
#[test]
fn a_backfill_judges_each_unjudged_finding_once_and_skips_the_judged() {
    let dir = scratch_root();
    let b = backfilled(&dir, Some(FAKE));
    plant_backfill(&b.door);
    let idle = backfill_status(&b.door);
    assert_eq!(idle["phase"], "idle", "never started on its own: {idle}");

    sink_backfill(&b.door, backfill::START).expect("an operator starts it");
    let first = settled(&b.door);
    assert_eq!(first["phase"], "done", "{first}");
    assert_eq!(
        first["total"], 4,
        "four serious findings, the mild one not walked: {first}"
    );
    assert_eq!(first["judged"], 2, "{first}");
    assert_eq!(first["already_judged"], 1, "{first}");
    assert_eq!(first["cannot"], 1, "{first}");
    assert_eq!(first["failed"], 0, "{first}");
    assert_eq!(first["remaining"], 0, "{first}");
    assert_eq!(first["calls"], 2, "{first}");
    assert_eq!(first["tag"], JUDGED_TAG, "{first}");
    assert_eq!(
        b.calls.load(AtomicOrdering::SeqCst),
        2,
        "one call per unjudged finding"
    );
    assert!(
        first["unjudgeable"][LOST]
            .as_str()
            .is_some_and(|why| !why.is_empty()),
        "the lost one is kept with browse's reason: {first}"
    );

    // The verdicts are browse's, archived on the findings — the Queue reads them back, and
    // the lost one is labeled with the reason and ordered after them.
    let html = page(&b.door, &[]);
    let [first_word, _, _] = verdict::triage();
    assert!(
        html.contains(&format!("judge: {first_word} · {JUDGED_TAG}")),
        "{html}"
    );
    assert!(html.contains("judge: could not judge — "), "{html}");
    assert_eq!(
        order_on(&html, &[FRESH_1, LOST]),
        [FRESH_1, LOST],
        "a judged finding before the one the judge could not judge:\n{html}"
    );
    assert!(
        html.contains("Judge backfill done"),
        "the page says where the run stands:\n{html}"
    );

    sink_backfill(&b.door, backfill::START).expect("a second run");
    let second = settled(&b.door);
    assert_eq!(second["phase"], "done", "{second}");
    assert_eq!(second["judged"], 0, "{second}");
    assert_eq!(second["already_judged"], 3, "{second}");
    assert_eq!(
        second["cannot"], 1,
        "asked once more, counted again: {second}"
    );
    assert_eq!(
        b.calls.load(AtomicOrdering::SeqCst),
        2,
        "the second run paid nothing it had already paid for"
    );
    drop(b.hub);
}

/// ★ **Stop ends a run after the call in flight; start resumes past what it judged.**
#[test]
fn a_stopped_backfill_resumes_past_what_it_judged() {
    let dir = scratch_root();
    let b = backfilled(&dir, Some(FAKE));
    plant_backfill(&b.door);
    // The first model call asks the run to stop: it finishes that one and stops.
    *b.stop_on_first.lock().expect("lock") = Some(Arc::clone(&b.backfill));
    sink_backfill(&b.door, backfill::START).expect("start");
    let stopped = settled(&b.door);
    assert_eq!(stopped["phase"], "stopped", "{stopped}");
    assert_eq!(
        stopped["judged"], 1,
        "the call in flight finished: {stopped}"
    );
    assert!(
        stopped["remaining"].as_u64().is_some_and(|r| r > 0),
        "{stopped}"
    );
    assert_eq!(b.calls.load(AtomicOrdering::SeqCst), 1);

    sink_backfill(&b.door, backfill::START).expect("resume");
    let resumed = settled(&b.door);
    assert_eq!(resumed["phase"], "done", "{resumed}");
    assert_eq!(
        resumed["judged"], 1,
        "only the one not yet judged: {resumed}"
    );
    assert_eq!(resumed["already_judged"], 2, "{resumed}");
    assert_eq!(
        b.calls.load(AtomicOrdering::SeqCst),
        2,
        "two calls across both runs, one per unjudged finding"
    );
}

/// ★ **It yields to a review pass**: while a pass is in flight no judge call is made, and a
/// stop while yielding stops without one.
#[test]
fn a_backfill_waits_while_a_review_pass_is_in_flight() {
    let dir = scratch_root();
    let b = backfilled(&dir, Some(FAKE));
    plant_backfill(&b.door);
    let pass = b.activity.begin(1).expect("a pass in flight");
    sink_backfill(&b.door, backfill::START).expect("start");
    let began = Instant::now();
    while backfill_status(&b.door)["phase"] != "yielding" {
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "it never yielded"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        b.calls.load(AtomicOrdering::SeqCst),
        0,
        "no call while a pass runs"
    );
    sink_backfill(&b.door, backfill::STOP).expect("stop");
    let stopped = settled(&b.door);
    assert_eq!(stopped["phase"], "stopped", "{stopped}");
    assert_eq!(stopped["done"], 0, "{stopped}");
    assert!(
        stopped["yielded_ms"].as_u64().is_some_and(|ms| ms > 0),
        "{stopped}"
    );
    drop(pass);
    assert_eq!(b.calls.load(AtomicOrdering::SeqCst), 0);
}

/// The review tier [`SlowReview`] serves in [`backfilled_with`].
const SLOW: &str = "urn:llm:slow:ask";

/// A reviewer that says it has been asked, then holds the call open until released — a
/// review pass in flight for exactly as long as a test wants one.
#[derive(Clone, Default)]
struct SlowReview {
    entered: Arc<std::sync::atomic::AtomicBool>,
    release: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl Endpoint for SlowReview {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        self.entered.store(true, AtomicOrdering::SeqCst);
        let began = Instant::now();
        while !self.release.load(AtomicOrdering::SeqCst)
            && began.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"[]".to_vec(),
        ))
    }
    fn name(&self) -> &str {
        "slow-review"
    }
    fn describe(&self) -> Description {
        let optional = |name: &str| ArgSpec::new(name).optional();
        Description::new("slow-review")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(optional("prompt"))
            .input(optional("system"))
            .input(optional("temperature"))
            .input(optional("max_tokens"))
    }
}

/// ★★ **A review pass started from the PAGE BUTTON makes the backfill wait too** (ledger
/// [#702](http://localhost:1060/l/default/item/702) item 4). The button does not go through
/// the review queue: browse's file face emits `hx-get="/k/source urn:repo:{root}:review:{path}
/// as=text/html"`, and the `/k/` adapter resolves it under the caller's grant. So the signal
/// the backfill yields to has to be raised where every review passes — the browse family's
/// overlay — not only where the queue's passes begin. Here the button's request is held open
/// at the model, the backfill is started, and it must yield until the click's pass ends.
#[test]
fn a_backfill_waits_while_a_button_review_is_in_flight() {
    let dir = scratch_root();
    let slow = SlowReview::default();
    let b = backfilled_with(&dir, Some(FAKE), Some(slow.clone()));
    plant_backfill(&b.door);
    let clicker = Capability::scoped(vec![
        ikigai_browse::CAP_WILDCARD.to_string(),
        "urn:cap:net:localhost".to_string(),
    ]);
    std::thread::scope(|scope| {
        let click = scope.spawn(|| {
            issue(
                &b.door,
                Verb::Source,
                ikigai_gonk::k::K_IRI,
                &[(
                    "c",
                    &format!("source urn:repo:{ROOT}:review:src/lib.rs as=text/html"),
                )],
                &clicker,
            )
        });
        let began = Instant::now();
        while !slow.entered.load(AtomicOrdering::SeqCst) {
            assert!(
                !click.is_finished(),
                "the click ended before it reached the reviewer: {:?}",
                click.join()
            );
            assert!(
                began.elapsed() < Duration::from_secs(10),
                "the click never reached the reviewer"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        sink_backfill(&b.door, backfill::START).expect("start");
        let began = Instant::now();
        while backfill_status(&b.door)["phase"] != "yielding" {
            let status = backfill_status(&b.door);
            assert!(
                began.elapsed() < Duration::from_secs(10) && status["done"] == 0,
                "it never yielded to the button's pass: {status}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            b.calls.load(AtomicOrdering::SeqCst),
            0,
            "no judge call while the button's pass runs"
        );
        assert_eq!(backfill_status(&b.door)["done"], 0);

        slow.release.store(true, AtomicOrdering::SeqCst);
        let _ = click.join().expect("the click's thread");
    });
    let finished = settled(&b.door);
    assert_eq!(finished["phase"], "done", "{finished}");
    assert_eq!(
        finished["judged"], 2,
        "it went on once the pass ended: {finished}"
    );
    assert!(
        finished["yielded_ms"].as_u64().is_some_and(|ms| ms > 0),
        "{finished}"
    );
}

/// The Sink's refusals: the judge switched off, a word it does not know, and a caller who
/// could not make the judge spend.
#[test]
fn the_backfill_refuses_without_a_judge_a_word_or_a_net_grant() {
    let dir = scratch_root();
    let off = backfilled(&dir, None);
    match sink_backfill(&off.door, backfill::START) {
        Err(ikigai_core::Error::Unavailable(why)) => assert!(why.contains("off"), "{why}"),
        other => panic!("no judge, no run: {other:?}"),
    }
    assert_eq!(backfill_status(&off.door)["phase"], "idle");

    let on = backfilled(&dir, Some(FAKE));
    match sink_backfill(&on.door, "whenever") {
        Err(ikigai_core::Error::InvalidArgument { .. }) => {}
        other => panic!("an undeclared word: {other:?}"),
    }
    match issue(
        &on.door,
        Verb::Sink,
        backfill::BACKFILL,
        &[("content", backfill::START)],
        &reviewer(),
    ) {
        Err(ikigai_core::Error::Denied(_)) => {}
        other => panic!("a browse grant without a net grant may not start it: {other:?}"),
    }
    assert_eq!(on.calls.load(AtomicOrdering::SeqCst), 0);
}

/// The backfill is a module citizen: the conformance walk over a kernel holding it alone.
/// The Sink is walked with `stop` — a word that changes nothing when no run is live, so the
/// walk spends nothing.
#[test]
fn the_backfill_conforms() {
    let backfill = Arc::new(Backfill::new(
        vec![ROOT.to_string()],
        QueuePolicy::default(),
        Some(FAKE.to_string()),
        vec![ikigai_browse::CAP_WILDCARD.to_string()],
        Arc::new(Activity::default()),
        None,
    ));
    let kernel =
        Kernel::with_meta_renderer(backfill.space(), Arc::new(ikigai_vocab::TurtleRenderer));
    let report = ikigai_conformance::Suite::new()
        .fixture(
            ikigai_conformance::Fixture::new("gonk-judge-backfill", Verb::Sink)
                .arg("content", backfill::STOP),
        )
        .live("gonk-judge-backfill")
        .run_blocking(&kernel);
    println!("{report}");
    assert!(report.is_clean(), "{report}");
}

// ------------------------------------------------------- the reproduced mark (PR C)

/// The contract's one reproduced word — never spelled here.
fn reproduced_word(door: &Kernel) -> String {
    queue::one_of(
        door,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        queue::REPRODUCED_ARG,
    )
    .expect("browse 0.16.0 declares the reproduced mark")
    .into_iter()
    .next()
    .expect("one word")
}

fn decide(door: &Kernel, body: &str) -> String {
    String::from_utf8(
        issue(
            door,
            Verb::Sink,
            queue::DECIDE_IRI,
            &[("content", body)],
            &reviewer(),
        )
        .expect("the decide adapter renders")
        .bytes,
    )
    .expect("utf-8")
}

/// A finding's row, as the findings face reads it back.
fn finding_row(door: &Kernel, id: &str) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = serde_json::from_slice(
        &issue(
            door,
            Verb::Source,
            &format!("urn:repo:{ROOT}:findings"),
            &[("as", "application/json"), ("state", "all")],
            &reviewer(),
        )
        .expect("the findings read")
        .bytes,
    )
    .expect("json rows");
    rows.into_iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("no `{id}`"))
}

/// ★ The decide form's box sits beside Publish, carries the contract's word, and a publish
/// with it ticked records the mark — shown on the row in words, with no reproduction form left
/// to offer.
#[test]
fn a_publish_with_the_box_ticked_records_the_reproduced_mark() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    plant_finding(&door, UPHELD, "fn delta() {}", "delta is wrong");
    let word = reproduced_word(&door);
    let html = page(&door, &[]);
    let publish = html.find("value='publish'").expect("the Publish button");
    let box_at = html.find("name='reproduced'").expect("the reproduced box");
    let decline = html.find("value='decline'").expect("the Decline button");
    assert!(
        publish < box_at && box_at < decline,
        "the box is beside Publish:\n{html}"
    );
    let input_at = html[..box_at].rfind("<input").expect("the box is an input");
    let input = &html[input_at..input_at + html[input_at..].find('>').expect("closes")];
    assert!(
        input.contains("type='checkbox'") && input.contains(&format!("value='{word}'")),
        "a checkbox carrying the contract's word: {input}"
    );

    let after = decide(
        &door,
        &format!("id={UPHELD}&decision=publish&reproduced={word}&content=ran+it+and+it+broke&_state=published"),
    );
    assert!(!after.contains("flash error"), "{after}");
    let row = finding_row(&door, UPHELD);
    assert_eq!(row["decision"]["reproduced"], true, "{row}");
    assert_eq!(
        row["decision"]["made"], "single",
        "stamped at the door: {row}"
    );
    assert!(
        after.contains("reproduced — a human showed the defect happen"),
        "the mark, in words:\n{after}"
    );
    assert!(
        !after.contains("class='reproduce'"),
        "a reproduced publication offers no reproduction form:\n{after}"
    );
}

/// A tick followed by Decline is a tick the person abandoned: the decline is recorded and the
/// mark is not sent (browse would refuse it beside a decline).
#[test]
fn a_tick_then_decline_declines_without_the_mark() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    plant_finding(&door, UPHELD, "fn delta() {}", "delta is wrong");
    let word = reproduced_word(&door);
    let after = decide(
        &door,
        &format!("id={UPHELD}&decision=decline&reproduced={word}&_state=pending"),
    );
    assert!(!after.contains("flash error"), "{after}");
    let row = finding_row(&door, UPHELD);
    assert_eq!(row["state"], "declined", "{row}");
    assert_eq!(row["decision"]["reproduced"], false, "{row}");
}

/// ★ A published finding offers a FOLDED "record a reproduction" form that revises its
/// publish: posting it adds the mark and keeps the outcome and the rating.
#[test]
fn a_published_finding_records_a_reproduction_by_revising_its_publish() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    plant_finding(&door, UPHELD, "fn delta() {}", "delta is wrong");
    decide(
        &door,
        &format!("id={UPHELD}&decision=publish&_state=published"),
    );
    let before = finding_row(&door, UPHELD);
    let decision_iri = before["decision"]["iri"]
        .as_str()
        .expect("an IRI")
        .to_string();
    assert_eq!(before["decision"]["reproduced"], false);

    let html = page(&door, &[("state", "published")]);
    let form = html.find("class='reproduce'").expect("the folded form");
    let tail = &html[form..];
    assert!(
        tail.contains("<summary>record a reproduction</summary>"),
        "{tail}"
    );
    assert!(
        !tail[..tail.find("</summary>").unwrap()].contains("open="),
        "folded:\n{tail}"
    );
    assert!(
        tail.contains(&format!("value='{decision_iri}'")),
        "revises the publish:\n{tail}"
    );

    let word = reproduced_word(&door);
    let after = decide(
        &door,
        &format!(
            "id={UPHELD}&decision=publish&reproduced={word}&revises={}&content=a+failing+test&_state=published",
            decision_iri.replace(':', "%3A")
        ),
    );
    assert!(!after.contains("flash error"), "{after}");
    let row = finding_row(&door, UPHELD);
    assert_eq!(row["decision"]["reproduced"], true, "{row}");
    assert_eq!(
        row["decision"]["outcome"], before["decision"]["outcome"],
        "{row}"
    );
    assert_eq!(
        row["decision"]["severity"], before["decision"]["severity"],
        "{row}"
    );
    assert_eq!(row["decision"]["made"], "single", "{row}");
    assert!(after.contains("reproduced — a human showed"), "{after}");
}

/// ★ A refused reproduction keeps its input (ledger #657): the form comes back OPEN, the note
/// the person typed in it, the refusal marked on the row.
#[test]
fn a_refused_reproduction_comes_back_open_with_its_note() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    plant_finding(&door, UPHELD, "fn delta() {}", "delta is wrong");
    decide(
        &door,
        &format!("id={UPHELD}&decision=publish&_state=published"),
    );
    let word = reproduced_word(&door);
    let after = decide(
        &door,
        &format!(
            "id={UPHELD}&decision=publish&reproduced={word}&revises=urn%3Aiki%3Afinding%3Anot-this-one&content=my+careful+steps&_state=published"
        ),
    );
    assert!(
        after.contains("flash error"),
        "a stale revises is refused:\n{after}"
    );
    let form = after
        .find("class='reproduce'")
        .expect("the form is drawn back");
    let tail = &after[form..];
    assert!(tail.contains("open='open'"), "open:\n{tail}");
    assert!(tail.contains("my careful steps"), "with the note:\n{tail}");
    assert!(
        tail.contains("class='problem'"),
        "and the refusal on the row:\n{tail}"
    );
    assert!(
        !after.contains("Your note was not recorded"),
        "the note is in the form, not beside it:\n{after}"
    );
}

/// A refused decision keeps the reproduced tick, as it keeps the rating, word and note.
#[test]
fn a_refused_decision_keeps_the_reproduced_tick() {
    let dir = scratch_root();
    let (door, _hub, _config) = door(&dir);
    // An UNRATED finding: publishing it with no rating is refused (there is no proposal to
    // accept), and the form must come back with the box still ticked.
    plant_finding_from(
        &door,
        UPHELD,
        "fn delta() {}",
        "delta is wrong",
        "urn:ikigai:browse:review:demo:src/lib.rs",
        "",
    );
    let word = reproduced_word(&door);
    let after = decide(
        &door,
        &format!("id={UPHELD}&decision=publish&reproduced={word}&content=the+steps&_state=pending"),
    );
    assert!(after.contains("flash error"), "{after}");
    let at = after.find("name='reproduced'").expect("the box");
    let start = after[..at].rfind("<input").expect("an input");
    let input = &after[start..start + after[start..].find('>').expect("closes")];
    assert!(input.contains("checked"), "still ticked: {input}");
    assert!(after.contains("the steps"), "{after}");
}
