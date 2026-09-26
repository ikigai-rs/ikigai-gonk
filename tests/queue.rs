//! The review Queue (ledger #444): what it offers, to whom, and where its menus come from.
//!
//! Five things are gonk's to get wrong on this page, and each is a test here:
//!
//! - [`an_anonymous_loopback_caller_can_neither_read_nor_decide_a_finding`] — ⚠ **the
//!   measurement**, not an assumption. Brian's rule is *"nothing gets published to Gonk
//!   except by the human"*, and an earlier arc measured that an anonymous loopback caller
//!   gets `can-write = true` on the LEDGER. This asks the kernel what that same caller can do
//!   to a FINDING, in both directions.
//! - [`the_queue_page_offers_no_decision_to_a_caller_that_could_not_make_one`] — an offer you
//!   refuse is a worse UI than no offer, and it teaches people to ignore refusals.
//! - [`the_state_nav_is_the_findings_contracts_own_one_of`] — the words on the page ARE the
//!   words in the contract, asked of the contract rather than compared to a list.
//! - [`no_severity_word_is_written_down_in_this_crate`] — the anti-drift assertion with teeth:
//!   the severity set has three copies in `ikigai-browse` served from ONE constant, and a
//!   fourth here would be the one that goes stale silently.
//! - [`the_intray_depth_tells_absent_empty_and_unreadable_apart`] — #446's shape on the one
//!   page whose job is to show process state.
//! - [`the_queue_asks_only_about_the_serious_and_lists_the_rest_on_request`] — ledger #496:
//!   minting keeps every severity, triage asks about the serious set, and the rows left out
//!   are counted and named rather than silently absent. With
//!   [`an_unrated_finding_is_never_hidden`], [`the_badge_carries_the_serious_count_and_the_other_count`]
//!   and [`a_serious_word_the_contract_does_not_declare_is_refused_at_start`].
//!
//! Every store here is `in_memory_shared_declaring`, so these run beside a live gonk holding
//! `~/.ikigai/store`.

use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Representation, Request, Verb};
use ikigai_gonk::config::QueuePolicy;
use ikigai_gonk::grants::{browse_graph_grants, grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::queue;
use ikigai_gonk::trigger::{Depth, Trigger};
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_store::DurableStore;
use tempfile::TempDir;

/// The root every fixture here configures.
const ROOT: &str = "demo";

/// A scratch repository: one file, no git.
fn scratch_root() -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir(dir.path().join("src")).expect("src/");
    std::fs::write(dir.path().join("src/lib.rs"), "the first version\n").expect("lib.rs");
    dir
}

fn roots(dir: &TempDir) -> Vec<(String, PathBuf)> {
    vec![(ROOT.to_string(), dir.path().to_path_buf())]
}

/// The HTTP door's kernel over the composition `main` builds with a browse root — gonk's
/// pages in front of a hub carrying the browse family over one declared dataset.
///
/// No watcher: `ikigai-browse`'s filesystem reads are live and uncacheable exactly as it
/// declares them, and nothing here reads a file twice. The `review` argument is the trigger
/// the Queue page reports the depth of.
///
/// ⚠ A configured trigger composes the trigger's SPACES too, because the page reads the
/// depth through `urn:iki:gonk:review:depth` rather than around it (ledger #466). A fixture
/// that left them out would exercise only the "could not be read" arm, which is exactly the
/// arm that must never be reached in ordinary use.
fn door(dir: &TempDir, review: Option<Trigger>) -> (Kernel, TempDir) {
    door_watching(
        dir,
        review,
        Arc::new(ikigai_gonk::trigger::Activity::default()),
        false,
    )
}

/// The same door, with the pass counters held by the CALLER and the armed flag its own
/// argument — so a test can put a pass in flight, end it, and ask the badge what it says
/// about each, which is the whole of ledger #469's first half.
fn door_watching(
    dir: &TempDir,
    review: Option<Trigger>,
    activity: Arc<ikigai_gonk::trigger::Activity>,
    armed: bool,
) -> (Kernel, TempDir) {
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store that declares where its sharer writes");
    let wired = browse::wire(roots(dir), handle, &[], None, &graph);
    let trigger_spaces = match &review {
        Some(t) => ikigai_gonk::trigger::space(t, activity, armed, QueuePolicy::default()),
        None => Vec::new(),
    };
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        trigger_spaces,
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
        // ★ The DEFAULT policy, as `main` runs it with no `gonk.queue.serious` line — so the
        // gate every test here sees is the one the shipped binary applies.
        queue: QueuePolicy::default(),
    });
    (doors::http_kernel(hub, web::space(face)), config)
}

/// ★ The grant this server hands an ANONYMOUS loopback caller, computed the way
/// `main` computes it — the configured ledgers at write authority, and nothing else.
fn anonymous() -> Capability {
    Capability::scoped(
        grants_for_all(&["default".to_string()], Authority::Write).expect("the ledger's tokens"),
    )
}

/// A grant that can read every root and publish: what `grants.json`'s `brian` identity holds.
fn reviewer() -> Capability {
    let mut scopes = vec![
        ikigai_browse::CAP_WILDCARD.to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ];
    scopes.extend(grants_for_all(&["default".to_string()], Authority::Write).expect("tokens"));
    scopes.extend(browse_graph_grants(Authority::Write).expect("a named browse graph"));
    Capability::scoped(scopes)
}

/// The same, WITHOUT the publishing token: a caller who may look and not decide.
fn onlooker() -> Capability {
    let mut scopes = vec![ikigai_browse::CAP_WILDCARD.to_string()];
    scopes.extend(grants_for_all(&["default".to_string()], Authority::Write).expect("tokens"));
    scopes.extend(browse_graph_grants(Authority::Read).expect("a named browse graph"));
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

/// One page, as HTML.
fn page(kernel: &Kernel, args: &[(&str, &str)], cap: &Capability) -> String {
    let answer = issue(kernel, Verb::Source, queue::QUEUE_IRI, args, cap)
        .unwrap_or_else(|e| panic!("the queue page: {e}"));
    String::from_utf8(answer.bytes).expect("utf-8")
}

// ------------------------------------------------------------------ the measurement

/// ⚠⚠ **The brief's question, asked of the kernel rather than reasoned about.**
///
/// `~/.config/ikigai/gonk/grants.json` holds only the `brian` identity, which carries
/// `urn:cap:annotate` — so if an anonymous caller could publish, the authority would be
/// coming from somewhere other than that file, and that would be a breach of the standing
/// rule rather than a UI question. It cannot: the anonymous grant is four ledger tokens per
/// configured ledger, `urn:repo:{repo}:findings` requires `urn:cap:browse:read:*`, and the
/// finding Sink requires that PLUS `urn:cap:annotate`.
///
/// ★ Both halves matter and the first is the one that is easy to skip: a caller who cannot
/// even READ the queue cannot be shown a row to decide about, so the page's refusal is not
/// the only thing standing between an anonymous browser and a published finding.
#[test]
fn an_anonymous_loopback_caller_can_neither_read_nor_decide_a_finding() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let anonymous = anonymous();

    let read = issue(
        &door,
        Verb::Source,
        &format!("urn:repo:{ROOT}:findings"),
        &[("as", "application/json")],
        &anonymous,
    );
    assert!(
        matches!(read, Err(ikigai_core::Error::Denied(_))),
        "an anonymous loopback caller must not READ the queue: {read:?}"
    );

    // The id is a position hash; this one names no finding, and that is deliberate — a
    // capability refusal must arrive BEFORE existence is consulted, or the refusal leaks
    // whether a finding is there.
    let decide = issue(
        &door,
        Verb::Sink,
        "urn:iki:finding:0123456789abcdef01234567",
        &[("decision", "publish"), ("severity", "minor")],
        &anonymous,
    );
    assert!(
        matches!(decide, Err(ikigai_core::Error::Denied(_))),
        "an anonymous loopback caller must not PUBLISH: {decide:?}"
    );

    // …and not through gonk's own form adapter either, which declares the same token so the
    // kernel refuses before dispatch rather than one hop in.
    let form = issue(
        &door,
        Verb::Sink,
        queue::DECIDE_IRI,
        &[(
            "content",
            "id=0123456789abcdef01234567&decision=publish&severity=minor",
        )],
        &anonymous,
    );
    assert!(
        matches!(form, Err(ikigai_core::Error::Denied(_))),
        "the queue's form adapter must refuse an anonymous caller: {form:?}"
    );
}

// ---------------------------------------------------------------------- the offer

/// An offer you refuse is a worse UI than no offer: no Queue link, no decision form, and a
/// sentence saying which token is missing.
#[test]
fn the_queue_page_offers_no_decision_to_a_caller_that_could_not_make_one() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);

    let anonymous = page(&door, &[], &anonymous());
    assert!(
        !anonymous.contains(">Queue</a>"),
        "the header must not offer a Queue link to a caller who cannot decide:\n{anonymous}"
    );
    assert!(
        !anonymous.contains("class='decide'"),
        "no decision form for a caller who cannot submit one:\n{anonymous}"
    );
    assert!(
        anonymous.contains(ikigai_browse::CAP_PREFIX),
        "the refusal must name the token that is missing:\n{anonymous}"
    );

    // A caller who may READ every root and not publish: the rows are legible, the form is
    // not drawn, and the page says which of the two authorities it is short of.
    let onlooker = page(&door, &[], &onlooker());
    assert!(
        !onlooker.contains(">Queue</a>"),
        "ledger #444 gates the LINK on being able to decide:\n{onlooker}"
    );
    assert!(
        onlooker.contains(ikigai_browse::CAP_ANNOTATE),
        "a read-only posture must name `{}`:\n{onlooker}",
        ikigai_browse::CAP_ANNOTATE
    );

    let reviewer = page(&door, &[], &reviewer());
    assert!(
        // ⚠ `>Queue<`, not `>Queue</a>`: the link now carries the live depth badge
        // inside it (ledger #466), so the close tag is no longer adjacent to the label.
        reviewer.contains(">Queue<"),
        "a caller holding both authorities is offered the page:\n{reviewer}"
    );
    assert!(
        !reviewer.contains(ikigai_browse::CAP_ANNOTATE),
        "…and is not told about a token it holds:\n{reviewer}"
    );
}

/// ⚠ **"No pending findings" and "this page failed" must not look alike** (ledger #446).
/// An empty queue says so in as many words, and says it about a named scope.
#[test]
fn an_empty_queue_says_so_affirmatively() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let rendered = page(&door, &[], &reviewer());
    assert!(
        rendered.contains("No pending findings in") && rendered.contains("Nothing is waiting"),
        "an empty queue must say so:\n{rendered}"
    );
    // ★ And the sentence that stops the page reading like a gate. The phrase "queued for
    // review" reads like one to anyone who has used one, and this pipeline blocks nothing.
    assert!(
        rendered.contains("A queue, not a gate"),
        "the page must say it blocks nothing:\n{rendered}"
    );
}

// ------------------------------------------------------------------- the contract

/// ★★ The state nav is the findings resource's OWN `one_of`, asked of the contract.
#[test]
fn the_state_nav_is_the_findings_contracts_own_one_of() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let hub = Iri::parse(format!("urn:repo:{ROOT}:findings")).expect("an IRI");
    let declared: Vec<String> = door
        .describe(&hub)
        .expect("the findings resource describes itself")
        .action_specs()
        .into_iter()
        .find(|spec| spec.verb == Verb::Source)
        .expect("a Source action")
        .inputs
        .iter()
        .find(|input| input.name == "state")
        .expect("a `state` input")
        .one_of
        .clone();
    assert!(
        !declared.is_empty(),
        "the findings contract must declare its states as a closed set"
    );

    let rendered = page(&door, &[], &reviewer());
    for state in &declared {
        assert!(
            rendered.contains(&format!(">{state}</a>")),
            "the state nav must carry `{state}`, which the contract declares:\n{rendered}"
        );
    }

    // …and a state the contract does NOT declare is refused, naming the set.
    let refused = issue(
        &door,
        Verb::Source,
        queue::QUEUE_IRI,
        &[("state", "whenever")],
        &reviewer(),
    );
    match refused {
        Err(ikigai_core::Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "state");
            for state in &declared {
                assert!(detail.contains(state.as_str()), "{detail}");
            }
        }
        other => panic!("an undeclared state must be refused naming the set: {other:?}"),
    }
}

/// ★★ **The anti-drift assertion, with the contract as its oracle.**
///
/// `ikigai-browse` serves ONE constant to the Sink's `one_of`, to the words the review prompt
/// hands the model, and to its own menu, so those three cannot disagree. A fourth copy here
/// would be the one that goes stale in silence: the page would go on offering a word the
/// resource had stopped accepting, and the refusal would arrive at the click.
///
/// So: no severity word the contract declares appears as a token in this crate's page code or
/// its stylesheet. ⚠ `web/gonk.css` is deliberately exempt and it is worth saying why — a
/// selector like `.badge.sev.critical` is a COLOUR, and a severity the sheet has no rule for
/// still renders with its own word in its own badge. A missing rule is a missing emphasis; a
/// missing menu entry is an option a human cannot choose.
#[test]
fn no_severity_word_is_written_down_in_this_crate() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let declared = queue::one_of(
        &door,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        "severity",
    )
    .expect("the finding Sink declares its severity set");
    assert!(
        declared.len() > 1,
        "a closed set of more than one: {declared:?}"
    );

    for (what, source) in [
        ("src/queue.rs", include_str!("../src/queue.rs")),
        ("src/batch.rs", include_str!("../src/batch.rs")),
        ("web/gonk.xsl", include_str!("../web/gonk.xsl")),
    ] {
        for word in &declared {
            assert!(
                !contains_token(source, word),
                "`{what}` names the severity `{word}`. The set lives in ONE place \
                 (`ikigai-browse`'s own constant, read through the contract); a copy here is \
                 the one that goes stale silently."
            );
        }
    }
}

/// Whether `text` contains `word` as a whole alphanumeric token — so `informative` does not
/// count as a use of `info`.
fn contains_token(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token == word)
}

// ------------------------------------------------------------------ the decline reason

/// The finding contract's decline reason words (browse 0.10.0), in contract order — the
/// ONLY place these tests get them, so no word is spelled in this file either.
fn reason_words(door: &Kernel) -> Vec<String> {
    queue::one_of(
        door,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        queue::REASON_ARG,
    )
    .expect("the finding Sink declares its reason set")
}

/// ★ The severity guard's sibling, for the five decline reason words: the set lives in
/// `ikigai-browse`'s own constant and reaches the page through the contract, so a copy here
/// is the one that goes stale silently.
///
/// ⚠ **Two differences from the severity guard, both deliberate.**
/// - The words carry HYPHENS, and [`contains_token`] splits on them — it would read a
///   hyphenated word as two harmless halves and never find it. So a token here is a run of
///   alphanumerics AND hyphens ([`contains_word`]).
/// - The stylesheet is checked over its REVIEW-QUEUE SECTION only. The ledger item's "Close
///   as" menu, further down the same file, is a different closed set — `ikigai-ledger`'s
///   close reasons — and one of its words is also a decline reason. That menu is a
///   hard-coded copy of ANOTHER crate's contract (a drift of its own, reported rather than
///   fixed here); counting it against this set would make the guard fail for a word this
///   arc never wrote.
#[test]
fn no_reason_word_is_written_down_in_this_crate() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let declared = reason_words(&door);
    assert!(
        declared.len() > 1,
        "a closed set of more than one: {declared:?}"
    );

    let xsl = include_str!("../web/gonk.xsl");
    let start = xsl
        .find("the review queue -->")
        .expect("the stylesheet's review-queue section marker");
    let end = start
        + xsl[start..]
            .find("a ledger -->")
            .expect("the section after the queue");
    for (what, source) in [
        ("src/queue.rs", include_str!("../src/queue.rs")),
        ("src/batch.rs", include_str!("../src/batch.rs")),
        ("web/gonk.xsl (the review queue)", &xsl[start..end]),
        ("web/gonk.js", include_str!("../web/gonk.js")),
    ] {
        for word in &declared {
            assert!(
                !contains_word(source, word),
                "`{what}` names the decline reason `{word}`. The set lives in ONE place \
                 (`ikigai-browse`'s own constant, read through the contract)."
            );
        }
    }
}

/// Whether `text` contains `word` as a whole token of alphanumerics and hyphens.
fn contains_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .any(|token| token == word)
}

/// The `<select … name='reason'>` of a rendered page, whole. ⚠ The serializer writes
/// attributes in its own order, not the stylesheet's, so the name is found first and the
/// element's start is walked back to.
fn reason_picker(html: &str) -> &str {
    let named = html
        .find(" name='reason'")
        .unwrap_or_else(|| panic!("no reason picker on the page:\n{html}"));
    let start = html[..named]
        .rfind("<select")
        .expect("the picker is a select");
    let end = start + html[start..].find("</select>").expect("the picker closes");
    &html[start..end]
}

/// The `value`s of a picker's options, in document order.
fn option_values(picker: &str) -> Vec<String> {
    picker
        .split("<option")
        .skip(1)
        .map(|option| {
            let at = option.find("value='").expect("an option carries a value") + 7;
            option[at..at + option[at..].find('\'').expect("the value closes")].to_string()
        })
        .collect()
}

/// A finding's own row, as the findings resource's JSON face answers it in `state`.
fn row(door: &Kernel, state: &str, id: &str) -> serde_json::Value {
    let answer = issue(
        door,
        Verb::Source,
        &format!("urn:repo:{ROOT}:findings"),
        &[("as", "application/json"), ("state", state)],
        &reviewer(),
    )
    .expect("the findings read");
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&answer.bytes).expect("json rows");
    rows.into_iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("no `{id}` among the {state} rows"))
}

/// One decision through gonk's own form adapter, answered with the re-rendered section.
fn decide_by_form(door: &Kernel, body: &str) -> String {
    let answer = issue(
        door,
        Verb::Sink,
        queue::DECIDE_IRI,
        &[("content", body)],
        &reviewer(),
    )
    .expect("the adapter renders rather than returning a status");
    String::from_utf8(answer.bytes).expect("utf-8")
}

/// ★★ **The picker IS the contract's reason menu**: an empty "no reason" option, then every
/// declared word in contract order, each carrying its meaning from the input's summary —
/// grouped with the Decline button and with nothing else.
#[test]
fn the_decline_picker_offers_the_contracts_reasons_in_order() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let reviewer = reviewer();
    plant_pending_finding(&door, &reviewer);
    let declared = reason_words(&door);

    let html = page(&door, &[], &reviewer);
    let picker = reason_picker(&html);
    let mut expected = vec![String::new()];
    expected.extend(declared.iter().cloned());
    assert_eq!(
        option_values(picker),
        expected,
        "the picker is the empty option then the contract's words, in order:\n{picker}"
    );
    assert!(
        picker.contains(">why? (optional)</option>"),
        "the empty option reads as no reason:\n{picker}"
    );
    // Each word's `title` is its meaning, read from the contract's own summary.
    let summary = queue::one_of_with_meanings(
        &door,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        queue::REASON_ARG,
    )
    .expect("the reason set, with meanings");
    for (word, meaning) in &summary {
        let meaning = meaning
            .as_deref()
            .unwrap_or_else(|| panic!("browse 0.10.0 defines `{word}` in its summary"));
        assert!(
            !meaning.contains("; ") && !meaning.is_empty(),
            "one definition per word, not the rest of the list: `{word}` = {meaning:?}"
        );
        let option = picker
            .split("<option")
            .find(|o| o.contains(&format!("value='{word}'")))
            .expect("the word's option");
        assert!(
            option.contains("title="),
            "`{word}` carries its meaning as a title:\n{option}"
        );
    }

    // Visibly Decline's: the picker sits inside the group that holds the Decline button,
    // and the Publish button is outside it.
    let group_at = html.find("class='decide-why'").expect("the decline group");
    let group = &html[group_at..group_at + html[group_at..].find("</span>").expect("closes")];
    assert!(
        group.contains("value='decline'") && group.contains("name='reason'"),
        "the picker is grouped with the Decline button:\n{group}"
    );
    assert!(
        !group.contains("value='publish'"),
        "…and not with Publish:\n{group}"
    );
    // The free-text note stays, on both outcomes.
    assert!(
        html.contains("name='content'") && html.contains("kept on both outcomes"),
        "the note is kept beside the word:\n{html}"
    );
}

/// A decline with a word: browse stores it, the row reads it back, and the decision line
/// renders it.
#[test]
fn a_decline_with_a_reason_word_reads_back_and_renders() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    plant_pending_finding(&door, &reviewer());
    let word = reason_words(&door)[1].clone();

    let answer = decide_by_form(
        &door,
        &format!("id={FINDING}&decision=decline&severity={SEVERITY}&reason={word}&content=a+note"),
    );
    assert!(
        !answer.contains("flash error"),
        "a declined word is accepted:\n{answer}"
    );
    let declined = row(&door, "declined", FINDING);
    assert_eq!(
        declined["decision"]["reason"], word,
        "the word is read back: {declined}"
    );
    assert_eq!(declined["decision"]["note"], "a note", "{declined}");

    let html = page(&door, &[("state", "declined")], &reviewer());
    assert!(
        html.contains(&format!(
            "(<span class='reason-word'>{word}</span>) as {SEVERITY}"
        )),
        "the decision line carries the word:\n{html}"
    );
}

/// ★ **The path a person will actually hit**: a word picked, then Publish pressed. The
/// adapter drops the word — browse would refuse it beside a publish — and the finding
/// publishes with no reason on file.
#[test]
fn a_word_picked_then_publish_still_publishes_without_it() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    plant_pending_finding(&door, &reviewer());
    let word = reason_words(&door)[0].clone();

    let answer = decide_by_form(
        &door,
        &format!("id={FINDING}&decision=publish&severity={SEVERITY}&reason={word}"),
    );
    assert!(
        !answer.contains("flash error"),
        "a stray pick must not turn Publish into a refusal:\n{answer}"
    );
    let published = row(&door, "published", FINDING);
    assert_eq!(published["decision"]["outcome"], "published", "{published}");
    assert!(
        published["decision"]["reason"].is_null(),
        "no reason travels with a publish: {published}"
    );

    // ⚠ And the drop is the ADAPTER's: the same word straight at the finding is refused, so
    // this test would catch the adapter forwarding it.
    plant_finding(
        &door,
        &reviewer(),
        "eeeeeeeeeeeeeeeeeeeeeeee",
        Some(SEVERITY),
        "another claim",
    );
    let direct = issue(
        &door,
        Verb::Sink,
        "urn:iki:finding:eeeeeeeeeeeeeeeeeeeeeeee",
        &[
            ("decision", "publish"),
            ("severity", SEVERITY),
            (queue::REASON_ARG, &word),
        ],
        &reviewer(),
    );
    assert!(
        matches!(&direct, Err(ikigai_core::Error::InvalidArgument { name, .. }) if name == queue::REASON_ARG),
        "browse refuses a word beside a publish: {direct:?}"
    );
}

/// The "no reason" option: an empty `reason=` declines with nothing on file.
#[test]
fn a_decline_with_the_empty_option_has_no_reason() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    plant_pending_finding(&door, &reviewer());

    let answer = decide_by_form(
        &door,
        &format!("id={FINDING}&decision=decline&severity={SEVERITY}&reason="),
    );
    assert!(!answer.contains("flash error"), "{answer}");
    let declined = row(&door, "declined", FINDING);
    assert_eq!(declined["decision"]["outcome"], "declined", "{declined}");
    assert!(
        declined["decision"]["reason"].is_null(),
        "no word, no reason: {declined}"
    );
    let html = page(&door, &[("state", "declined")], &reviewer());
    assert!(
        !html.contains("reason-word"),
        "a decline with no word renders none:\n{html}"
    );
}

/// A like claim to a twin declined WITH a word: the mark on the fresh row names the word.
#[test]
fn the_prior_decision_mark_names_the_twins_reason() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let declared = queue::check_serious(&door, &QueuePolicy::default()).expect("words");
    let severity = declared[0].as_str();
    let word = reason_words(&door).last().expect("a reason").clone();
    let twin = "ffffffffffffffffffffffff";
    let fresh = "abababababababababababab";
    plant_finding(&door, &reviewer(), twin, Some(severity), "raised once");
    issue(
        &door,
        Verb::Sink,
        &format!("urn:iki:finding:{twin}"),
        &[
            ("decision", "decline"),
            ("severity", severity),
            (queue::REASON_ARG, &word),
        ],
        &reviewer(),
    )
    .expect("a human declines the twin, with a word");
    plant_finding(&door, &reviewer(), fresh, Some(severity), "raised again");
    let graph = browse::Graph::chosen()
        .named()
        .expect("a named browse graph")
        .as_str()
        .to_string();
    let link = format!(
        "PREFIX prov: <http://www.w3.org/ns/prov#>\nINSERT DATA {{ GRAPH <{graph}> {{ \
         <urn:iki:finding:{fresh}> prov:wasInfluencedBy <urn:iki:finding:{twin}:decision> . }} }}"
    );
    issue(
        &door,
        Verb::Sink,
        "urn:iki:store:graph-update",
        &[("graph", &graph), ("content", &link)],
        &reviewer(),
    )
    .expect("the link browse mints at mint time");

    let html = page(&door, &[("state", "pending")], &reviewer());
    assert!(
        html.contains(&format!(
            "a like claim on this line was declined (<span class='reason-word'>{word}</span>) as {severity}"
        )),
        "the mark names the twin's reason:\n{html}"
    );
}

// --------------------------------------------------------------------- the intray

/// ⚠ **Three answers, and two of them are not a number.** "No queue configured", "the queue
/// is empty" and "the queue could not be read" are different statements; rendering `0` for
/// all three is ledger #446's shape on the one page whose job is to show process state.
#[test]
fn the_intray_depth_tells_absent_empty_and_unreadable_apart() {
    assert_eq!(ikigai_gonk::trigger::depth(None), Depth::NotConfigured);

    let spaces = tempfile::tempdir().expect("a spaces tree");
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };

    // Configured and never prepared: the inbox is not there, and that is NOT zero.
    assert!(
        matches!(
            ikigai_gonk::trigger::depth(Some(&trigger)),
            Depth::Unreadable(_)
        ),
        "a missing inbox is unreadable, not empty"
    );

    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    assert_eq!(
        ikigai_gonk::trigger::depth(Some(&trigger)),
        Depth::Counted {
            inbox: 0,
            outbox: 0,
            error: 0
        }
    );

    for n in 0..3 {
        std::fs::write(trigger.inbox().join(format!("{n}.tuple")), "x").expect("a tuple");
    }
    // A file that is not a tuple is not a queued request.
    std::fs::write(trigger.inbox().join("notes.txt"), "x").expect("a stray file");
    assert_eq!(
        ikigai_gonk::trigger::depth(Some(&trigger)),
        Depth::Counted {
            inbox: 3,
            outbox: 0,
            error: 0
        }
    );
}

/// The depth reaches the page as a SENTENCE, in all three shapes.
#[test]
fn the_queue_page_says_what_is_waiting() {
    let dir = scratch_root();

    let (no_queue, _c1) = door(&dir, None);
    let rendered = page(&no_queue, &[], &reviewer());
    assert!(
        rendered.contains("No review queue is configured"),
        "{rendered}"
    );

    let spaces = tempfile::tempdir().expect("a spaces tree");
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };
    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    let (empty, _c2) = door(&dir, Some(trigger.clone()));
    let rendered = page(&empty, &[], &reviewer());
    assert!(rendered.contains("The review queue is empty"), "{rendered}");

    for n in 0..39 {
        std::fs::write(trigger.inbox().join(format!("{n}.tuple")), "x").expect("a tuple");
    }
    let (deep, _c3) = door(&dir, Some(trigger));
    let rendered = page(&deep, &[], &reviewer());
    assert!(
        rendered.contains("39 review requests are waiting"),
        "★ \"nothing has happened yet\" and \"39 still queued\" must not render \
         identically:\n{rendered}"
    );
}

// ------------------------------------------------------------- a real finding

/// The id is a POSITION in `ikigai-browse` (`sha256(pass ‖ start ‖ exact)`, 24 hex); here it
/// is just 24 hex, because nothing downstream re-derives it.
const FINDING: &str = "aaaabbbbccccddddeeee0001";

/// ★ **Plant one pending finding, through the store's own narrow door.**
///
/// A finding is normally minted by a review pass, which needs a model — so a test that
/// insisted on the real producer could not run in CI, and would be testing the model. What it
/// would test here anyway is gonk's rendering of a shape `ikigai-browse` documents: the quads
/// below are that module's own `store_annotation`, for `Family::Finding`.
///
/// ⚠ **It goes in through `urn:iki:store:graph-update` under `urn:cap:store:write:graph:`**,
/// which is a real authority an operator can mint (`passkey invite … --browse-graph write`)
/// and which the README already describes as "lets an identity mint or edit those quads
/// directly". So this is a fixture written the way a holder of that grant would write it, not
/// a back door around one.
fn plant_pending_finding(door: &Kernel, cap: &Capability) {
    plant_finding(
        door,
        cap,
        FINDING,
        Some(SEVERITY),
        "The bound truncates instead of refusing.",
    );
}

/// The same, for any id, any body, and any severity — or none, which is a finding the model
/// left unrated (no `sh:resultSeverity` at all, the shape browse reads back as `null`).
fn plant_finding(door: &Kernel, cap: &Capability, id: &str, severity: Option<&str>, body: &str) {
    let graph = browse::Graph::chosen()
        .named()
        .expect("this server writes browse's quads in a NAMED graph")
        .as_str()
        .to_string();
    let rated = match severity {
        Some(word) => format!("    sh:resultSeverity <urn:iki:severity:{word}> ;\n"),
        None => String::new(),
    };
    let update = format!(
        r#"PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX oa: <http://www.w3.org/ns/oa#>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX sh: <http://www.w3.org/ns/shacl#>
PREFIX ik: <https://ikigai-rs.dev/ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
INSERT DATA {{ GRAPH <{graph}> {{
  <urn:iki:finding:{id}> a prov:Entity ;
    dcterms:description "{body}" ;
    dcterms:creator "a-test-reviewer" ;
    dcterms:created "2026-09-19T12:00:00Z"^^xsd:dateTime ;
    prov:wasGeneratedBy <urn:ikigai:browse:review:demo:src/lib.rs> ;
{rated}    ik:annotates <urn:repo:{ROOT}:file:src/lib.rs> ;
    ik:repo "{ROOT}" ;
    ik:path "src/lib.rs" ;
    ik:contentHash "sha256:planted" ;
    oa:hasSelector <urn:iki:finding:{id}:selector:quote> ,
                   <urn:iki:finding:{id}:selector:position> .
  <urn:iki:finding:{id}:selector:quote> a oa:TextQuoteSelector ;
    oa:exact "first version" .
  <urn:iki:finding:{id}:selector:position> a oa:TextPositionSelector ;
    oa:start "4"^^xsd:nonNegativeInteger ;
    oa:end "17"^^xsd:nonNegativeInteger .
}} }}"#
    );
    issue(
        door,
        Verb::Sink,
        "urn:iki:store:graph-update",
        &[("graph", &graph), ("content", &update)],
        cap,
    )
    .expect("the browse graph's write token plants the finding");
}

/// ★ Ledger #496's other half, through this server's own adapter: a file page asked for as
/// HTML carries `proposals=` = the contract's severity set less the serious words, so a
/// pending finding BELOW the gate is drawn beside its line as a proposal and one AT the gate
/// is not — it is the queue's. Both findings are planted; neither is published; the words are
/// read from the contract, never spelled here.
#[test]
fn the_file_page_through_the_adapter_draws_the_other_severities_as_proposals() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let policy = QueuePolicy::default();
    let declared = queue::check_serious(&door, &policy).expect("the contract's words");
    let others = queue::other_severities(&declared, &policy);
    let (serious, other) = (policy.serious[0].as_str(), others[0].as_str());
    plant_finding(
        &door,
        &reviewer(),
        "aaaaaaaaaaaaaaaaaaaaaaaa",
        Some(serious),
        "for the queue",
    );
    plant_finding(
        &door,
        &reviewer(),
        "bbbbbbbbbbbbbbbbbbbbbbbb",
        Some(other),
        "for the page",
    );

    let read = |command: &str| {
        let answer = issue(
            &door,
            Verb::Source,
            ikigai_gonk::k::K_IRI,
            &[("c", command)],
            &onlooker(),
        )
        .unwrap_or_else(|e| panic!("`{command}`: {e}"));
        String::from_utf8(answer.bytes).expect("utf-8")
    };

    // The HTML face: the other severity is a proposal, the serious one is not on this page.
    let html = read("source urn:repo:demo:file:src/lib.rs as=text/html");
    assert!(
        html.contains("browse-proposals"),
        "no proposals panel: {html}"
    );
    assert!(
        html.contains(&format!("browse-proposal-{other}")),
        "{other} not drawn: {html}"
    );
    assert!(
        html.contains("proposal-bbbbbbbbbbbbbbbbbbbbbbbb"),
        "the planted {other} finding is missing"
    );
    assert!(
        !html.contains("proposal-aaaaaaaaaaaaaaaaaaaaaaaa"),
        "a {serious} finding was drawn as a proposal — that is the queue's"
    );
    assert!(
        !html.contains("browse-annotation-marker-proposal") || !html.contains("decision="),
        "no decision form on the file page"
    );

    // The caller's own choice wins over gonk's complement.
    let chosen = read(&format!(
        "source urn:repo:demo:file:src/lib.rs as=text/html proposals={serious}"
    ));
    assert!(
        chosen.contains("proposal-aaaaaaaaaaaaaaaaaaaaaaaa"),
        "an explicit proposals= was overridden"
    );
    assert!(!chosen.contains("proposal-bbbbbbbbbbbbbbbbbbbbbbbb"));

    // The default face is untouched: no argument is added to anything but an HTML file page.
    let plain = read("source urn:repo:demo:file:src/lib.rs");
    assert!(
        !plain.contains("proposals ("),
        "the text face grew a proposals section unasked: {plain}"
    );
}

/// ★ Ledger #475 on the page: a pending finding that browse 0.9.0 minted as a like claim to
/// one a human already declined renders the prior decision on its row — outcome, rating,
/// date, and the twin — beside the same form. The twin's decision is made through the real
/// Sink so its node has the shape browse reads back; the link is the triple browse mints.
#[test]
fn a_like_claim_to_a_declined_finding_arrives_marked_on_the_queue_row() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let declared = queue::check_serious(&door, &QueuePolicy::default()).expect("words");
    let word = declared[0].as_str();
    let twin = "cccccccccccccccccccccccc";
    let fresh = "dddddddddddddddddddddddd";
    plant_finding(
        &door,
        &reviewer(),
        twin,
        Some(word),
        "the first time this was raised",
    );
    issue(
        &door,
        Verb::Sink,
        &format!("urn:iki:finding:{twin}"),
        &[("decision", "decline"), ("severity", word)],
        &reviewer(),
    )
    .expect("a human declines the twin");
    plant_finding(
        &door,
        &reviewer(),
        fresh,
        Some(word),
        "the same claim, raised again",
    );
    let graph = browse::Graph::chosen()
        .named()
        .expect("a named browse graph")
        .as_str()
        .to_string();
    let link = format!(
        "PREFIX prov: <http://www.w3.org/ns/prov#>\nINSERT DATA {{ GRAPH <{graph}> {{ \
         <urn:iki:finding:{fresh}> prov:wasInfluencedBy <urn:iki:finding:{twin}:decision> . }} }}"
    );
    issue(
        &door,
        Verb::Sink,
        "urn:iki:store:graph-update",
        &[("graph", &graph), ("content", &link)],
        &reviewer(),
    )
    .expect("the link browse mints at mint time");

    let html = page(&door, &[("state", "pending")], &reviewer());
    assert!(
        html.contains("a like claim on this line was declined"),
        "the prior decision is not on the row: {html}"
    );
    assert!(html.contains(twin), "the twin is not named: {html}");
    assert!(
        html.contains("prior-decision") && html.contains("class='decide'"),
        "the mark must sit BESIDE the form, not replace it: {html}"
    );
    let declined = page(&door, &[("state", "declined")], &reviewer());
    assert!(
        !declined.contains("a like claim on this line"),
        "the twin itself carries no prior: {declined}"
    );
}

/// The severity this fixture proposes. ⚠ Not a member of a list held here: it is read back
/// out of the contract by [`a_planted_finding_renders_the_contracts_menu_and_publishes`]
/// before it is used, so this constant cannot disagree with the closed set.
const SEVERITY: &str = "major";

/// ★★ **The whole act, end to end: a pending finding, the menu the contract declares, the
/// human's decision, and the record it leaves.**
#[test]
fn a_planted_finding_renders_the_contracts_menu_and_publishes() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let reviewer = reviewer();

    let severities = queue::one_of(
        &door,
        &format!("urn:iki:finding:{FINDING}"),
        Verb::Sink,
        "severity",
    )
    .expect("the finding Sink declares its severity set");
    assert!(
        severities.iter().any(|s| s == SEVERITY),
        "this fixture proposes `{SEVERITY}`, which the contract must know: {severities:?}"
    );

    plant_pending_finding(&door, &reviewer);

    let pending = page(&door, &[], &reviewer);
    assert!(
        pending.contains("The bound truncates instead of refusing."),
        "the finding's body is the row:\n{pending}"
    );
    assert!(
        pending.contains("src/lib.rs") && pending.contains("a-test-reviewer"),
        "a finding reaches the code it is about, and says which pass found it:\n{pending}"
    );
    // ★ THE POINT OF THE ARC: every declared severity is an option, and nothing else is.
    for word in &severities {
        assert!(
            pending.contains(&format!(">{word}</option>")),
            "the menu must offer `{word}`, which the contract declares:\n{pending}"
        );
    }
    assert!(
        pending.contains("Publish to Gonk") && pending.contains("name='severity'"),
        "a caller holding the token is offered the form:\n{pending}"
    );
    // ⚠ And NO human rating is shown while the finding is pending. `effective_severity`
    // falls back to the model's proposal when there is no decision — correct for a consumer
    // wanting one number, and a lie on a badge labelled "human", on the one page whose whole
    // argument is that the two ratings are different things.
    assert!(
        !pending.contains("human:"),
        "a pending finding has no human rating to show:\n{pending}"
    );

    // …and a caller who may read and not decide sees the row and no form.
    let watched = page(&door, &[], &onlooker());
    assert!(
        watched.contains("The bound truncates instead of refusing."),
        "the rows are legible without the publishing token:\n{watched}"
    );
    assert!(
        !watched.contains("Publish to Gonk"),
        "…and the form is not drawn for a caller who could not submit it:\n{watched}"
    );

    // The human decides, through gonk's own form adapter, re-rating as the last severity the
    // contract offers rather than as a word chosen here.
    let human = severities.last().expect("a non-empty set").clone();
    let answer = issue(
        &door,
        Verb::Sink,
        queue::DECIDE_IRI,
        &[(
            "content",
            &format!(
                "id={FINDING}&decision=publish&severity={human}&content=Worth+keeping+\
                 as+a+note"
            ),
        )],
        &reviewer,
    )
    .expect("a holder of urn:cap:annotate publishes");
    let rendered = String::from_utf8(answer.bytes).expect("utf-8");
    assert!(
        rendered.contains("urn:iki:annotation:"),
        "publishing answers with the annotation it minted:\n{rendered}"
    );

    // ⚠ Both ratings survive: the model's proposal on the finding, the human's on the
    // decision. That is the only signal that could ever say whether the reviewer is
    // calibrated, and it is unrecoverable if the first write clobbers the proposal.
    let published = page(&door, &[("state", "published")], &reviewer);
    assert!(
        published.contains(&format!("model: {SEVERITY}")),
        "the model's PROPOSAL is not overwritten:\n{published}"
    );
    assert!(
        published.contains(&format!("human: {human}")),
        "the human's final rating is shown beside it:\n{published}"
    );
    assert!(
        published.contains("Worth keeping as a note"),
        "the reason is kept:\n{published}"
    );
    // ★ The asymmetry, said rather than hidden.
    assert!(
        published.contains("delete urn:iki:annotation:"),
        "undoing a publication is a separate, visible act, and the page says so:\n{published}"
    );

    // The pending queue is now affirmatively empty, and the row moved rather than vanished.
    let pending = page(&door, &[], &reviewer);
    assert!(pending.contains("No pending findings in"), "{pending}");
}

/// ⚠ **A decision is not overwritten, and the refusal is RENDERED rather than swallowed.**
///
/// htmx does not swap a non-2xx response, so returning the refusal as a status would make the
/// page silently ignore the most informative answer the system can give — "here is what is on
/// file". The capability layer has already refused; this is about how a person reads it.
#[test]
fn a_second_different_decision_is_refused_and_the_refusal_is_legible() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let reviewer = reviewer();
    plant_pending_finding(&door, &reviewer);

    let decide = |body: &str| -> String {
        let answer = issue(
            &door,
            Verb::Sink,
            queue::DECIDE_IRI,
            &[("content", body)],
            &reviewer,
        )
        .expect("the adapter renders rather than returning a status");
        String::from_utf8(answer.bytes).expect("utf-8")
    };

    decide(&format!(
        "id={FINDING}&decision=decline&severity={SEVERITY}"
    ));
    // The identical repeat: a double-clicked button is not an error.
    let again = decide(&format!(
        "id={FINDING}&decision=decline&severity={SEVERITY}"
    ));
    assert!(
        !again.contains("flash error"),
        "an identical repeat is a no-op:\n{again}"
    );
    // Anything that would CHANGE the record is refused, naming what is recorded.
    let changed = decide(&format!(
        "id={FINDING}&decision=publish&severity={SEVERITY}"
    ));
    assert!(
        changed.contains("flash error"),
        "a second, different decision must be refused:\n{changed}"
    );
}

/// ★★ **The live badge, rendered — the armed trigger's only liveness signal.**
///
/// Ledger [#466](http://localhost:1060/l/default/item/466). `SpaceReactor::watch()` catches
/// up at startup and then lives on a thread nothing else observes, and gonk runs no log
/// ([#383](http://localhost:1060/l/default/item/383)), so a depth that stops falling is the
/// whole symptom of a dead watcher. The header is where a person is already looking.
///
/// ⚠ The fragment is rendered through the SAME stylesheet as every page, and `xrust` is an
/// XSLT-1.0 subset — a template that silently produced nothing would leave a badge that is
/// permanently blank, which reads exactly like an empty queue. So this asserts the markup,
/// not just the absence of an error.
#[test]
fn the_header_badge_renders_the_depth_and_polls_for_it() {
    let dir = scratch_root();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };
    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    for n in 0..7 {
        std::fs::write(trigger.inbox().join(format!("{n}.tuple")), "x").expect("a tuple");
    }
    let (served, _config) = door(&dir, Some(trigger));

    // The nav link carries the poll, at the interval the Rust side names — never the
    // stylesheet's own number.
    let rendered = page(&served, &[], &reviewer());
    assert!(
        rendered.contains(&format!("hx-get='{}'", ikigai_gonk::queue::BADGE_PATH)),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!(
            "hx-trigger='load, every {}'",
            ikigai_gonk::queue::BADGE_EVERY
        )),
        "{rendered}"
    );

    // …and the fragment it polls.
    let answer = issue(
        &served,
        Verb::Source,
        ikigai_gonk::queue::BADGE_IRI,
        &[],
        &reviewer(),
    )
    .expect("the badge renders");
    let badge = String::from_utf8(answer.bytes).expect("utf-8");
    // ★ Since ledger #496 the NUMBERS are findings waiting for a human (none planted here),
    // and the request depth — the seven tuples — rides in the tooltip's sentence with the
    // rest of the liveness half. The colour still says a backlog exists.
    assert!(
        badge.contains("class='serious'>0<") && badge.contains("class='other'>+0<"),
        "the two numbers a person came to see: {badge}"
    );
    assert!(
        badge.contains("7 review requests are waiting"),
        "the request depth is in the sentence: {badge}"
    );
    // ⚠ Not empty, and not stuck: unarmed, nothing is supposed to be draining, so seven
    // waiting is correct rather than alarming.
    assert!(badge.contains("badge-depth count"), "{badge}");
    assert!(!badge.contains("stuck"), "{badge}");
    // ★ The whole sentence rides as the tooltip, so colour is never the message (WCAG 1.4.1)
    // and the one fact that tells a slow queue from a stuck one is readable.
    assert!(badge.contains("NOTHING IS DRAINING"), "{badge}");

    // ⚠ No queue configured: NOTHING, not a zero. A zero on the badge says "empty queue",
    // which is a different statement (ledger #446).
    let (no_queue, _c) = door(&dir, None);
    let answer = issue(
        &no_queue,
        Verb::Source,
        ikigai_gonk::queue::BADGE_IRI,
        &[],
        &reviewer(),
    )
    .expect("the badge renders without a queue too");
    assert!(
        answer.bytes.is_empty(),
        "an unconfigured queue draws no badge at all: {}",
        String::from_utf8_lossy(&answer.bytes)
    );
}

// ------------------------------------------------- liveness: ledger #469

/// Epoch milliseconds, the way `crate::trigger` counts them.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_millis() as u64
}

/// The badge fragment, rendered.
fn badge(kernel: &Kernel) -> String {
    let answer = issue(kernel, Verb::Source, queue::BADGE_IRI, &[], &reviewer())
        .unwrap_or_else(|e| panic!("the badge: {e}"));
    String::from_utf8(answer.bytes).expect("utf-8")
}

/// A trigger over its own empty scratch tree.
fn armed_trigger(spaces: &TempDir) -> Trigger {
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: true,
    };
    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    trigger
}

/// ⚠ **One cadence, two spellings, and nothing but this test holding them together.**
///
/// htmx wants `"10s"` and an activity window wants milliseconds, so the number is written
/// twice. The window must also be at least the interval — that is the guarantee the whole
/// fix rests on ([#469](http://localhost:1060/l/default/item/469)): if the window were
/// shorter than the gap between two polls, a pass could still finish entirely unseen, which
/// is precisely the failure being fixed.
#[test]
fn the_badge_cadence_has_one_number_in_two_spellings() {
    assert_eq!(
        queue::BADGE_EVERY,
        format!("{}s", queue::BADGE_EVERY_MS / 1000),
        "the htmx interval and its milliseconds are the same number, or the badge polls at \
         one rate and measures activity against another"
    );
    // ⚠ The window-is-at-least-the-interval guarantee is a `const _: () = assert!(…)` in
    // `src/queue.rs`: it is constant, so the compiler is a better place for it than a test.
}

/// ★★ **A pass shorter than the poll interval still reports itself.**
///
/// [#469](http://localhost:1060/l/default/item/469), the first live end-to-end run: a commit
/// dropped a tuple, the pass ran, the queue drained, three findings landed — in about three
/// seconds, between two ten-second polls. The badge read 0, then 0.
///
/// Two defects, both asserted here.
///
/// 1. **A pass in flight against an EMPTY inbox rendered as `empty`**, because `in_flight`
///    was only consulted on the `waiting > 0` arm — and the reactor moves the tuple OUT of
///    the inbox before it starts the work, so the ordinary case is a running pass with an
///    empty queue. The badge said "nothing is happening" during the one interval when
///    something was.
/// 2. **A pass that has ENDED left no trace at all.** It does now, for
///    [`queue::BADGE_RECENT_MS`] — and the window is the poll interval, so no phase of the
///    clock can hide a pass from every poll.
#[test]
fn a_pass_shorter_than_the_poll_interval_still_reports_itself() {
    let dir = scratch_root();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let activity = Arc::new(ikigai_gonk::trigger::Activity::default());
    let (served, _config) = door_watching(
        &dir,
        Some(armed_trigger(&spaces)),
        Arc::clone(&activity),
        true,
    );

    // Idle, and nothing has ever run: a dim zero and no spark. An armed server that has done
    // nothing yet must not claim otherwise.
    let idle = badge(&served);
    assert!(idle.contains("badge-depth empty"), "{idle}");
    assert!(!idle.contains("spark"), "nothing has happened yet: {idle}");

    // A pass, running, with NOTHING in the inbox — the shape the old code called `empty`.
    let pass = activity.begin(now_ms()).expect("the one pass slot");
    let running = badge(&served);
    assert!(
        running.contains("badge-depth working"),
        "a running pass against an empty inbox is WORKING, not empty: {running}"
    );
    assert!(
        running.contains("spark running"),
        "and it is drawn, not only coloured: {running}"
    );
    assert!(
        running.contains("A pass has been running for"),
        "the sentence rides as the tooltip, so the state is readable and not only visible: \
         {running}"
    );

    // …and when it ends, the badge goes on saying so for the window. This is the assertion
    // that would have caught the reported bug: the pass is OVER, the queue is empty, and the
    // page still has something true to say about the last few seconds.
    pass.succeeded();
    let after = badge(&served);
    assert!(
        after.contains("badge-depth recent"),
        "a pass that ended within the window is RECENT, not idle: {after}"
    );
    assert!(after.contains("spark recent"), "{after}");
    assert!(
        after.contains("the last ended"),
        "and the sentence says how long ago: {after}"
    );
}

/// ★ **The revision is what the findings list refreshes on, and it moves only when the queue
/// does.**
///
/// A token that changed every poll would make the list re-fetch every ten seconds, which is
/// the cost the single-cadence design exists to avoid; one that never changed would leave
/// the page exactly as stale as before ([#469](http://localhost:1060/l/default/item/469)).
#[test]
fn the_badge_revision_moves_only_when_the_queue_does() {
    let dir = scratch_root();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let activity = Arc::new(ikigai_gonk::trigger::Activity::default());
    let (served, _config) = door_watching(
        &dir,
        Some(armed_trigger(&spaces)),
        Arc::clone(&activity),
        true,
    );

    let rev = |markup: &str| {
        let at = markup
            .find("data-rev=")
            .expect("the badge carries a revision");
        let rest = &markup[at + "data-rev=".len() + 1..];
        rest[..rest.find('\'').expect("a closed attribute")].to_string()
    };

    let first = rev(&badge(&served));
    assert_eq!(
        first,
        rev(&badge(&served)),
        "two polls with nothing happening between them must agree, or the list re-fetches \
         for nothing every interval"
    );

    activity.begin(now_ms()).expect("the slot").succeeded();
    assert_ne!(
        first,
        rev(&badge(&served)),
        "a completed pass may have minted findings, and that is exactly when the list has \
         something new to show"
    );
}

/// ★★ **The findings list refreshes without a reload — off the badge's poll, not a clock of
/// its own.**
///
/// The second half of [#469](http://localhost:1060/l/default/item/469): `web/gonk.xsl`
/// carried exactly one `hx-trigger` for this surface, the badge's, so the one page in gonk
/// whose content changes with no user action was the one page that never refreshed itself.
///
/// ⚠ The event name is a THIRD spelling of one fact — Rust names it, the stylesheet renders
/// it, the script raises it — so this asserts the script against the constant. A rename that
/// reached two of the three would leave a page that quietly stopped updating, with nothing
/// failing anywhere.
#[test]
fn the_findings_list_listens_for_the_badges_news() {
    let dir = scratch_root();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let (served, _config) = door(&dir, Some(armed_trigger(&spaces)));

    let rendered = page(&served, &[("state", "pending")], &reviewer());
    assert!(
        rendered.contains(&format!("hx-trigger='{}'", queue::NEWS_EVENT)),
        "the queue section listens for the badge's news: {rendered}"
    );
    assert!(
        rendered.contains(&format!("hx-get='{}?state=pending'", queue::ROWS_PATH)),
        "…and re-fetches ITSELF, at the filter the human is looking at: {rendered}"
    );
    // ⚠ No second interval. The cadence is the server's one number, and a page that grew its
    // own `every Ns` would be a clock this crate does not control.
    assert!(
        !rendered.contains("hx-trigger='every"),
        "the list must not poll on a clock of its own: {rendered}"
    );

    // A human who asked to see everything keeps seeing everything across a refresh.
    let all = page(
        &served,
        &[("state", "pending"), ("limit", "all")],
        &reviewer(),
    );
    assert!(
        all.contains(&format!(
            "hx-get='{}?state=pending&amp;limit=all'",
            queue::ROWS_PATH
        )),
        "a refresh keeps the row bound the human chose: {all}"
    );

    // The held-refresh notice exists and starts hidden: the page says a refresh was held
    // rather than silently going stale.
    assert!(
        rendered.contains("id='queue-stale'") && rendered.contains("hidden='hidden'"),
        "{rendered}"
    );

    // The three spellings of the one event.
    let script = include_str!("../web/gonk.js");
    assert!(
        script.contains(&format!("\"{}\"", queue::NEWS_EVENT)),
        "`web/gonk.js` raises `{}` — the constant the stylesheet renders",
        queue::NEWS_EVENT
    );
    assert!(
        script.contains("data-rev"),
        "…and compares the revision the badge carries"
    );
}

// ---------------------------------------------- the serious gate: ledger #496

/// The declared set split by the DEFAULT policy: one word the page asks about and one it
/// only counts — both read off the contract, neither spelled here.
fn a_serious_and_an_other_word(door: &Kernel) -> (String, String) {
    let declared = queue::one_of(
        door,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        "severity",
    )
    .expect("the finding Sink declares its severity set");
    let policy = QueuePolicy::default();
    let serious = declared
        .iter()
        .find(|w| policy.is_serious(w))
        .expect("the shipped default names a declared word")
        .clone();
    let other = declared
        .iter()
        .find(|w| !policy.is_serious(w))
        .expect("the contract declares a word outside the default set")
        .clone();
    (serious, other)
}

/// ★★ **The gate: by default the page asks about the serious rows and SAYS what it left
/// out; `severity=all` lists everything; and a scope the page does not know is refused
/// naming both.**
///
/// Brian, 2026-09-21: *"the preference is to highlight issues that need addressing, so
/// narrowing the squishy stuff is the priority"* — and *"positive signal is still signal"*.
/// Minting keeps every severity; triage asks about the serious ones. Both are asserted:
/// the suggestion rows are absent from the default page AND present under `all`, and the
/// default page counts them rather than pretending they do not exist.
#[test]
fn the_queue_asks_only_about_the_serious_and_lists_the_rest_on_request() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let reviewer = reviewer();
    let (serious, other) = a_serious_and_an_other_word(&door);
    plant_finding(
        &door,
        &reviewer,
        "aaaabbbbccccddddeeee0001",
        Some(&serious),
        "A serious one.",
    );
    plant_finding(
        &door,
        &reviewer,
        "aaaabbbbccccddddeeee0002",
        Some(&other),
        "A suggestion.",
    );
    plant_finding(
        &door,
        &reviewer,
        "aaaabbbbccccddddeeee0003",
        Some(&other),
        "Another suggestion.",
    );

    let narrowed = page(&door, &[], &reviewer);
    assert!(narrowed.contains("A serious one."), "{narrowed}");
    assert!(
        !narrowed.contains("A suggestion.") && !narrowed.contains("Another suggestion."),
        "a row below the serious set is not offered for a decision:\n{narrowed}"
    );
    assert!(
        narrowed.contains("1 serious pending finding in demo"),
        "the count sentence says which scope it counted:\n{narrowed}"
    );
    // ★ The rows left out are SAID: how many, which words, and the link that lists them.
    assert!(
        narrowed.contains("2 other pending findings") && narrowed.contains(&other),
        "the hidden rows are counted and their words named:\n{narrowed}"
    );
    assert!(
        narrowed.contains(&format!(
            "hx-get='{}?state=pending&amp;{}={}'",
            queue::ROWS_PATH,
            queue::SCOPE_ARG,
            queue::SCOPE_ALL
        )) && narrowed.contains("list all severities"),
        "…and the way to them is a link, not a hint:\n{narrowed}"
    );
    // The scope nav spells the configured set, from the configuration and not this file.
    assert!(
        narrowed.contains(&format!(
            "serious: {}",
            QueuePolicy::default().serious.join(", ")
        )),
        "{narrowed}"
    );
    // ⚠ And the default URL carries NO scope: the narrowed page is the plain one, so every
    // existing link and every bookmark lands on the gate rather than around it.
    assert!(
        narrowed.contains(&format!("hx-get='{}?state=pending'", queue::ROWS_PATH)),
        "{narrowed}"
    );

    let all = page(&door, &[(queue::SCOPE_ARG, queue::SCOPE_ALL)], &reviewer);
    assert!(
        all.contains("A serious one.")
            && all.contains("A suggestion.")
            && all.contains("Another suggestion."),
        "`{}={}` lists everything:\n{all}",
        queue::SCOPE_ARG,
        queue::SCOPE_ALL
    );
    assert!(all.contains("3 pending findings in demo"), "{all}");
    assert!(
        !all.contains("other pending findings"),
        "nothing is hidden under `all`, so nothing is said to be:\n{all}"
    );

    // A scope the page does not know — a severity WORD, say, which is the natural mistake —
    // is refused naming the two it does.
    let refused = issue(
        &door,
        Verb::Source,
        queue::QUEUE_IRI,
        &[(queue::SCOPE_ARG, other.as_str())],
        &reviewer,
    );
    match refused {
        Err(ikigai_core::Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, queue::SCOPE_ARG);
            assert!(
                detail.contains(queue::SCOPE_SERIOUS) && detail.contains(queue::SCOPE_ALL),
                "{detail}"
            );
        }
        other => panic!("an unknown scope must be refused naming the set: {other:?}"),
    }

    // Deciding the one serious row leaves a serious queue that is affirmatively empty AND
    // still says the others exist — three statements, not two (ledger #446's shape).
    issue(
        &door,
        Verb::Sink,
        queue::DECIDE_IRI,
        &[(
            "content",
            &format!("id=aaaabbbbccccddddeeee0001&decision=decline&severity={serious}"),
        )],
        &reviewer,
    )
    .expect("a holder of urn:cap:annotate declines");
    let after = page(&door, &[], &reviewer);
    assert!(
        after.contains("No serious pending findings in demo"),
        "{after}"
    );
    assert!(
        after.contains(
            "Nothing is waiting for a decision; 2 other findings are minted and not queued"
        ),
        "{after}"
    );
}

/// ★ **An unrated finding is never hidden.** The gate is on a word; a row with no word has
/// not been classed as a suggestion by anyone, and hiding it would be a finding nobody is
/// ever asked about.
#[test]
fn an_unrated_finding_is_never_hidden() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let reviewer = reviewer();
    plant_finding(
        &door,
        &reviewer,
        "aaaabbbbccccddddeeee0004",
        None,
        "Nobody rated this.",
    );
    let narrowed = page(&door, &[], &reviewer);
    assert!(narrowed.contains("Nobody rated this."), "{narrowed}");
    assert!(narrowed.contains("model: unrated"), "{narrowed}");
    assert!(
        !narrowed.contains("other pending finding"),
        "an unrated row is asked about, not counted as hidden:\n{narrowed}"
    );
}

/// ★★ **The badge carries TWO numbers**: what is waiting for a human in the serious set, and
/// the rest — and both are facts about the STORE, so a finding minted by anyone moves the
/// revision the list refreshes on. The liveness half (colour, spark, the depth's own
/// sentence) is untouched, which the tooltip's first sentence shows.
#[test]
fn the_badge_carries_the_serious_count_and_the_other_count() {
    let dir = scratch_root();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };
    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    let (door, _config) = door(&dir, Some(trigger));
    let reviewer = reviewer();
    let (serious, other) = a_serious_and_an_other_word(&door);

    let rev = |markup: &str| {
        let at = markup
            .find("data-rev=")
            .expect("the badge carries a revision");
        let rest = &markup[at + "data-rev=".len() + 1..];
        rest[..rest.find('\'').expect("a closed attribute")].to_string()
    };

    let before = badge(&door);
    assert!(
        before.contains("class='serious'>0<") && before.contains("class='other'>+0<"),
        "{before}"
    );
    assert!(before.contains("badge-depth empty"), "{before}");
    let first = rev(&before);

    plant_finding(
        &door,
        &reviewer,
        "aaaabbbbccccddddeeee0005",
        Some(&serious),
        "One.",
    );
    plant_finding(
        &door,
        &reviewer,
        "aaaabbbbccccddddeeee0006",
        Some(&other),
        "Two.",
    );
    plant_finding(
        &door,
        &reviewer,
        "aaaabbbbccccddddeeee0007",
        Some(&other),
        "Three.",
    );

    let after = badge(&door);
    assert!(
        after.contains("class='serious'>1<") && after.contains("class='other'>+2<"),
        "the two numbers: {after}"
    );
    assert!(
        after.contains("1 serious finding waiting for a decision, 2 others minted and not queued"),
        "the split is readable, not only visible: {after}"
    );
    // A serious finding waiting for someone is not `empty`, whatever the request queue says.
    assert!(after.contains("badge-depth count"), "{after}");
    // The liveness half is the depth's own sentence, first and unchanged.
    assert!(after.contains("The review queue is empty"), "{after}");
    // ★ Minted by "another process" (the store's own door here, not a pass this server
    // counted), and the revision still moved — the gap the old revision had.
    assert_ne!(
        first,
        rev(&after),
        "a finding minted by anyone is news the list should refresh on"
    );
}

/// ★ **A serious word the contract does not declare stops the server at start, naming both
/// lists** — and the shipped default passes the same check, so a browse release that renames
/// a severity is a refusal here rather than a queue that silently asks about nothing.
#[test]
fn a_serious_word_the_contract_does_not_declare_is_refused_at_start() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);

    let declared = queue::check_serious(&door, &QueuePolicy::default())
        .expect("the shipped default names words the contract declares");
    assert!(declared.len() > 1, "{declared:?}");

    let stale = QueuePolicy {
        serious: vec![declared[0].clone(), "whenever".to_string()],
        configured: true,
    };
    let err = queue::check_serious(&door, &stale).unwrap_err();
    assert!(err.contains("`whenever`"), "{err}");
    for word in &declared {
        assert!(
            err.contains(word.as_str()),
            "the refusal names the set: {err}"
        );
    }

    // The complement is the declared set less the policy — what the banner prints and what
    // the browse arc's `proposals=` argument will take.
    let others = queue::other_severities(&declared, &QueuePolicy::default());
    assert_eq!(
        others.len(),
        declared.len() - QueuePolicy::default().serious.len()
    );
    assert!(others.iter().all(|w| !QueuePolicy::default().is_serious(w)));
}

// ------------------------------------------------------------ the batch view (ledger #506)

/// The file the batch fixtures quote from: a comment line and three code lines, so a
/// comment-shaped quote and a code quote sit in one file.
const LIB: &str = "// Frobs the widget.\nfn alpha() {}\nfn beta() {}\nfn gamma() {}\n";
/// A doc file: every line of it is "a comment or doc line" to browse's heuristic.
const NOTES: &str = "# Notes\nThe queue is not a gate.\n";

/// A scratch root carrying [`LIB`] and [`NOTES`].
fn batch_root() -> TempDir {
    let dir = scratch_root();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).expect("lib.rs");
    std::fs::write(dir.path().join("NOTES.md"), NOTES).expect("NOTES.md");
    dir
}

/// One planted finding, placed by its quote in the file's real text so browse reconciles it
/// to a line — the same quads as [`plant_finding`], with the anchor and the mint time free.
struct Plant<'a> {
    id: &'a str,
    severity: &'a str,
    body: &'a str,
    path: &'a str,
    exact: &'a str,
    created: &'a str,
}

fn plant(door: &Kernel, p: Plant<'_>) {
    let text = if p.path == "NOTES.md" { NOTES } else { LIB };
    let start = text
        .find(p.exact)
        .unwrap_or_else(|| panic!("`{}` is not in {}", p.exact, p.path));
    let end = start + p.exact.len();
    let graph = browse::Graph::chosen()
        .named()
        .expect("a named browse graph")
        .as_str()
        .to_string();
    let (id, path, body, created, exact) = (p.id, p.path, p.body, p.created, p.exact);
    // An empty severity plants an UNRATED finding: no proposal at all, as a reviewer that
    // rated nothing leaves it.
    let severity = match p.severity {
        "" => String::new(),
        word => format!("sh:resultSeverity <urn:iki:severity:{word}> ;"),
    };
    let update = format!(
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
    dcterms:created "{created}"^^xsd:dateTime ;
    prov:wasGeneratedBy <urn:ikigai:browse:review:demo:{path}> ;
    {severity}
    ik:annotates <urn:repo:{ROOT}:file:{path}> ;
    ik:repo "{ROOT}" ;
    ik:path "{path}" ;
    ik:contentHash "sha256:planted" ;
    oa:hasSelector <urn:iki:finding:{id}:selector:quote> ,
                   <urn:iki:finding:{id}:selector:position> .
  <urn:iki:finding:{id}:selector:quote> a oa:TextQuoteSelector ;
    oa:exact "{exact}" .
  <urn:iki:finding:{id}:selector:position> a oa:TextPositionSelector ;
    oa:start "{start}"^^xsd:nonNegativeInteger ;
    oa:end "{end}"^^xsd:nonNegativeInteger .
}} }}"#
    );
    issue(
        door,
        Verb::Sink,
        "urn:iki:store:graph-update",
        &[("graph", &graph), ("content", &update)],
        &reviewer(),
    )
    .expect("the browse graph's write token plants the finding");
}

/// The findings contract's group kinds, in contract order — the ONLY place these tests get
/// them, so no kind word is spelled in this file either.
fn group_kinds(door: &Kernel) -> Vec<String> {
    queue::one_of(
        door,
        &format!("urn:repo:{ROOT}:findings"),
        Verb::Source,
        ikigai_gonk::batch::GROUP_ARG,
    )
    .expect("the findings face declares its group kinds (browse 0.12.0)")
}

/// Browse's own groups of one kind — the oracle the page is checked against.
fn raw_groups(door: &Kernel, kind: &str) -> Vec<serde_json::Value> {
    let answer = issue(
        door,
        Verb::Source,
        &format!("urn:repo:{ROOT}:findings"),
        &[("as", "application/json"), ("group", kind)],
        &reviewer(),
    )
    .expect("the grouped read");
    let body: serde_json::Value = serde_json::from_slice(&answer.bytes).expect("json");
    body["groups"].as_array().expect("a groups array").clone()
}

fn member_ids(group: &serde_json::Value) -> Vec<String> {
    group["members"]
        .as_array()
        .expect("members")
        .iter()
        .map(|m| m["id"].as_str().expect("an id").to_string())
        .collect()
}

/// The kind whose groups carry a declined twin — found by the DATA, as the page finds it.
fn twin_kind(door: &Kernel) -> String {
    group_kinds(door)
        .into_iter()
        .find(|kind| raw_groups(door, kind).iter().any(|g| !g["twin"].is_null()))
        .expect("a kind whose groups carry a declined twin")
}

/// A kind with a twin-less group holding every id in `ids` — somewhere to post a batch from.
fn a_kind_holding(door: &Kernel, ids: &[&str]) -> String {
    group_kinds(door)
        .into_iter()
        .find(|kind| {
            raw_groups(door, kind).iter().any(|g| {
                let members = member_ids(g);
                g["twin"].is_null() && ids.iter().all(|id| members.iter().any(|m| m == id))
            })
        })
        .unwrap_or_else(|| panic!("no kind groups {ids:?} together"))
}

/// One batch through gonk's own adapter, answered with the re-rendered section.
fn batch_by_form(door: &Kernel, body: &str) -> String {
    let answer = issue(
        door,
        Verb::Sink,
        ikigai_gonk::batch::BATCH_IRI,
        &[("content", body)],
        &reviewer(),
    )
    .expect("the batch adapter renders rather than returning a status");
    String::from_utf8(answer.bytes).expect("utf-8")
}

/// A finding's state as the findings face reads it back.
fn state_of(door: &Kernel, id: &str) -> String {
    let answer = issue(
        door,
        Verb::Source,
        &format!("urn:repo:{ROOT}:findings"),
        &[("as", "application/json"), ("state", "all")],
        &reviewer(),
    )
    .expect("the findings read");
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&answer.bytes).expect("json rows");
    rows.into_iter()
        .find(|row| row["id"] == id)
        .map(|row| row["state"].as_str().unwrap_or("").to_string())
        .unwrap_or_else(|| panic!("no `{id}` at all"))
}

/// The value of the option a rendered `<select name='{name}'>` has selected, if any.
fn selected_in(html: &str, name: &str) -> Option<String> {
    let named = html
        .find(&format!(" name='{name}'"))
        .unwrap_or_else(|| panic!("no select named {name}:\n{html}"));
    let start = html[..named].rfind("<select").expect("a select");
    let end = start + html[start..].find("</select>").expect("closes");
    html[start..end]
        .split("<option")
        .skip(1)
        .find(|option| option.contains("selected"))
        .map(|option| {
            let at = option.find("value='").expect("a value") + 7;
            option[at..at + option[at..].find('\'').expect("closes")].to_string()
        })
}

/// Four findings on one file: two serious near-duplicates on one line, one serious comment
/// quote, and one below the gate. Returns (serious, other) — both read off the contract.
fn plant_the_mix(door: &Kernel) -> (String, String) {
    let (serious, other) = a_serious_and_an_other_word(door);
    for (id, severity, body, exact, created) in [
        (
            "a1a1a1a1a1a1a1a1a1a1a1a1",
            &serious,
            "alpha leaks",
            "fn alpha() {}",
            "2026-09-19T12:00:00Z",
        ),
        (
            "a2a2a2a2a2a2a2a2a2a2a2a2",
            &serious,
            "alpha leaks, reworded",
            "fn alpha() {}",
            "2026-09-19T12:05:00Z",
        ),
        (
            "c1c1c1c1c1c1c1c1c1c1c1c1",
            &serious,
            "the comment restates",
            "// Frobs the widget.",
            "2026-09-19T12:00:00Z",
        ),
        (
            "b1b1b1b1b1b1b1b1b1b1b1b1",
            &other,
            "beta could be nicer",
            "fn beta() {}",
            "2026-09-19T12:00:00Z",
        ),
    ] {
        plant(
            door,
            Plant {
                id,
                severity,
                body,
                path: "src/lib.rs",
                exact,
                created,
            },
        );
    }
    (serious, other)
}

/// ★★ **Each kind's view is the contract's groups, filtered to the serious set, with GONK's
/// counts.** The oracle is browse's own grouped read, filtered here the way the Queue filters
/// rows; every kind the contract declares is walked, none is named.
#[test]
fn each_kind_lists_the_contracts_groups_filtered_to_the_serious_set() {
    let dir = batch_root();
    let (door, _config) = door(&dir, None);
    let (_, other) = plant_the_mix(&door);
    let policy = QueuePolicy::default();
    let kinds = group_kinds(&door);
    assert!(kinds.len() > 1, "{kinds:?}");

    // The kind nav is on the ordinary rows page, every word in contract order.
    let rows = page(&door, &[], &reviewer());
    let mut at = 0;
    for kind in &kinds {
        let found = rows[at..]
            .find(&format!(">{kind}</a>"))
            .unwrap_or_else(|| panic!("the kind nav lacks `{kind}` (or is out of order):\n{rows}"));
        at += found;
    }

    let mut hid_somewhere = false;
    for kind in &kinds {
        let groups = raw_groups(&door, kind);
        let mut expected_groups = 0;
        let mut expected_members = 0;
        let html = page(&door, &[("group", kind)], &reviewer());
        for group in &groups {
            let mut shown = 0;
            let all = group["members"].as_array().expect("members").len();
            for member in group["members"].as_array().expect("members") {
                let id = member["id"].as_str().expect("id");
                if policy.queues(member["severity"].as_str()) {
                    shown += 1;
                    assert!(
                        html.contains(&format!("value='{id}'")),
                        "`{kind}` must offer the serious member {id}:\n{html}"
                    );
                } else {
                    hid_somewhere = true;
                    assert!(
                        !html.contains(&format!("value='{id}'")),
                        "`{kind}` offers {id}, rated below the gate:\n{html}"
                    );
                }
            }
            if shown > 0 {
                expected_groups += 1;
                expected_members += shown;
                // Browse's label counts every severity; the group says what was left out.
                let left_out = all - shown;
                let note = format!(
                    "{left_out} other finding{} in this group {} rated below the serious set",
                    if left_out == 1 { "" } else { "s" },
                    if left_out == 1 { "is" } else { "are" },
                );
                if left_out > 0 {
                    assert!(
                        html.contains(&note),
                        "the group's own left-out note `{note}`:\n{html}"
                    );
                }
            }
        }
        if expected_groups == 0 {
            assert!(
                html.contains(&format!(
                    "No {kind} groups among the serious pending findings"
                )),
                "an empty kind says so affirmatively:\n{html}"
            );
            continue;
        }
        let count = format!(
            "{expected_groups} {kind} group{} holding {expected_members} serious pending finding{}",
            if expected_groups == 1 { "" } else { "s" },
            if expected_members == 1 { "" } else { "s" },
        );
        assert!(html.contains(&count), "gonk's own count `{count}`:\n{html}");

        // `severity=all` lists the member the gate left out, wherever browse grouped it.
        let all = page(&door, &[("group", kind), ("severity", "all")], &reviewer());
        for group in &groups {
            for id in member_ids(group) {
                assert!(
                    all.contains(&format!("value='{id}'")),
                    "`{kind}` under severity=all lists {id}:\n{all}"
                );
            }
        }
    }
    assert!(
        hid_somewhere,
        "the fixture plants a `{other}` finding, so some kind must have left it out"
    );

    // A kind the contract does not declare, and a group beside a non-pending state, refuse.
    for args in [
        vec![("group", "whenever")],
        vec![("group", kinds[0].as_str()), ("state", "declined")],
    ] {
        match issue(&door, Verb::Source, queue::QUEUE_IRI, &args, &reviewer()) {
            Err(ikigai_core::Error::InvalidArgument { .. }) => {}
            other => panic!("{args:?} must be refused: {other:?}"),
        }
    }
}

/// ★ **Nothing is pre-decided**: an unticked member is not decided, and a batch decline with
/// no word — or a word the contract does not declare — is refused WHOLE before any member is
/// touched.
#[test]
fn an_unticked_member_is_not_decided_and_a_wordless_batch_is_refused() {
    let dir = batch_root();
    let (door, _config) = door(&dir, None);
    plant_the_mix(&door);
    let (first, second) = ("a1a1a1a1a1a1a1a1a1a1a1a1", "c1c1c1c1c1c1c1c1c1c1c1c1");
    let kind = a_kind_holding(&door, &[first, second]);
    let word = reason_words(&door)[0].clone();

    // No word: refused, and nothing moved.
    let refused = batch_by_form(
        &door,
        &format!("_group={kind}&member={first}&member={second}"),
    );
    assert!(
        refused.contains("Nothing was declined") && refused.contains("reason word"),
        "{refused}"
    );
    assert_eq!(state_of(&door, first), "pending");
    assert_eq!(state_of(&door, second), "pending");

    // A word the contract does not declare: refused the same way.
    let refused = batch_by_form(
        &door,
        &format!("_group={kind}&member={first}&reason=whenever"),
    );
    assert!(
        refused.contains("`whenever` is not a reason word"),
        "{refused}"
    );
    assert_eq!(state_of(&door, first), "pending");

    // Nothing ticked: refused.
    let refused = batch_by_form(&door, &format!("_group={kind}&reason={word}"));
    assert!(refused.contains("no finding was ticked"), "{refused}");

    // One ticked, one not: exactly the ticked one is declined, with the batch's word.
    let done = batch_by_form(
        &door,
        &format!("_group={kind}&member={first}&reason={word}"),
    );
    assert!(done.contains("Declined 1 of 1."), "{done}");
    assert_eq!(state_of(&door, first), "declined");
    assert_eq!(
        state_of(&door, second),
        "pending",
        "an unticked member is not decided"
    );
    assert_eq!(
        row(&door, "declined", first)["decision"]["reason"],
        word.as_str()
    );
}

/// ★★ **The twin-carrying view** (Brian, 2026-09-25): every group at once, in ONE form, each
/// member pre-set to its OWN twin's word; a twin with no word leaves its member's picker
/// empty and the batch refused until a word is picked or the member unticked.
#[test]
fn a_recurrence_batch_declines_each_member_with_its_own_twins_word() {
    let dir = batch_root();
    let (door, _config) = door(&dir, None);
    let (serious, _) = a_serious_and_an_other_word(&door);
    let words = reason_words(&door);
    let (w1, w2) = (words[0].clone(), words[1].clone());
    let twins = [
        (
            "1111111111111111111111a1",
            "fn alpha() {}",
            Some(w1.as_str()),
        ),
        (
            "1111111111111111111111b1",
            "fn beta() {}",
            Some(w2.as_str()),
        ),
        ("1111111111111111111111c1", "fn gamma() {}", None),
    ];
    for (id, exact, word) in twins {
        plant(
            &door,
            Plant {
                id,
                severity: &serious,
                body: "raised once",
                path: "src/lib.rs",
                exact,
                created: "2026-09-18T12:00:00Z",
            },
        );
        let mut args = vec![("decision", "decline")];
        if let Some(word) = word {
            args.push((queue::REASON_ARG, word));
        }
        issue(
            &door,
            Verb::Sink,
            &format!("urn:iki:finding:{id}"),
            &args,
            &reviewer(),
        )
        .expect("a human declines the twin");
    }
    let fresh = [
        ("2222222222222222222222a2", "fn alpha() {}"),
        ("2222222222222222222222b2", "fn beta() {}"),
        ("2222222222222222222222c2", "fn gamma() {}"),
    ];
    for (id, exact) in fresh {
        plant(
            &door,
            Plant {
                id,
                severity: &serious,
                body: "raised again",
                path: "src/lib.rs",
                exact,
                created: "2026-09-19T12:00:00Z",
            },
        );
    }
    let kind = twin_kind(&door);
    let html = page(&door, &[("group", &kind)], &reviewer());
    assert_eq!(
        html.matches("class='batch'").count(),
        1,
        "every twin-carrying group sits in ONE form:\n{html}"
    );
    let (f1, f2, f3) = (fresh[0].0, fresh[1].0, fresh[2].0);
    assert_eq!(
        selected_in(&html, &format!("reason:{f1}")).as_deref(),
        Some(w1.as_str())
    );
    assert_eq!(
        selected_in(&html, &format!("reason:{f2}")).as_deref(),
        Some(w2.as_str())
    );
    assert_eq!(
        selected_in(&html, &format!("reason:{f3}")).as_deref(),
        Some(""),
        "a twin with no word leaves its member's picker on the empty choice:\n{html}"
    );
    // Each member is shown beside its twin's decision line, word and all.
    assert!(
        html.contains(&format!(
            "the like claim on this line was declined (<span class='reason-word'>{w1}</span>)"
        )),
        "{html}"
    );

    // As the page would post it: the empty choice is not submitted, so f3 has no word.
    let refused = batch_by_form(
        &door,
        &format!(
            "_group={kind}&member={f1}&member={f2}&member={f3}&reason:{f1}={w1}&reason:{f2}={w2}"
        ),
    );
    assert!(
        refused.contains("Nothing was declined") && refused.contains(f3),
        "the wordless member blocks the batch, by name:\n{refused}"
    );
    for id in [f1, f2, f3] {
        assert_eq!(state_of(&door, id), "pending");
    }

    // Unticked, it no longer blocks; each declined member carries ITS twin's word.
    let done = batch_by_form(
        &door,
        &format!(
            "_group={kind}&member={f1}&member={f2}&reason:{f1}={w1}&reason:{f2}={w2}&reason:{f3}="
        ),
    );
    assert!(done.contains("Declined 2 of 2."), "{done}");
    assert_eq!(
        row(&door, "declined", f1)["decision"]["reason"],
        w1.as_str()
    );
    assert_eq!(
        row(&door, "declined", f2)["decision"]["reason"],
        w2.as_str()
    );
    assert_eq!(state_of(&door, f3), "pending");
}

/// ★ **A batch decline IS an ordinary decision**: one decision node per member, read back
/// through the finding face like any other, and a later like claim is grouped under it — and
/// marked by it — exactly as it would be after a single decline.
#[test]
fn a_batch_decline_is_an_ordinary_decision_the_recurrence_mark_reads() {
    let dir = batch_root();
    let (door, _config) = door(&dir, None);
    let (serious, _) = plant_the_mix(&door);
    let word = reason_words(&door).last().expect("a word").clone();
    let (first, second) = ("a1a1a1a1a1a1a1a1a1a1a1a1", "c1c1c1c1c1c1c1c1c1c1c1c1");
    let kind = a_kind_holding(&door, &[first, second]);
    let done = batch_by_form(
        &door,
        &format!("_group={kind}&member={first}&member={second}&reason={word}"),
    );
    assert!(done.contains("Declined 2 of 2."), "{done}");
    for id in [first, second] {
        let decided = row(&door, "declined", id);
        assert_eq!(decided["decision"]["reason"], word.as_str());
        assert_eq!(
            decided["decision"]["severity"],
            serious.as_str(),
            "the proposal, accepted"
        );
    }
    // The re-render is from the source: the declined members are gone from the view.
    assert!(!done.contains(&format!("value='{first}'")), "{done}");

    // A later like claim on the comment line: the twin-carrying kind groups it under the
    // batch-declined finding, suggesting that finding's word, as it would after one decline.
    let later = "3333333333333333333333c3";
    plant(
        &door,
        Plant {
            id: later,
            severity: &serious,
            body: "the comment still restates",
            path: "src/lib.rs",
            exact: "// Frobs the widget.",
            created: "2026-09-20T12:00:00Z",
        },
    );
    let kind = twin_kind(&door);
    let group = raw_groups(&door, &kind)
        .into_iter()
        .find(|g| member_ids(g).iter().any(|m| m == later))
        .expect("the later claim is grouped under a declined twin");
    assert_eq!(group["twin"]["id"], second);
    assert_eq!(group["reason"], word.as_str());
    let html = page(&door, &[("group", &kind)], &reviewer());
    assert_eq!(
        selected_in(&html, &format!("reason:{later}")).as_deref(),
        Some(word.as_str())
    );
}

/// **Publish is not batchable**, and **a doc file's one proposal is shown once.**
#[test]
fn publish_is_not_offered_in_a_batch_and_a_doc_file_shows_one_group() {
    let dir = batch_root();
    let (door, _config) = door(&dir, None);
    let (serious, _) = plant_the_mix(&door);
    let docs = ["d1d1d1d1d1d1d1d1d1d1d1d1", "d2d2d2d2d2d2d2d2d2d2d2d2"];
    for (id, exact) in docs.iter().zip(["# Notes", "The queue is not a gate."]) {
        plant(
            &door,
            Plant {
                id,
                severity: &serious,
                body: "the doc restates",
                path: "NOTES.md",
                exact,
                created: "2026-09-19T12:00:00Z",
            },
        );
    }
    let decisions = queue::one_of(
        &door,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        "decision",
    )
    .expect("the decision set");
    let kinds = group_kinds(&door);

    // Browse proposes the doc file under two kinds with the same members…
    let proposing: Vec<&String> = kinds
        .iter()
        .filter(|kind| {
            raw_groups(&door, kind)
                .iter()
                .any(|g| member_ids(g).iter().any(|m| m == docs[0]))
        })
        .collect();
    assert!(
        proposing.len() >= 2,
        "browse proposes the doc twice: {proposing:?}"
    );
    // …and the batch views show it once.
    let mut showing = 0;
    for kind in &kinds {
        let html = page(&door, &[("group", kind)], &reviewer());
        if html.contains(&format!("value='{}'", docs[0])) {
            showing += 1;
        }
        for word in &decisions {
            assert!(
                !html.contains(&format!("value='{word}'")),
                "the `{kind}` batch view offers the decision `{word}` — a batch only declines:\n{html}"
            );
        }
        assert!(
            !html.contains("class='decide'"),
            "no single-row form in a batch view"
        );
    }
    assert_eq!(showing, 1, "the doc file's one proposal is shown once");

    // And a batch that asks to publish is refused by name, deciding nothing.
    let publish = decisions
        .iter()
        .find(|d| d.as_str() != "decline")
        .expect("a decision other than decline");
    let refused = batch_by_form(
        &door,
        &format!(
            "member={}&reason={}&decision={publish}",
            docs[0],
            reason_words(&door)[0]
        ),
    );
    assert!(refused.contains("a batch only declines"), "{refused}");
    assert_eq!(state_of(&door, docs[0]), "pending");
}

// ------------------------------------------------ the batch view's second pass (ledger #508)

/// The rendered `<input … value='{id}' …>` of one member's box, whole — the serializer
/// orders attributes its own way, so the value is found first and the element walked to.
fn member_input(html: &str, id: &str) -> String {
    let at = html
        .find(&format!("value='{id}'"))
        .unwrap_or_else(|| panic!("no member box for {id}:\n{html}"));
    let start = html[..at].rfind("<input").expect("an input");
    let end = start + html[start..].find('>').expect("closes");
    html[start..=end].to_string()
}

/// A kind whose target-only group (no twin, no kept row) holds every id in `ids` and
/// carries a suggested word — found by the DATA, as the page decides it.
fn a_worded_target_only_kind(door: &Kernel, ids: &[&str]) -> (String, serde_json::Value) {
    for kind in group_kinds(door) {
        if let Some(group) = raw_groups(door, &kind).into_iter().find(|g| {
            let members = member_ids(g);
            g["twin"].is_null()
                && g["kept"].is_null()
                && g["reason"].is_string()
                && ids.iter().all(|id| members.iter().any(|m| m == id))
        }) {
            return (kind, group);
        }
    }
    panic!("no worded target-only group holds {ids:?}");
}

/// Plant `n` serious findings on the one comment line of [`LIB`], so a CODE file's pending
/// findings all quote a comment: two kinds propose the same members, one with a word.
fn plant_comment_quotes(door: &Kernel, n: usize) -> Vec<String> {
    let (serious, _) = a_serious_and_an_other_word(door);
    (0..n)
        .map(|i| {
            let id = format!("cc{i}cc{i}cc{i}cc{i}cc{i}cc{i}cc{i}cc{i}");
            plant(
                door,
                Plant {
                    id: &id,
                    severity: &serious,
                    body: &format!("the comment restates, take {i}"),
                    path: "src/lib.rs",
                    exact: "// Frobs the widget.",
                    created: &format!("2026-09-19T12:0{i}:00Z"),
                },
            );
            id
        })
        .collect()
}

/// ★★ **A twin with no word takes the BATCH word** (Brian, 2026-09-25, ledger #508). The
/// twin-carrying form carries one optional batch-wide picker, unselected, and says how many
/// ticked members will take it; a member's own word — its twin's, or one the person picked —
/// wins over it; and a wordless member with no batch word still refuses the batch.
#[test]
fn a_batch_word_falls_back_for_members_whose_twin_had_none() {
    let dir = batch_root();
    let (door, _config) = door(&dir, None);
    let (serious, _) = a_serious_and_an_other_word(&door);
    let words = reason_words(&door);
    assert!(words.len() >= 3, "{words:?}");
    let (w1, w2, batch_word) = (
        words[0].clone(),
        words[1].clone(),
        words.last().expect("a word").clone(),
    );
    assert!(batch_word != w1 && batch_word != w2);
    // Four declined twins: two with a word, two from before words existed.
    let twins = [
        (
            "1111111111111111111111a1",
            "fn alpha() {}",
            Some(w1.as_str()),
        ),
        (
            "1111111111111111111111b1",
            "fn beta() {}",
            Some(w2.as_str()),
        ),
        ("1111111111111111111111c1", "fn gamma() {}", None),
        ("1111111111111111111111d1", "// Frobs the widget.", None),
    ];
    for (id, exact, word) in twins {
        plant(
            &door,
            Plant {
                id,
                severity: &serious,
                body: "raised once",
                path: "src/lib.rs",
                exact,
                created: "2026-09-18T12:00:00Z",
            },
        );
        let mut args = vec![("decision", "decline")];
        if let Some(word) = word {
            args.push((queue::REASON_ARG, word));
        }
        issue(
            &door,
            Verb::Sink,
            &format!("urn:iki:finding:{id}"),
            &args,
            &reviewer(),
        )
        .expect("a human declines the twin");
    }
    let fresh = [
        ("2222222222222222222222a2", "fn alpha() {}"),
        ("2222222222222222222222b2", "fn beta() {}"),
        ("2222222222222222222222c2", "fn gamma() {}"),
        ("2222222222222222222222d2", "// Frobs the widget."),
    ];
    for (id, exact) in fresh {
        plant(
            &door,
            Plant {
                id,
                severity: &serious,
                body: "raised again",
                path: "src/lib.rs",
                exact,
                created: "2026-09-19T12:00:00Z",
            },
        );
    }
    let (f1, f2, f3, f4) = (fresh[0].0, fresh[1].0, fresh[2].0, fresh[3].0);
    let kind = twin_kind(&door);
    let html = page(&door, &[("group", &kind)], &reviewer());

    // The batch-wide picker: present, on its empty choice, and choosable (not locked); the
    // form says how many will take it. Every member with a twin starts ticked.
    assert_eq!(
        selected_in(&html, "reason").as_deref(),
        Some(""),
        "the batch word starts unselected:\n{html}"
    );
    let batch_picker = reason_picker(&html);
    assert!(
        !batch_picker.contains("required"),
        "the fallback is optional:\n{batch_picker}"
    );
    assert!(
        !batch_picker
            .split("<option")
            .nth(1)
            .expect("an option")
            .contains("disabled"),
        "the empty choice can be chosen back:\n{batch_picker}"
    );
    assert!(
        html.contains("2 of 4 will take it"),
        "the count of wordless twins among the ticked:\n{html}"
    );
    for id in [f1, f2, f3, f4] {
        assert!(
            member_input(&html, id).contains("checked"),
            "a member with a twin starts ticked: {id}\n{html}"
        );
    }
    assert_eq!(
        selected_in(&html, &format!("reason:{f3}")).as_deref(),
        Some("")
    );

    // A wordless member with no batch word still refuses the batch, by name.
    let refused = batch_by_form(&door, &format!("_group={kind}&member={f3}&reason:{f3}="));
    assert!(
        refused.contains("Nothing was declined") && refused.contains(f3),
        "{refused}"
    );
    assert_eq!(state_of(&door, f3), "pending");

    // A member's OWN pick wins over the batch word.
    let done = batch_by_form(
        &door,
        &format!("_group={kind}&member={f4}&reason:{f4}={w2}&reason={batch_word}"),
    );
    assert!(done.contains("Declined 1 of 1."), "{done}");
    assert_eq!(
        row(&door, "declined", f4)["decision"]["reason"],
        w2.as_str(),
        "the member's own word, not the batch's"
    );
    assert!(
        done.contains("1 of 3 will take it"),
        "the re-render counts what is left:\n{done}"
    );

    // As the page posts it: the batch word lands on the wordless member only, and a member
    // left unticked (f2) is not decided.
    let done = batch_by_form(
        &door,
        &format!(
            "_group={kind}&member={f1}&member={f3}&reason:{f1}={w1}&reason:{f2}={w2}&reason:{f3}=&reason={batch_word}"
        ),
    );
    assert!(done.contains("Declined 2 of 2."), "{done}");
    assert_eq!(
        row(&door, "declined", f1)["decision"]["reason"],
        w1.as_str(),
        "the twin's word, not the batch's"
    );
    assert_eq!(
        row(&door, "declined", f3)["decision"]["reason"],
        batch_word.as_str(),
        "the twin had no word, so the batch word is this member's stated reason"
    );
    assert_eq!(state_of(&door, f2), "pending");
    // What is left (f2) has a worded twin: no fallback picker is offered, and the member
    // still carries its own.
    assert!(
        member_input(&done, f2).contains("checked") && !done.contains(" name='reason'"),
        "no member would take a batch word now, so none is offered:\n{done}"
    );
    assert_eq!(
        selected_in(&done, &format!("reason:{f2}")).as_deref(),
        Some(w2.as_str())
    );
}

/// ★★ **A target-only group starts UNTICKED, its word shown and not selected** (Brian,
/// 2026-09-25, ledger #508) — and **a code file whose findings all quote comments keeps its
/// worded group**: the fold sends the wordless twin of the proposal to it, not the reverse.
#[test]
fn a_target_only_group_starts_unticked_with_its_word_shown_and_not_selected() {
    let dir = batch_root();
    let (door, _config) = door(&dir, None);
    let ids = plant_comment_quotes(&door, 5);
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let (kind, group) = a_worded_target_only_kind(&door, &ids);
    let word = group["reason"]
        .as_str()
        .expect("a suggested word")
        .to_string();

    // Another kind proposes the same members on the same file with NO word…
    let wordless: Vec<String> = group_kinds(&door)
        .into_iter()
        .filter(|k| {
            *k != kind
                && raw_groups(&door, k).iter().any(|g| {
                    g["twin"].is_null()
                        && g["kept"].is_null()
                        && g["reason"].is_null()
                        && member_ids(g) == ids
                })
        })
        .collect();
    assert_eq!(
        wordless.len(),
        1,
        "one wordless twin of the proposal: {wordless:?}"
    );

    // …and the proposal is shown ONCE, under the kind that carries the word.
    let html = page(&door, &[("group", &kind)], &reviewer());
    for id in &ids {
        let input = member_input(&html, id);
        assert!(
            !input.contains("checked") && !input.contains("disabled"),
            "a target-only member starts unticked and tickable: {input}"
        );
    }
    assert!(
        html.contains(&format!("suggested word: {word}")),
        "the word is shown beside the group:\n{html}"
    );
    assert_eq!(
        selected_in(&html, "reason").as_deref(),
        Some(""),
        "and not selected:\n{html}"
    );
    let picker = reason_picker(&html);
    assert!(
        picker.contains("required"),
        "a group's word is required:\n{picker}"
    );
    assert!(
        picker
            .split("<option")
            .nth(1)
            .expect("an option")
            .contains("disabled"),
        "the empty choice cannot be submitted:\n{picker}"
    );
    assert!(
        picker.contains(&format!("value='{word}'")),
        "the suggested word is among the choices:\n{picker}"
    );
    let other = page(&door, &[("group", &wordless[0])], &reviewer());
    for id in &ids {
        assert!(
            !other.contains(&format!("value='{id}'")),
            "`{}` shows the worded kind's proposal again:\n{other}",
            wordless[0]
        );
    }
    assert!(other.contains("shown there, once"), "{other}");

    // Nothing ticked is refused; two of five ticked declines exactly two.
    let refused = batch_by_form(&door, &format!("_group={kind}&reason={word}"));
    assert!(refused.contains("no finding was ticked"), "{refused}");
    let done = batch_by_form(
        &door,
        &format!(
            "_group={kind}&member={}&member={}&reason={word}",
            ids[1], ids[3]
        ),
    );
    assert!(done.contains("Declined 2 of 2."), "{done}");
    for (i, id) in ids.iter().enumerate() {
        let expected = if i == 1 || i == 3 {
            "declined"
        } else {
            "pending"
        };
        assert_eq!(state_of(&door, id), expected, "{id}");
    }
}

/// ★ **The rule is evidence, not kind**: a group that proposes a row to KEEP carries evidence
/// about each member, so its members start ticked and its word pre-selected; the kept row
/// is shown locked and unticked.
#[test]
fn a_group_with_a_kept_row_keeps_its_members_pre_ticked() {
    let dir = batch_root();
    let (door, _config) = door(&dir, None);
    plant_the_mix(&door);
    let (kind, group) = group_kinds(&door)
        .into_iter()
        .find_map(|kind| {
            raw_groups(&door, &kind)
                .into_iter()
                .find(|g| !g["kept"].is_null())
                .map(|g| (kind, g))
        })
        .expect("a kind whose group proposes a row to keep");
    let kept = group["kept"]["id"].as_str().expect("the kept id");
    let members = member_ids(&group);
    assert!(!members.is_empty());
    let html = page(&door, &[("group", &kind)], &reviewer());
    for id in &members {
        let input = member_input(&html, id);
        assert!(
            input.contains("checked") && !input.contains("disabled"),
            "a member beside a kept row starts ticked: {input}"
        );
    }
    assert!(
        !html.contains(&format!("value='{kept}'")),
        "the kept row carries no box of its own:\n{html}"
    );
    assert!(html.contains("class='badge final'>kept<"), "{html}");
    assert_eq!(
        selected_in(&html, "reason").as_deref(),
        group["reason"].as_str(),
        "the word is pre-selected beside evidence:\n{html}"
    );
    assert!(!html.contains("suggested word:"), "{html}");
}

/// **An unrated member is shown, locked, and told where to go**: a batch states no rating
/// and a decline needs one, so the row says so and links the finding's own page.
#[test]
fn an_unrated_member_is_shown_locked_with_its_own_page_linked() {
    let dir = batch_root();
    let (door, _config) = door(&dir, None);
    let (serious, _) = a_serious_and_an_other_word(&door);
    let (rated, unrated) = ("e1e1e1e1e1e1e1e1e1e1e1e1", "e0e0e0e0e0e0e0e0e0e0e0e0");
    for (id, severity, exact) in [
        (rated, serious.as_str(), "fn gamma() {}"),
        (unrated, "", "fn beta() {}"),
    ] {
        plant(
            &door,
            Plant {
                id,
                severity,
                body: "something about a function",
                path: "src/lib.rs",
                exact,
                created: "2026-09-19T12:00:00Z",
            },
        );
    }
    let kind = a_kind_holding(&door, &[rated, unrated]);
    let html = page(&door, &[("group", &kind)], &reviewer());
    let locked = member_input(&html, unrated);
    assert!(
        locked.contains("disabled") && !locked.contains("checked"),
        "{locked}"
    );
    assert!(
        html.contains("decide this one singly"),
        "the row says why there is no tick:\n{html}"
    );
    assert!(
        html.contains(&format!("href='/browse/urn:iki:finding:{unrated}'")),
        "and links the finding's own page:\n{html}"
    );
    let offered = member_input(&html, rated);
    assert!(!offered.contains("disabled"), "{offered}");
}

/// ★ The anti-drift guard's third sibling: no group KIND word is spelled in this crate's page
/// code — the kinds reach the page through the findings contract's `group` set.
///
/// ⚠ Two of the words are ordinary English ("file", and the one that also names 0.9.0's
/// recurrence mark in half this crate's comments), so a whole-token scan would fail on prose.
/// What drifts is a word the CODE spells, so the check is for the word as a quoted literal or
/// a query value; the hyphenated kinds, which prose never uses, are also checked as tokens.
#[test]
fn no_group_kind_word_is_written_down_in_this_crate() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let kinds = group_kinds(&door);
    assert!(kinds.len() > 1, "a closed set of more than one: {kinds:?}");
    let xsl = include_str!("../web/gonk.xsl");
    let start = xsl.find("the review queue -->").expect("the queue section");
    let end = start + xsl[start..].find("a ledger -->").expect("the next section");
    for (what, source) in [
        ("src/queue.rs", include_str!("../src/queue.rs")),
        ("src/batch.rs", include_str!("../src/batch.rs")),
        ("web/gonk.xsl (the review queue)", &xsl[start..end]),
        ("web/gonk.js", include_str!("../web/gonk.js")),
    ] {
        for kind in &kinds {
            for spelled in [
                format!("\"{kind}\""),
                format!("'{kind}'"),
                format!("={kind}"),
            ] {
                assert!(
                    !source.contains(&spelled),
                    "`{what}` spells the group kind `{kind}` as {spelled}. The set lives in \
                     ikigai-browse and is read through the contract."
                );
            }
            if kind.contains('-') {
                assert!(
                    !contains_word(source, kind),
                    "`{what}` names the group kind `{kind}`."
                );
            }
        }
    }
}
