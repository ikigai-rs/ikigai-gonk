//! The **Queue** — the review pipeline's state, and the one place a human publishes.
//!
//! ```text
//! /queue          urn:iki:gonk:page:queue      Source  the page
//! /queue/rows     urn:iki:gonk:fragment:queue  Source  the same section, for htmx
//! /queue/decide   urn:iki:gonk:queue:decide    Sink    one human decision
//! ```
//!
//! Ledger [#444](http://localhost:1060/l/default/item/444). `ikigai-browse` 0.5.0 stopped a
//! review pass from minting annotations: a pass produces **pending findings**, and
//! `Sink urn:iki:finding:{id} decision=publish` is the only path into the
//! `urn:iki:annotation:` family. This is the face for that act.
//!
//! # ★ The rule, and the thing that enforces it
//!
//! Brian, 2026-09-19: *"Nothing gets published to Gonk except by the human."* The enforcement
//! is the capability layer, not this page: publishing requires `urn:cap:annotate`, and a
//! review pass no longer declares it, so a trigger can run complete and be unable to publish
//! anything. **This page's job is to not lie about that** — it renders a decision form only
//! to a caller who could actually submit it, because an offer you refuse is a worse UI than
//! no offer and teaches people to ignore refusals.
//!
//! ⚠ **What an anonymous loopback caller can do to a finding: nothing, in either direction.**
//! Measured rather than assumed — `tests/queue.rs::an_anonymous_loopback_caller_can_neither
//! _read_nor_decide_a_finding`. The anonymous grant is `grants_for_all(gonk.http.ledger,
//! Write)` ([`crate::doors::HttpDoor`]), which is four ledger tokens per ledger and nothing
//! else. Reading `urn:repo:{repo}:findings` needs `urn:cap:browse:read:*` and deciding needs
//! that plus `urn:cap:annotate`; the anonymous capability holds neither, so both are typed
//! `Denied`. The `can-write = true` an earlier arc measured for an anonymous caller is the
//! LEDGER's write, which is a different authority over a different graph.
//!
//! # ★★ The menu is rendered FROM THE CONTRACT
//!
//! The severity words, the decision words and the state words are never written down here.
//! They are read from the resources' own descriptions ([`one_of`]) — the same `one_of`
//! `ikigai-browse` serves to the Sink's ArgSpec, to the review prompt, and to its own menu,
//! from one constant. A fourth hard-coded copy in this crate would re-introduce exactly the
//! drift that design prevents, and it would drift silently: the page would go on offering
//! a word the resource had stopped accepting, and the refusal would arrive at the click.
//! ⚠ `tests/queue.rs::no_severity_word_is_written_down_in_this_crate` holds this file and the
//! stylesheet to it, with the CONTRACT as the oracle — so the assertion cannot go stale
//! either.
//!
//! ⚠ **Only the WORDING is ours.** `publish` gets the label "Publish to Gonk" and `decline`
//! gets "Decline"; a value this table does not know renders under its own word rather than
//! being dropped, so a new outcome is offered the day it exists.
//!
//! ★ **The decline reason is the same kind of menu** (browse 0.10.0): a `reason` picker beside
//! the Decline button, its words the finding Sink's own `one_of` in contract order, each
//! word's meaning read out of the input's summary as the option's `title`, and an empty first
//! option for "no reason". None of the words is written here —
//! `tests/queue.rs::no_reason_word_is_written_down_in_this_crate` holds this file, the
//! stylesheet and the script to that, again with the contract as the oracle. The one decision
//! word the picker hangs off is the wording table's, and [`Decide`] forwards a word ONLY with
//! that decision: browse refuses a reason beside a publish, and one form with two buttons is
//! exactly how a person picks a word and then presses Publish.
//!
//! ⚠ **And when the contract cannot be read, no form is rendered at all** — not a fallback
//! list. A page that invents a menu when the manifold is silent is the failure this whole
//! approach exists to prevent, one layer up.
//!
//! # ★ What the page asks about, and what it only counts
//!
//! Ledger [#496](http://localhost:1060/l/default/item/496). Brian, 2026-09-21: *"the preference
//! is to highlight issues that need addressing, so narrowing the squishy stuff is the
//! priority"* — and, the same day, on the kind words a review leaves: *"positive signal is
//! still signal and tells us something about the code."* Both hold; they are about different
//! places. **Minting** keeps
//! every severity. **Triage** asks a human only about the SERIOUS set — `gonk.queue.serious`
//! ([`crate::config::QueuePolicy`]), validated at start against the finding contract's own
//! `severity` set ([`check_serious`]), never spelled in this file. Everything else is still
//! minted, stored, anchored, counted in the badge's second number and listed under
//! `?severity=all` ([`SCOPE_ARG`]); it just does not ask for a decision. On 2026-09-21 the
//! default hid 185 of 319 pending rows and asked about 134 (133 serious, 1 unrated).
//!
//! ⚠ **The trap.** Severity is self-reported by the model, and
//! [#449](http://localhost:1060/l/default/item/449) measured a prompt asking only for the
//! serious tier moving the serious share 27% → 62% by RE-LABELLING. Gating on the word makes the
//! word load-bearing. Two defences: nothing this page renders or sends reaches a pass (the
//! prompt is browse's, and this crate adds no hint, banner or form copy a pass could see), and
//! `urn:iki:gonk:review:depth` reports the serious share of what this run has minted
//! ([`crate::trigger::Status::serious_share_percent`]) so a jump with no model or prompt
//! change is a number a person sees.
//!
//! # The asymmetry the page has to make legible
//!
//! A decision is **final**: an identical repeat is a no-op, and anything that would change
//! the record is refused naming what is on file. There is **no Delete on a finding** —
//! declining is how a human removes one, and the decline is the record. Undoing a
//! publication is `delete urn:iki:annotation:{id}`, a separate visible act under the same
//! capability, reached through the annotation itself in the browse face. The cards say so.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Description, Endpoint, Error, InputSource, Invocation, Iri,
    Kernel, Representation, Request, Result, Verb,
};
use serde_json::Value;

use crate::render::{self, element, envelope, wrap};
use crate::web::{self, Web};

/// `urn:iki:gonk:page:queue` — the whole page.
pub const QUEUE_IRI: &str = "urn:iki:gonk:page:queue";
/// Where that page is served, for the header link.
pub const QUEUE_PATH: &str = "/queue";
/// `urn:iki:gonk:fragment:queue` — the queue section alone, for an htmx swap.
pub const ROWS_IRI: &str = "urn:iki:gonk:fragment:queue";
/// Where the fragment is served.
pub const ROWS_PATH: &str = "/queue/rows";
/// `urn:iki:gonk:queue:decide` — one human decision, from a form.
pub const DECIDE_IRI: &str = "urn:iki:gonk:queue:decide";
/// Where the form posts.
pub const DECIDE_PATH: &str = "/queue/decide";
/// `urn:iki:gonk:fragment:queue-depth` — the nav badge, one number and one state word.
pub const BADGE_IRI: &str = "urn:iki:gonk:fragment:queue-depth";
/// Where the badge polls.
pub const BADGE_PATH: &str = "/queue/depth";

/// How often the badge asks, as htmx spells an interval. See [`Badge`] for why it is not
/// one second.
pub const BADGE_EVERY: &str = "10s";

/// The same cadence as arithmetic — the number [`BADGE_RECENT_MS`] is derived from.
///
/// ⚠ Two spellings of one number, because htmx wants `"10s"` and a window wants
/// milliseconds. `tests/queue.rs::the_badge_cadence_has_one_number_in_two_spellings` is what
/// keeps them the same number; nothing else can.
pub const BADGE_EVERY_MS: u64 = 10_000;

/// How long after a pass ENDS the badge still reports that something happened.
///
/// ★ **The window is the poll interval, and that is the whole of the fix for
/// [#469](http://localhost:1060/l/default/item/469).** A depth gauge can only report what is
/// true at the instant it is asked, so a pass that starts and finishes between two polls is
/// invisible to it — which is exactly what happened on the first live run: three seconds of
/// work between two ten-second polls, and the badge read 0 then 0. A window at least as long
/// as the interval makes *"anything since the last poll"* the unit, and then nothing can
/// happen entirely unseen: whatever the phase, at least one poll lands inside the window.
///
/// Twice the interval rather than exactly one, so the afterglow lasts one or two polls and a
/// person who looks up a moment late still sees it.
pub const BADGE_RECENT_MS: u64 = 2 * BADGE_EVERY_MS;

/// ⚠ The guarantee the fix rests on, checked by the COMPILER rather than believed: a window
/// shorter than the interval would let a pass begin and end between two polls and be seen by
/// neither, which is the bug itself.
const _: () = assert!(BADGE_RECENT_MS >= BADGE_EVERY_MS);

/// The event the header's badge asks the Queue page's list to refresh on, when its
/// `depth_rev` has changed — `web/gonk.js` relays it, `web/gonk.xsl` listens for it.
///
/// ★ **One cadence, not two.** The badge already polls; a second `every Ns` on the list
/// would double the server's clock and let the two drift. So the list has no clock of its
/// own: it refreshes when the poll that is already happening brings news, and never
/// otherwise.
pub const NEWS_EVENT: &str = "gonk:news";

/// The finding family's own name — the Sink a decision reaches, and the description the
/// menu is rendered from. Taken one id at a time (`{prefix}{id}`), never guessed.
const FINDING_PREFIX: &str = "urn:iki:finding:";

/// The finding Sink's one-word decline reason (browse 0.10.0). Its WORDS are the contract's
/// ([`one_of_with_meanings`]); only the argument's name is ours.
pub const REASON_ARG: &str = "reason";

/// The one decision a reason word travels with. ⚠ A decision WORD, like the two in
/// [`decision_label`] — the wording table's, not a copy of a set: `ikigai-browse` states the
/// rule in the `reason` summary ("only with decision=decline") and REFUSES a word beside any
/// other decision, so this is what the adapter checks before forwarding one and what the form
/// hangs the picker beside.
const REASONED_DECISION: &str = "decline";

/// The picker's empty first option: "no reason", which browse reads as omitted.
const NO_REASON_LABEL: &str = "why? (optional)";

/// The machine face this module asks every resource it reads for.
const JSON: &str = "application/json";

/// How many rows a page draws before it stops and says so.
///
/// The ledger listing's bound and its reason (`README`, ledger #419): the server-side XSLT
/// costs milliseconds per row, and a page slow enough to read as a hung server is worse than
/// a page that admits what it left out. `?limit=` asks for more.
const ROWS: usize = 50;

/// The ceiling `?limit=all` means. Past this the bound REFUSES rather than truncating.
const MAX_ROWS: usize = 500;

/// The argument that widens the page: `?severity=all`. ⚠ Its VALUES are not severity words
/// — they name a scope — so the contract's own set is never retyped here.
pub const SCOPE_ARG: &str = "severity";
/// The default scope: the rows whose word is in `gonk.queue.serious`, plus the unrated.
pub const SCOPE_SERIOUS: &str = "serious";
/// Every row, whatever its word.
pub const SCOPE_ALL: &str = "all";

/// An id to read the finding family's CONTRACT through. The id is a position hash in
/// `ikigai-browse` and the description is the template's, identical for every id, so this
/// names no finding and reads nothing from the store — the same trick
/// [`crate::trigger::review_probe_iri`] uses on the review.
const PROBE_ID: &str = "000000000000000000000000";

/// The finding contract's severity set, with every word of the policy checked against it.
///
/// ★ Run by `main` before a door opens, so a `gonk.queue.serious` word the contract does not
/// declare — or a DEFAULT that a browse release has quietly outgrown — is a refusal at start
/// that names both lists, never a queue that silently asks about nothing. The words are never
/// retyped in this crate: `ikigai-browse` owns them, the config default is the one place gonk
/// spells them, and this check is what keeps that spelling honest.
///
/// # Errors
///
/// When the contract cannot be read (this server composes browse itself, so that is a bug or
/// a changed contract), or a word is not in it.
pub fn check_serious(
    hub: &Kernel,
    policy: &crate::config::QueuePolicy,
) -> std::result::Result<Vec<String>, String> {
    let iri = finding_iri(PROBE_ID);
    let Some(declared) = one_of(hub, &iri, Verb::Sink, "severity") else {
        return Err(format!(
            "`{iri}` does not declare a closed `severity` set for Sink, so gonk.queue.serious \
             (`{}`) cannot be checked against it and the Queue page could render no decision \
             form. This server composes ikigai-browse itself, so this is a changed contract \
             or a bug, not a configuration to fix",
            policy.serious.join(",")
        ));
    };
    for word in &policy.serious {
        if !declared.contains(word) {
            return Err(format!(
                "gonk.queue.serious names `{word}`, which the finding contract does not \
                 declare: its severities are {}. The set lives in ikigai-browse and is read \
                 from the contract{}",
                declared.join(", "),
                if policy.configured {
                    "; spell one of those".to_string()
                } else {
                    format!(
                        ". No line is written, so the DEFAULT this build ships \
                         (`{}`) is what has gone stale — report it, and set the line to \
                         run meanwhile",
                        crate::config::DEFAULT_QUEUE_SERIOUS
                    )
                }
            ));
        }
    }
    Ok(declared)
}

/// The declared severities the policy leaves out — what the gate does not ask about, and the
/// complement the browse arc's file-page `proposals=` argument will take (see
/// [`crate::k`]'s seam).
pub fn other_severities(declared: &[String], policy: &crate::config::QueuePolicy) -> Vec<String> {
    declared
        .iter()
        .filter(|word| !policy.is_serious(word))
        .cloned()
        .collect()
}

/// The words gonk sends browse as `proposals=`: the contract's own severity set less the
/// configured serious words — read from the contract at call time, so a browse release that
/// renames a word changes what is sent without anything here being edited. `None` when the
/// contract cannot be read, and the caller sends nothing rather than a guess (ledger #496).
pub fn proposal_words(hub: &Kernel, policy: &crate::config::QueuePolicy) -> Option<Vec<String>> {
    one_of(hub, &finding_iri(PROBE_ID), Verb::Sink, "severity")
        .map(|declared| other_severities(&declared, policy))
}

// ------------------------------------------------------------------ the contract

/// The closed set a resource's own contract declares for one argument of one verb — the
/// ONLY source of a menu's options in this module.
///
/// `None` for five reasons, in the order the body rules them out. Every caller treats
/// `None` the same way — "do not render a menu", never "use the usual list" — so the code
/// cannot act on the difference, and the list is for the person holding a card that has no
/// form and wanting to know which thing is missing.
///
/// 1. `iri` is not a well-formed IRI. A bug on this side; it says nothing about the
///    resource, which was never asked.
/// 2. **It does not resolve.** Nothing is bound there, or what is bound does not answer
///    `Meta` — either way the kernel has no description to hand back at all. The resource
///    is ABSENT.
/// 3. **It resolves and does not answer that verb.** The endpoint is present and describes
///    itself; `verb` is simply not among the verbs it lists. The resource is PRESENT and
///    answers a different question. ⚠ This is not "declares no [`ActionSpec`]" — an
///    endpoint that answers the verb but authors flat gets a spec synthesized from its own
///    `inputs`, so a missing per-verb spec never reaches here. ⚠ And `Verb::Meta` is
///    filtered out of `action_specs()` upstream, so asking for it is `None` whatever the
///    contract says.
/// 4. It answers the verb and names no such input.
/// 5. It names the input and leaves it open-valued. A contract saying "any string" — a
///    decision, not an omission, and the reason this collapses to `None` with the rest is
///    that an open-valued input has no menu to draw either.
pub fn one_of(hub: &Kernel, iri: &str, verb: Verb, argument: &str) -> Option<Vec<String>> {
    let values = input_spec(hub, iri, verb, argument)?.one_of;
    (!values.is_empty()).then_some(values)
}

/// One declared input of one verb, whole — [`one_of`]'s first four `None`s, and the
/// summary beside the set for a caller that wants what the words MEAN.
fn input_spec(hub: &Kernel, iri: &str, verb: Verb, argument: &str) -> Option<ArgSpec> {
    let target = Iri::parse(iri).ok()?;
    hub.describe(&target)?
        .action_specs()
        .into_iter()
        .find(|spec| spec.verb == verb)?
        .inputs
        .into_iter()
        .find(|input| input.name == argument)
}

/// The contract's closed set for `argument`, each word paired with what the input's own
/// summary says it means — `None` exactly when [`one_of`] is.
///
/// ★ The meanings are READ, not held: `ikigai-browse` builds the `reason` summary as
/// `…. word: meaning; word: meaning; ….` from the same constant as the set, so a word it adds
/// arrives here with its definition. A summary that does not follow that shape costs only the
/// hint — each word keeps its place in the menu with no meaning attached (`meaning`).
pub fn one_of_with_meanings(
    hub: &Kernel,
    iri: &str,
    verb: Verb,
    argument: &str,
) -> Option<Vec<(String, Option<String>)>> {
    let spec = input_spec(hub, iri, verb, argument)?;
    let words: Vec<(String, Option<String>)> = spec
        .one_of
        .iter()
        .map(|word| (word.clone(), meaning(&spec.summary, word)))
        .collect();
    (!words.is_empty()).then_some(words)
}

/// What `summary` says `word` means, when it says so as `word: meaning` ended by `; ` or by
/// the summary's own end. ⚠ The word must start a token (the start, or after a space), so a
/// word that ends another — or the prose before the list — is not read as a definition.
fn meaning(summary: &str, word: &str) -> Option<String> {
    let marker = format!("{word}: ");
    let (at, _) = summary
        .match_indices(&marker)
        .find(|(at, _)| *at == 0 || summary[..*at].ends_with(' '))?;
    let rest = &summary[at + marker.len()..];
    let text = rest[..rest.find("; ").unwrap_or(rest.len())]
        .trim()
        .trim_end_matches('.')
        .trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The findings resource for one root.
fn findings_iri(root: &str) -> String {
    format!("urn:repo:{root}:findings")
}

/// One finding's own resource.
fn finding_iri(id: &str) -> String {
    format!("{FINDING_PREFIX}{id}")
}

/// Whether this capability may decide — the token `urn:iki:finding:{id}`'s Sink declares.
///
/// Presentation only: the Sink enforces it whatever this page shows, and
/// [`Decide`] declares it too, so the kernel refuses before dispatch. A wrong answer here
/// costs a misdrawn page, never an unauthorized decision.
///
/// It is still worth asking, and for two things rather than one: whether a decision form is
/// drawn on a card at all, and — when it is not — whether the list carries the `read-only`
/// posture that NAMES the grant a caller is missing. Without the second, a reader holding
/// only a read grant gets a queue of findings, no buttons, and nothing on the page saying
/// why; the layers that actually enforce this refuse at submit time, which is too late to
/// be an explanation.
fn can_decide(inv: &Invocation<'_>) -> bool {
    inv.capability.allows(ikigai_browse::CAP_ANNOTATE)
}

/// Whether the header should carry a Queue link for this caller.
///
/// ★ Both halves, and the second is easy to forget: a caller must be able to DECIDE
/// (ledger #444 settled that — "a link to a page of things you cannot decide is worse than no
/// link") **and** there must be a repository it may read, or the page behind the link is a
/// refusal with a nav entry pointing at it.
pub(crate) fn offers_queue(web: &Web, inv: &Invocation<'_>) -> bool {
    can_decide(inv) && !crate::k::readable_roots(web, inv).is_empty()
}

// ---------------------------------------------------------------------- the page

/// `urn:iki:gonk:page:queue` and `urn:iki:gonk:fragment:queue`.
pub struct QueuePage {
    pub web: Arc<Web>,
    /// `false` renders the whole page; `true` renders the queue section alone.
    pub fragment: bool,
}

/// What one root's read answered.
enum Rows {
    /// The rows, in the order the resource returned them (already triage order).
    Got(Vec<Value>),
    /// The read was refused or failed, with the reason as the resource stated it.
    Failed(String),
}

/// What narrows a rendering: the three values both entrances agree on.
#[derive(Default)]
struct Params {
    /// Which part of the pipeline — validated against the findings contract, not here.
    state: Option<String>,
    /// One configured root, or every readable one.
    repo: Option<String>,
    /// How many rows to draw.
    limit: Option<String>,
    /// [`SCOPE_SERIOUS`] (the default) or [`SCOPE_ALL`] — the `severity` argument.
    scope: Option<String>,
}

impl Params {
    /// From a Source's query arguments.
    fn from(inv: &Invocation<'_>) -> Params {
        let arg = |name: &str| inv.inline_str(name).ok().map(str::to_string);
        Params {
            state: arg("state"),
            repo: arg("repo"),
            limit: arg("limit"),
            scope: arg(SCOPE_ARG),
        }
    }
}

/// Which rows the page asks about: `?severity=serious` (the default, the configured set) or
/// `?severity=all`.
fn scope_wanted(asked: Option<&str>) -> Result<&'static str> {
    match asked.map(str::trim) {
        None | Some("") | Some(SCOPE_SERIOUS) => Ok(SCOPE_SERIOUS),
        Some(SCOPE_ALL) => Ok(SCOPE_ALL),
        Some(other) => Err(Error::InvalidArgument {
            name: SCOPE_ARG.to_string(),
            detail: format!(
                "`{other}`: `{SCOPE_SERIOUS}` (the set `gonk.queue.serious` names — what the \
                 page asks a human about) or `{SCOPE_ALL}` (every severity)"
            ),
        }),
    }
}

/// How many rows to draw: `?limit=<n>` or `?limit=all`.
fn rows_wanted(limit: Option<&str>) -> Result<usize> {
    match limit {
        None => Ok(ROWS),
        Some("all") => Ok(MAX_ROWS),
        Some(other) => match other.parse::<usize>() {
            Ok(n) if n > 0 && n <= MAX_ROWS => Ok(n),
            _ => Err(Error::InvalidArgument {
                name: "limit".to_string(),
                detail: format!(
                    "`{other}`: a row count between 1 and {MAX_ROWS}, or `all` for {MAX_ROWS}"
                ),
            }),
        },
    }
}

/// Read one root's findings at `state`, under the CALLER's capability — the page's rows and
/// the badge's counts come from this one read.
async fn read_findings(inv: &Invocation<'_>, root: &str, state: &str) -> Rows {
    let iri = findings_iri(root);
    let Ok(target) = Iri::parse(&iri) else {
        return Rows::Failed(format!("`{iri}` is not an IRI"));
    };
    let request = Request::new(Verb::Source, target)
        .with_arg("as", ArgRef::Inline(b"application/json".to_vec()))
        .with_arg("state", ArgRef::Inline(state.as_bytes().to_vec()));
    match inv.issue(request).await {
        Err(e) => Rows::Failed(format!("{e}")),
        Ok(answer) => match serde_json::from_slice::<Value>(&answer.bytes) {
            Ok(Value::Array(rows)) => Rows::Got(rows),
            Ok(_) | Err(_) => Rows::Failed(format!(
                "`{iri}` answered something that is not a JSON array of findings"
            )),
        },
    }
}

/// Whether a human has already answered this row — a published or declined finding carries
/// its decision, and the gate does not apply to it.
fn decided(row: &Value) -> bool {
    row.get("decision").is_some_and(|d| !d.is_null())
}

/// The severity word the gate reads for one row: the human's override when a decision set
/// one, else the model's proposal, else nothing.
///
/// ⚠ `effective_severity` already falls back to `severity` on browse's side, so the second
/// arm is belt-and-braces for a row that omits the derived field — and the comment is the
/// point: the gate reads the EFFECTIVE word, so a human re-rating is what decides once one
/// exists. Today none does (0 of 371 pending rows differed on 2026-09-21).
fn rated(row: &Value) -> Option<&str> {
    row.get("effective_severity")
        .and_then(Value::as_str)
        .or_else(|| row.get("severity").and_then(Value::as_str))
}

/// What is waiting for a human across the readable roots, split the way the page splits it.
struct Counts {
    /// Rows the default page asks about: serious, or unrated.
    serious: usize,
    /// Rows it only counts.
    other: usize,
    /// Roots whose findings could not be read under this caller.
    refused: usize,
}

impl Counts {
    /// The badge's half of the tooltip.
    fn sentence(&self) -> String {
        let mut out = format!(
            "{} serious finding{} waiting for a decision, {} other{} minted and not queued.",
            self.serious,
            if self.serious == 1 { "" } else { "s" },
            self.other,
            if self.other == 1 { "" } else { "s" },
        );
        if self.refused > 0 {
            out.push_str(&format!(
                " ({} repositor{} could not be read.)",
                self.refused,
                if self.refused == 1 { "y" } else { "ies" }
            ));
        }
        out
    }
}

/// Count the pending findings this caller may read, under the configured policy.
async fn pending_counts(web: &Web, inv: &Invocation<'_>) -> Counts {
    let mut counts = Counts {
        serious: 0,
        other: 0,
        refused: 0,
    };
    for root in crate::k::readable_roots(web, inv) {
        match read_findings(inv, &root, "pending").await {
            Rows::Got(rows) => {
                for row in &rows {
                    if web.queue.queues(rated(row)) {
                        counts.serious += 1;
                    } else {
                        counts.other += 1;
                    }
                }
            }
            Rows::Failed(_) => counts.refused += 1,
        }
    }
    counts
}

/// `a`, `a or b`, `a, b or c`.
fn join_or(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

impl QueuePage {
    /// The whole body: the state nav, the intray line, the rows, and the refusals.
    ///
    /// ⚠ `params` is passed rather than read from `inv`, because [`Decide`] renders this same
    /// section after a write and its invocation carries the FORM, not a query string — and
    /// `Invocation` has no reborrow that swaps the request (only `with_bindings`). Threading
    /// the values is what keeps one renderer serving both entrances.
    async fn body(
        &self,
        inv: &Invocation<'_>,
        params: &Params,
        flash: Option<(&str, &str)>,
    ) -> Result<Representation> {
        let roots = crate::k::readable_roots(&self.web, inv);
        let wanted = rows_wanted(params.limit.as_deref())?;
        let scope = scope_wanted(params.scope.as_deref())?;
        let only = params
            .repo
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string);
        if let Some(repo) = &only {
            if !roots.iter().any(|r| r == repo) {
                return Err(Error::Denied(format!(
                    "`{repo}` is not a repository this grant may read. A grant names one root \
                     as `{}{repo}`, or every root as `{}`.",
                    ikigai_browse::CAP_PREFIX,
                    ikigai_browse::CAP_WILDCARD
                )));
            }
        }
        let chosen: Vec<String> = match &only {
            Some(repo) => vec![repo.clone()],
            None => roots.clone(),
        };

        // ★ The states come from the findings resource's own contract, like every other menu
        // here. With no readable root there is no contract to read and no rows to filter, so
        // the nav is absent rather than invented.
        let states = roots
            .first()
            .and_then(|root| one_of(&self.web.hub, &findings_iri(root), Verb::Source, "state"));
        let state = match params.state.as_deref().map(str::trim) {
            None | Some("") => "pending".to_string(),
            Some(asked) => match &states {
                Some(known) if !known.iter().any(|s| s == asked) => {
                    return Err(Error::InvalidArgument {
                        name: "state".to_string(),
                        detail: format!("`{asked}` is not one of {}", known.join(", ")),
                    })
                }
                _ => asked.to_string(),
            },
        };

        let mut read: Vec<(String, Rows)> = Vec::new();
        for root in &chosen {
            let rows = read_findings(inv, root, &state).await;
            read.push((root.clone(), rows));
        }

        // ★★ THE GATE (ledger #496): by default the page asks a human only about the SERIOUS
        // rows. Every other row was still minted, stored and anchored, and it is still
        // counted here — it is simply not offered for a decision, and `severity=all` lists
        // it. The word the gate reads is the human's override when there is one and the
        // model's proposal otherwise (`effective_severity`, falling back to `severity`, see
        // [`rated`]); an unrated row is never hidden ([`crate::config::QueuePolicy::queues`]).
        //
        // ★ It gates UNDECIDED rows only. A published or declined row asks nothing of anyone
        // — it is the record of an answer — so it is listed whatever its word, under every
        // state filter. (A human who publishes a finding as the mildest word the contract
        // offers must still find it on the published tab.)
        //
        // ⚠ Nothing about this gate is visible to a review pass — no argument, no banner, no
        // form copy reaches the prompt — and that is a defence, not an omission: severity is
        // self-reported, and a model that learned only serious words get read would rate
        // everything serious. The tripwire for that is the depth's serious share.
        let policy = &self.web.queue;
        let mut hidden = 0usize;
        let mut undecided = 0usize;
        for (_, rows) in &mut read {
            if let Rows::Got(rows) = rows {
                undecided += rows.iter().filter(|row| !decided(row)).count();
                if scope == SCOPE_SERIOUS {
                    let before = rows.len();
                    rows.retain(|row| decided(row) || policy.queues(rated(row)));
                    hidden += before - rows.len();
                }
            }
        }
        // Where the sentences point: the one root asked for, or how many were read.
        let where_ = match chosen.as_slice() {
            [one] => one.clone(),
            many => format!("{} repositories", many.len()),
        };
        // The words the gate leaves out, from the CONTRACT rather than a list: the finding
        // Sink's own severity set less the configured serious words. `None` when the contract
        // cannot be read, in which case the sentence says "other" and names nothing.
        let others = proposal_words(&self.web.hub, policy);

        let matched: usize = read
            .iter()
            .map(|(_, r)| match r {
                Rows::Got(rows) => rows.len(),
                Rows::Failed(_) => 0,
            })
            .sum();
        let refused: Vec<&(String, Rows)> = read
            .iter()
            .filter(|(_, r)| matches!(r, Rows::Failed(_)))
            .collect();

        let decide = can_decide(inv);
        let mut children = web::nav(&self.web, inv, &web::readable_ledgers(&self.web, inv), None);
        if let Some((kind, text)) = flash {
            children.push_str(&element("flash", &[("kind", kind)], text));
        }
        children.push_str(&self.intray_element(inv).await);
        if let Some(known) = &states {
            for name in known {
                children.push_str(&element(
                    "state",
                    &[
                        ("name", name),
                        ("href", &page_url(&query(name, only.as_deref(), scope))),
                        ("rows-url", &rows_url(&query(name, only.as_deref(), scope))),
                        ("current", flag(name == &state)),
                    ],
                    "",
                ));
            }
            // The scope nav beside it: the serious set, spelled from the configuration so a
            // reader sees which words the page is asking about, and everything. Only where
            // the gate can apply — the pending tab, or any listing that has an undecided row
            // in it; on a tab of records it would be a control that does nothing.
            let serious_label = format!("serious: {}", policy.serious.join(", "));
            let gate_applies = state == "pending" || undecided > 0;
            for (name, label) in [
                (SCOPE_SERIOUS, serious_label.as_str()),
                (SCOPE_ALL, "all severities"),
            ] {
                if !gate_applies {
                    break;
                }
                children.push_str(&element(
                    "scope",
                    &[
                        ("name", name),
                        ("label", label),
                        ("href", &page_url(&query(&state, only.as_deref(), name))),
                        ("rows-url", &rows_url(&query(&state, only.as_deref(), name))),
                        ("current", flag(name == scope)),
                    ],
                    "",
                ));
            }
        }
        for (root, rows) in &read {
            if let Rows::Failed(why) = rows {
                children.push_str(&element("denied", &[("repo", root)], why));
            }
        }
        if hidden > 0 {
            // ★ The rows the gate left out are SAID, with their words and a way to them: a
            // page that quietly showed fewer findings than exist would read as a smaller
            // corpus, not a narrower question.
            let rated = match &others {
                Some(words) if !words.is_empty() => format!(" — rated {} —", join_or(words)),
                _ => String::new(),
            };
            children.push_str(&element(
                "hidden",
                &[
                    ("count", &hidden.to_string()),
                    (
                        "href",
                        &page_url(&query(&state, only.as_deref(), SCOPE_ALL)),
                    ),
                    (
                        "rows-url",
                        &rows_url(&query(&state, only.as_deref(), SCOPE_ALL)),
                    ),
                    ("label", "list all severities"),
                ],
                &format!(
                    "{hidden} other {state} finding{}{rated} {} minted, anchored and counted, \
                     not queued: nothing there asks for a decision.",
                    if hidden == 1 { "" } else { "s" },
                    if hidden == 1 { "is" } else { "are" },
                ),
            ));
        }

        // The rows, each repository's in the order its resource returned them — which is
        // already triage order (severity rank, then path, then position). ⚠ They are NOT
        // re-sorted across repositories: that ordering is `ikigai-browse`'s, computed from a
        // severity rank this crate does not have and must not re-derive, so the page groups
        // rather than claiming a global order it did not compute.
        let mut drawn = 0usize;
        for (root, rows) in &read {
            let Rows::Got(rows) = rows else { continue };
            for row in rows {
                if drawn == wanted {
                    break;
                }
                children.push_str(&self.finding_element(
                    row,
                    root,
                    decide,
                    &state,
                    only.as_deref(),
                    scope,
                ));
                drawn += 1;
            }
        }

        let mut attributes: Vec<(&str, String)> = vec![
            ("view", "queue".to_string()),
            ("full", flag(!self.fragment).to_string()),
            ("title", "Queue".to_string()),
            ("state", state.clone()),
            ("scope", scope.to_string()),
            ("page-url", page_url(&query(&state, only.as_deref(), scope))),
            ("rows-url", rows_url(&query(&state, only.as_deref(), scope))),
            // ★ How this section re-fetches ITSELF when the header's poll brings news
            // ([#469](http://localhost:1060/l/default/item/469)). It is the rows URL with
            // this request's own `limit` kept, because a human who asked to see all 300
            // findings must not be quietly cut back to the first 50 by a refresh nobody
            // asked for.
            (
                "refresh-url",
                rows_url(&refresh_query(
                    &state,
                    only.as_deref(),
                    scope,
                    params.limit.as_deref(),
                )),
            ),
            ("news", NEWS_EVENT.to_string()),
            // ⚠ Shown only when a refresh was HELD BACK — `web/gonk.js` unhides it. The
            // wording lives here with every other sentence this face speaks, so the
            // stylesheet stays a stylesheet and the script stays a relay.
            (
                "stale-text",
                "New findings arrived while you were deciding. They will appear when this \
                 decision is submitted or the selection is left as it was."
                    .to_string(),
            ),
            (
                "message",
                "The review pipeline. ⚠ A queue, not a gate: nothing here blocks a commit, a \
                 push or a merge, and nothing reaches Gonk until a person publishes it."
                    .to_string(),
            ),
        ];
        if let Some(repo) = &only {
            attributes.push(("repo", repo.clone()));
        }
        if roots.is_empty() {
            attributes.push(("empty", "true".to_string()));
            attributes.push((
                "empty-text",
                if self.web.browse_roots.is_empty() {
                    "This server has no browse root configured (`gonk.browse.root`), so no \
                     review pass can run and there is no queue to show."
                        .to_string()
                } else {
                    format!(
                        "This browser holds no grant naming a repository here, so no finding is \
                         readable. A grant names one root as `{}<root>`, or every root as `{}`.",
                        ikigai_browse::CAP_PREFIX,
                        ikigai_browse::CAP_WILDCARD
                    )
                },
            ));
        } else if matched == 0 && refused.is_empty() {
            // ⚠ The affirmative sentence, and it is the point of writing it out: "no pending
            // findings" is a DIFFERENT statement from a page that failed to load, and the two
            // must not look alike (ledger #446). And "no serious findings, N others minted" is
            // a third statement, different again from "no findings at all".
            attributes.push(("empty", "true".to_string()));
            attributes.push((
                "empty-text",
                if hidden > 0 {
                    format!(
                        "No serious {state} findings in {where_}. Nothing is waiting for a \
                         decision; {hidden} other finding{} minted and not queued.",
                        if hidden == 1 { " is" } else { "s are" }
                    )
                } else {
                    format!("No {state} findings in {where_}. Nothing is waiting for a decision.")
                },
            ));
        } else if matched > 0 {
            attributes.push((
                "count-text",
                count_sentence(matched, drawn, &state, &chosen, scope),
            ));
            if drawn < matched {
                attributes.push(("more", "true".to_string()));
                attributes.push((
                    "more-url",
                    page_url(&format!(
                        "{}&limit=all",
                        query(&state, only.as_deref(), scope)
                    )),
                ));
                attributes.push((
                    "more-rows-url",
                    rows_url(&format!(
                        "{}&limit=all",
                        query(&state, only.as_deref(), scope)
                    )),
                ));
                attributes.push(("more-label", format!("show all {matched}")));
            }
        }
        if !decide && !roots.is_empty() {
            attributes.push(("posture", "read-only".to_string()));
            attributes.push((
                "posture-text",
                format!(
                    "This grant may read findings and not publish them: that needs `{}`. \
                     Nothing gets published to Gonk except by a human who holds it.",
                    ikigai_browse::CAP_ANNOTATE
                ),
            ));
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let doc = envelope("page", &attributes, &children);
        Ok(web::html(
            render::render(&doc, !self.fragment).map_err(web::render_err)?,
        ))
    }

    /// The intray's depth, as one affirmative sentence — see [`crate::trigger::depth`].
    ///
    /// ⚠ Since the trigger can be ARMED ([#466](http://localhost:1060/l/default/item/466))
    /// the count alone is no longer the statement: a queue with twelve waiting and a pass in
    /// flight is working, and a queue with twelve waiting and nothing in flight is a dead
    /// watcher. Both render as "12 waiting" if only the number is printed, and exactly one
    /// of them needs a person. So the trigger's own [`crate::trigger::Status`] writes the
    /// sentence and this only chooses the word the stylesheet colours it by.
    async fn intray_element(&self, inv: &Invocation<'_>) -> String {
        match read_depth(inv).await {
            Ok(status) => element(
                "intray",
                &[("kind", depth_kind(&status))],
                depth_sentence(&status),
            ),
            // ★ **An UNRESOLVED depth is "no queue configured", and it is the manifold
            // saying so rather than this page guessing.** `gonk.review.space` is the whole
            // switch: with no line, `trigger::space` binds nothing, so the absence of the
            // resource IS the absence of the queue. Reading it that way keeps
            // declared = enforced — an unconfigured gonk must not advertise a queue — and
            // leaves the page with one fewer fact of its own to get wrong.
            Err(Error::Unresolved(_) | Error::NotFound(_)) => element(
                "intray",
                &[("kind", "absent")],
                "No review queue is configured (`gonk.review.space`), so nothing is dropping \
                 review requests here.",
            ),
            // ⚠ Anything else is NOT "no queue". This page renders only for callers holding
            // the browse read the depth asks for, so a failure here is a real one rather
            // than a posture — and a depth that cannot be read is the one thing this line
            // must not render as a zero (ledger #446).
            Err(e) => element(
                "intray",
                &[("kind", "error")],
                &format!("The review queue's depth could not be read: {e}"),
            ),
        }
    }

    /// One row.
    fn finding_element(
        &self,
        row: &Value,
        root: &str,
        decide: bool,
        state: &str,
        only: Option<&str>,
        scope: &str,
    ) -> String {
        let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
        let yes = |key: &str| row.get(key).and_then(Value::as_bool).unwrap_or(false);
        let id = text("id");
        let annotates = text("annotates");
        let path = text("path");
        let where_ = match row.get("line").and_then(Value::as_u64) {
            Some(line) => format!("{path}:{line}"),
            None => path.to_string(),
        };
        let proposal = row.get("severity").and_then(Value::as_str);
        let row_state = text("state");

        let mut attributes: Vec<(&str, String)> = vec![
            ("id", id.to_string()),
            ("iri", text("iri").to_string()),
            // The row's own `repo`, falling back to the root this read was ISSUED for — a
            // fact the page knows independently, so a row that omits the field is still
            // attributed rather than rendered under an empty name.
            (
                "repo",
                match text("repo") {
                    "" => root.to_string(),
                    named => named.to_string(),
                },
            ),
            ("where", where_),
            ("state", row_state.to_string()),
            ("severity", proposal.unwrap_or("unrated").to_string()),
            (
                "severity-label",
                match proposal {
                    Some(word) => format!("model: {word}"),
                    None => "model: unrated".to_string(),
                },
            ),
            ("orphaned", flag(yes("orphaned")).to_string()),
            ("reanchored", flag(yes("reanchored")).to_string()),
            ("provenance", provenance(row)),
        ];
        if !annotates.is_empty() {
            attributes.push(("browse-href", browse_url(annotates)));
        }
        // ⚠ **Only when a human actually rated it.** `effective_severity` FALLS BACK to the
        // model's proposal when there is no decision — which is right for a consumer that
        // wants one number, and wrong for a badge labelled "human": rendering it on a pending
        // row would show a rating nobody made, on the one page built to keep the two apart.
        if let (Some(effective), true) = (
            row.get("effective_severity").and_then(Value::as_str),
            row.get("decision").is_some_and(|d| !d.is_null()),
        ) {
            attributes.push(("effective", effective.to_string()));
        }

        let mut children = element("body", &[], text("body"));
        let quote = text("exact");
        if !quote.is_empty() {
            children.push_str(&element("quote", &[], quote));
        }
        // ★ A like claim on this line was already declined (ledger #475; browse 0.9.0 mints
        // the link at mint time and answers it as `prior_decision`). Rendered BEFORE the
        // form, because it is the thing that makes the second decision one click — and
        // visibly a different claim when it is one, which withholding could never show.
        // Nothing here decides anything: the mark is information beside the same form.
        if let Some(prior) = row.get("prior_decision").filter(|p| !p.is_null()) {
            children.push_str(&prior_element(prior));
        }
        if let Some(decision) = row.get("decision").filter(|d| !d.is_null()) {
            children.push_str(&decision_element(decision));
        } else if decide {
            children.push_str(&self.decide_element(id, proposal, state, only, scope));
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        wrap("finding", &attributes, &children)
    }

    /// The decision form for one undecided finding — **built from the contract**.
    ///
    /// ⚠ When either menu is missing from the description, no form is rendered and the card
    /// says why. A hard-coded fallback here would be the fourth copy of the severity set and
    /// would go on offering words the resource had stopped accepting.
    fn decide_element(
        &self,
        id: &str,
        proposal: Option<&str>,
        state: &str,
        only: Option<&str>,
        scope: &str,
    ) -> String {
        let iri = finding_iri(id);
        let (Some(severities), Some(decisions)) = (
            one_of(&self.web.hub, &iri, Verb::Sink, "severity"),
            one_of(&self.web.hub, &iri, Verb::Sink, "decision"),
        ) else {
            return element(
                "no-form",
                &[],
                &format!(
                    "`{iri}` does not describe its `severity` and `decision` menus, so this \
                     page will not invent them. Nothing can be decided here until it does."
                ),
            );
        };
        let mut options = String::new();
        // ⚠ The model may have proposed NOTHING, and the contract says so: `severity` omitted
        // means "accept the proposal", which is an error when there is none. So an unrated
        // finding gets a placeholder that is selected and not submittable — the human must
        // choose, which is what the resource will insist on anyway.
        if proposal.is_none() {
            options.push_str(&element(
                "severity-option",
                &[
                    ("value", ""),
                    ("label", "choose a rating"),
                    ("selected", "true"),
                    ("placeholder", "true"),
                ],
                "",
            ));
        }
        for value in &severities {
            options.push_str(&element(
                "severity-option",
                &[
                    ("value", value),
                    ("label", value),
                    ("selected", flag(proposal == Some(value.as_str()))),
                    ("placeholder", "false"),
                ],
                "",
            ));
        }
        // ★ The decline reason picker (browse 0.10.0): the contract's words, in its order,
        // after an EMPTY option that means "no reason" — browse reads an empty `reason=` as
        // omitted. It hangs off the Decline button, not the form, because a word is Decline's
        // alone; the adapter drops it from any other decision ([`Decide`]). An older browse
        // that declares no `reason` simply gets no picker — the form still decides.
        let reasons = one_of_with_meanings(&self.web.hub, &iri, Verb::Sink, REASON_ARG);
        for value in &decisions {
            let mut picker = String::new();
            if let (Some(reasons), REASONED_DECISION) = (&reasons, value.as_str()) {
                picker.push_str(&element(
                    "reason-option",
                    &[("value", ""), ("label", NO_REASON_LABEL), ("title", "")],
                    "",
                ));
                for (word, meaning) in reasons {
                    picker.push_str(&element(
                        "reason-option",
                        &[
                            ("value", word),
                            ("label", word),
                            ("title", meaning.as_deref().unwrap_or("")),
                        ],
                        "",
                    ));
                }
            }
            options.push_str(&wrap(
                "decision-option",
                &[
                    ("value", value),
                    ("label", decision_label(value)),
                    ("action", DECIDE_PATH),
                    // ⚠ The htmx payload is built HERE because a curly brace cannot appear in
                    // a literal attribute value in the stylesheet's engine at all (they are
                    // attribute-value-template delimiters), and JSON is nothing but curly
                    // braces. The button's own `name`/`value` carries the same word for the
                    // scripting-off path, so both entrances send one decision.
                    ("vals", &format!(r#"{{"decision":"{value}"}}"#)),
                ],
                &picker,
            ));
        }
        wrap(
            "decide",
            &[
                ("action", DECIDE_PATH),
                ("id", id),
                ("state", state),
                ("repo", only.unwrap_or("")),
                ("scope", scope),
                ("rows-url", &rows_url(&query(state, only, scope))),
                ("required", flag(proposal.is_none())),
            ],
            &options,
        )
    }
}

/// The record of a decision already taken, and the asymmetry that follows it.
/// The prior decision on a like claim, as browse 0.9.0 answers it on the row: the declined
/// twin, when, at what rating, the one-word reason (browse 0.10.0) and the human's note if
/// one was typed. ⚠ Both are empty on most rows today (14 of 458 declines carried a note when
/// 0.9.0 shipped, and none can carry a word from before 0.10.0), so the line is built to stand
/// without them: the date and the twin are what make the repeat recognisable.
fn prior_element(prior: &Value) -> String {
    let text = |key: &str| prior.get(key).and_then(Value::as_str).unwrap_or("");
    let twin = text("finding");
    let mut attributes: Vec<(&str, String)> = vec![
        ("twin", twin.to_string()),
        ("twin-id", twin.rsplit(':').next().unwrap_or("").to_string()),
        ("outcome", text("outcome").to_string()),
        ("severity", text("severity").to_string()),
        ("at", web::when(text("decided_at"))),
    ];
    // The one-word reason (browse 0.10.0), when the twin's decline stated one — `null` on
    // every decline made before the word existed, so the line is built to stand without it.
    let reason = text(REASON_ARG);
    if !reason.is_empty() {
        attributes.push((REASON_ARG, reason.to_string()));
    }
    let note = text("note");
    let children = if note.is_empty() {
        String::new()
    } else {
        element("note", &[], note)
    };
    let attributes: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    wrap("prior", &attributes, &children)
}

fn decision_element(decision: &Value) -> String {
    let text = |key: &str| decision.get(key).and_then(Value::as_str).unwrap_or("");
    let outcome = text("outcome");
    let minted = text("minted");
    let mut attributes: Vec<(&str, String)> = vec![
        ("outcome", outcome.to_string()),
        ("severity", text("severity").to_string()),
        ("at", web::when(text("decided_at"))),
        (
            "undo",
            // ★ The asymmetry, said rather than hidden: a publication is undone by deleting
            // the annotation it minted (a separate, visible act under the same capability);
            // a decline is not undone at all, because the decline IS the record.
            match outcome {
                "published" => format!(
                    "Published. Undoing this is a separate act: `delete {minted}` — the \
                     annotation, not the finding."
                ),
                _ => "Declined. The decline is the record: a finding is never deleted, and a \
                      re-run knows a person looked and said no."
                    .to_string(),
            },
        ),
    ];
    let reason = text(REASON_ARG);
    if !reason.is_empty() {
        attributes.push((REASON_ARG, reason.to_string()));
    }
    if !minted.is_empty() {
        attributes.push(("minted", minted.to_string()));
        attributes.push(("minted-href", browse_url(minted)));
    }
    let note = text("note");
    let attributes: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    wrap("decision", &attributes, &element("note", &[], note))
}

/// The wording for one decision value. ⚠ The SET is the contract's; only these words are
/// ours, and a value this table does not know keeps its own word rather than disappearing.
fn decision_label(value: &str) -> &str {
    match value {
        "publish" => "Publish to Gonk",
        REASONED_DECISION => "Decline",
        other => other,
    }
}

/// Where a finding came from, in one line.
fn provenance(row: &Value) -> String {
    let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match text("creator") {
        "" => parts.push("an unnamed reviewer".to_string()),
        who => parts.push(format!("found by {who}")),
    }
    if !text("generated_by").is_empty() {
        parts.push(format!("in pass {}", text("generated_by")));
    }
    if !text("created").is_empty() {
        parts.push(web::when(text("created")));
    }
    if !text("content_hash").is_empty() {
        parts.push(format!("against {}", text("content_hash")));
    }
    parts.join(" · ")
}

// ------------------------------------------------------------------ the live badge

/// Read [`crate::trigger::DEPTH`]'s JSON face through the kernel, under the CALLER's
/// capability.
///
/// ★ **Through the kernel, although this very process holds the counters.** The depth is a
/// resource with a capability floor now
/// ([#464](http://localhost:1060/l/default/item/464) chose that over minting the space's read
/// token), and a page reaching past the floor into `crate::trigger`'s own functions would
/// make the floor decorative: the rendering caller would be shown a number the same caller is
/// refused at the door. It also means the badge and the socket answer from one place.
async fn read_depth(inv: &Invocation<'_>) -> Result<Value> {
    let answer = inv
        .issue(
            Request::new(
                Verb::Source,
                Iri::parse(crate::trigger::DEPTH)
                    .map_err(|e| Error::Endpoint(format!("{}: {e}", crate::trigger::DEPTH)))?,
            )
            .with_arg("as", ArgRef::Inline(JSON.as_bytes().to_vec())),
        )
        .await?;
    serde_json::from_slice(&answer.bytes).map_err(|e| {
        Error::Endpoint(format!(
            "{} did not answer JSON: {e}",
            crate::trigger::DEPTH
        ))
    })
}

/// The sentence that resource wrote — never re-derived here, so the badge, the page and the
/// socket cannot disagree about what a queue is doing.
fn depth_sentence(status: &Value) -> &str {
    status
        .get("sentence")
        .and_then(Value::as_str)
        .unwrap_or("The review queue's depth could not be read.")
}

/// The word the stylesheet colours by. The only thing this face adds to the resource's own
/// answer, because a colour is presentation and a count is not.
///
/// ⚠ **`in_flight` is asked BEFORE the count, and that ordering is a bug fix.** It used to be
/// a guard on the `Some(_)` arm, so a pass running against an EMPTY inbox — which is the
/// ordinary case, because the reactor moves the tuple out of the inbox before it starts the
/// work — rendered as `empty`. The badge was showing "nothing is happening" during the one
/// interval when something was ([#469](http://localhost:1060/l/default/item/469)).
fn depth_kind(status: &Value) -> &'static str {
    let yes = |key: &str| status.get(key).and_then(Value::as_bool).unwrap_or(false);
    if !yes("configured") {
        return "absent";
    }
    if status.get("unreadable").is_some_and(|v| !v.is_null()) {
        return "error";
    }
    if yes("stuck") {
        return "stuck";
    }
    if yes("in_flight") {
        return "working";
    }
    match status.get("waiting").and_then(Value::as_u64) {
        // An empty queue that JUST finished something is a different statement from an empty
        // queue that has been idle for an hour, and only the first one answers "is anything
        // happening". See [`BADGE_RECENT_MS`].
        Some(0) | None => match depth_activity(status) {
            Some(_) => "recent",
            None => "empty",
        },
        Some(_) => "count",
    }
}

/// What the badge draws a spark for: a pass running now, or one that ended inside
/// [`BADGE_RECENT_MS`]. `None` = nothing to say.
///
/// ★ This is the ACTIVITY question, and it is not the depth question. "Is there a backlog"
/// and "is anything happening" have different answers on a fast queue, and the badge was
/// only ever answering the first ([#469](http://localhost:1060/l/default/item/469)). Both
/// answers come out of the same poll of the same resource — the numbers were already in
/// `urn:iki:gonk:review:depth` and were being discarded here.
fn depth_activity(status: &Value) -> Option<&'static str> {
    if status.get("in_flight").and_then(Value::as_bool) == Some(true) {
        return Some("running");
    }
    match status.get("since_last_pass_ms").and_then(Value::as_u64) {
        Some(ms) if ms <= BADGE_RECENT_MS => Some("recent"),
        _ => None,
    }
}

/// The queue's whole observable state as one opaque token: what a poll compares against the
/// poll before it to decide whether there is anything new to show.
///
/// ★ **It is a REVISION, not a timestamp**, so it is stable while nothing moves — which is
/// what makes "refresh the list only on news" possible at all. Every number here is one the
/// depth resource already publishes, and each one changing means a finding may have appeared,
/// been handed to a pass, or been given up on.
///
/// ⚠ **What it cannot see: a finding minted by ANOTHER process.** `passes_*` are this
/// server's own counters ([`crate::trigger::Passes`] — "what this process has spent"), and
/// `waiting`/`handled` are this server's queue. A review run from a second gonk, or by hand
/// through the CLI, writes findings into the same browse graph and moves none of these, so
/// this page will not learn of it until its own queue moves. A revision on the findings
/// resource itself is `ikigai-browse`'s to offer and is reported up rather than guessed at
/// here.
fn depth_rev(status: &Value) -> String {
    let n = |key: &str| match status.get(key).and_then(Value::as_u64) {
        Some(value) => value.to_string(),
        None => "-".to_string(),
    };
    format!(
        "{}.{}.{}.{}.{}",
        n("waiting"),
        n("handled"),
        n("dead_lettered"),
        n("passes_succeeded"),
        n("passes_failed")
    )
}

/// `urn:iki:gonk:fragment:queue-depth` — the nav badge, polled by htmx.
///
/// # ★ Why this exists at all: it is the liveness signal, not decoration
///
/// Brian asked for *"notifications so someone knows there are things in the queue (kind of
/// like the Live indicator in the Web demo)"*. ⚠ That indicator is a **static dot**; what is
/// actually live in web-demo is `hx-trigger="load, every 1s"` on the nav clock, and this is
/// that shape. But the reason is stronger than a nicety: `SpaceReactor::watch` catches up at
/// startup and then lives on a thread, and **a watcher thread that dies while gonk lives
/// drains nothing and says nothing until a restart**. gonk runs no log at all
/// ([#383](http://localhost:1060/l/default/item/383)), so a depth that stops falling is the
/// only symptom there is, and it has to be somewhere a person is already looking.
///
/// A stuck queue looks like this: the badge shows a number, it does not go down, and the
/// Queue page's own line says `NONE IN FLIGHT`. A slow one shows a number that falls and a
/// pass in flight. They are told apart by the second fact, never by the first.
///
/// ⚠ **[`BADGE_EVERY`] is ten seconds, not one.** The clock in web-demo is a clock and has to
/// tick; this is a queue whose entries take a model call each, and every poll here is an
/// XSLT render plus a directory listing. A second would cost this server more than the thing
/// it is watching.
///
/// # ★★ What one poll carries, after [#469](http://localhost:1060/l/default/item/469)
///
/// The first live end-to-end run showed the cadence was not the problem and the RENDERING
/// was: a pass ran in three seconds between two polls, and a badge that draws only a depth
/// had nothing to say about it either time. One poll now answers three questions from the
/// one read it was already doing:
///
/// - **how deep** — `count` and `kind`, as before;
/// - **is anything happening** — `depth_activity`, which survives a pass shorter than the
///   interval because its window IS the interval ([`BADGE_RECENT_MS`]);
/// - **is there anything new to show** — `depth_rev`, which `web/gonk.js` compares against
///   the previous poll's and turns into one [`NEWS_EVENT`] when it changes.
///
/// ★ So the Queue page's findings list refreshes off THIS poll and has no clock of its own:
/// one cadence, owned in Rust, and a list that is re-fetched when there is news rather than
/// every ten seconds regardless.
pub struct Badge {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for Badge {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "the queue badge answers Source, not {:?}",
                inv.request.verb
            )));
        }
        let (kind, mut title, activity, rev) = match read_depth(inv).await {
            Ok(status) => (
                depth_kind(&status),
                depth_sentence(&status).to_string(),
                depth_activity(&status),
                depth_rev(&status),
            ),
            // No `gonk.review.space`: nothing is bound, so there is nothing to show — and
            // nothing is the right answer rather than a zero, which would say "empty queue".
            // The host span collapses on `.queue-badge:empty`.
            Err(Error::Unresolved(_) | Error::NotFound(_)) => return Ok(web::html(String::new())),
            // ⚠ A badge that renders nothing when it cannot read is a badge that looks like
            // an empty queue. It says so instead, in the one character it has room for.
            // ⚠ And NO revision: a depth that cannot be read says nothing about whether the
            // findings list has changed, so the list is left alone. A stale list beside a red
            // badge is honest; a list that re-fetches every ten seconds because the badge is
            // broken is a second failure on top of the first.
            Err(e) => ("error", format!("{e}"), None, String::new()),
        };

        // ★★ THE TWO NUMBERS (ledger #496): how many findings are waiting for a HUMAN, split
        // the way the Queue page splits them — the serious ones it asks about, and the rest
        // it only counts. The request depth (tuples waiting for a PASS) stays in the sentence
        // and in the colour; it is the liveness half and it is unchanged.
        //
        // ⚠ The cost, stated: one findings read per readable root per poll, on top of the
        // depth read. Measured 2026-09-21 over the socket at 40ms for the largest root (156
        // pending rows), so seven roots are well inside a ten-second cadence; a root count
        // an order of magnitude larger is when this wants a count face on the findings
        // resource rather than a row read, which is browse's to offer.
        let counts = pending_counts(&self.web, inv).await;
        // An idle, empty request queue with serious findings waiting is not `empty`: the
        // dim style says "nothing for anyone", and there is something for someone.
        let kind = if kind == "empty" && counts.serious > 0 {
            "count"
        } else {
            kind
        };
        title.push(' ');
        title.push_str(&counts.sentence());
        // ★ The two counts join the revision, and that closes a gap the old one had: a
        // finding minted by ANOTHER process, or decided from another tab, moved nothing in
        // this server's own counters and so never refreshed the list. A pending count is a
        // fact about the store, whoever wrote it.
        let rev = if rev.is_empty() {
            rev
        } else {
            format!("{rev}.{}.{}", counts.serious, counts.other)
        };
        let (count, other) = (counts.serious.to_string(), counts.other.to_string());
        let mut attributes = vec![
            ("view", "queue-badge"),
            ("kind", kind),
            ("count", count.as_str()),
            ("other", other.as_str()),
            ("title", title.as_str()),
        ];
        if let Some(activity) = activity {
            attributes.push(("activity", activity));
        }
        if !rev.is_empty() {
            attributes.push(("rev", rev.as_str()));
        }
        let doc = envelope("page", &attributes, "");
        Ok(web::html(
            render::render(&doc, false).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-queue-badge"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-queue-badge")
            .title("The review queue's depth, for the header")
            .summary(
                "Two numbers, one state word and one revision, polled by the Queue link in \
                 the header. The numbers are what is waiting for a HUMAN: findings in the \
                 serious set `gonk.queue.serious` names (plus any unrated), and the rest — \
                 minted and counted, not queued. The colour, the spark and the tooltip's \
                 first sentence render `urn:iki:gonk:review:depth` unchanged, and that half \
                 is a LIVENESS signal rather than a count: a queue that is armed and not \
                 empty with nothing in flight is a dead watcher, and a pass shorter than \
                 the poll interval still reports itself, because the window the activity is \
                 measured over is the interval itself. The revision is what the Queue \
                 page's findings list refreshes on, so there is one cadence on this server \
                 and not two.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .input(web::as_html_arg())
            .output(web::HTML)
    }
}

/// What the filter MATCHED and what the page DREW, which are different numbers.
fn count_sentence(
    matched: usize,
    drawn: usize,
    state: &str,
    chosen: &[String],
    scope: &str,
) -> String {
    let where_ = match chosen {
        [one] => one.clone(),
        many => format!("{} repositories", many.len()),
    };
    // ⚠ "serious" is the SCOPE's name on the count, not a claim about every row in it: an
    // unrated row and a decided row of any word are listed under it too.
    let serious = if scope == SCOPE_SERIOUS {
        "serious "
    } else {
        ""
    };
    if drawn < matched {
        format!("showing the first {drawn} of {matched} {serious}{state} findings in {where_}")
    } else {
        format!(
            "{matched} {serious}{state} finding{} in {where_}",
            if matched == 1 { "" } else { "s" }
        )
    }
}

fn flag(yes: bool) -> &'static str {
    if yes {
        "true"
    } else {
        "false"
    }
}

/// `state=pending`, `state=pending&repo=x`, `state=pending&severity=all` — the query both
/// URLs carry. The default scope is left OUT, so a URL narrowed to the serious set is the
/// plain one and only the widened page says so.
fn query(state: &str, repo: Option<&str>, scope: &str) -> String {
    let mut out = format!("state={state}");
    if let Some(repo) = repo.filter(|r| !r.is_empty()) {
        out.push_str(&format!("&repo={repo}"));
    }
    if scope != SCOPE_SERIOUS {
        out.push_str(&format!("&{SCOPE_ARG}={scope}"));
    }
    out
}

fn page_url(query: &str) -> String {
    format!("{QUEUE_PATH}?{query}")
}

fn rows_url(query: &str) -> String {
    format!("{ROWS_PATH}?{query}")
}

/// The query a self-refresh repeats: the filter AND the row bound this request was made
/// with, so a refresh shows what the human is already looking at rather than the default.
fn refresh_query(state: &str, repo: Option<&str>, scope: &str, limit: Option<&str>) -> String {
    match limit.map(str::trim).filter(|l| !l.is_empty()) {
        Some(limit) => format!("{}&limit={limit}", query(state, repo, scope)),
        None => query(state, repo, scope),
    }
}

/// A `urn:*` resource's page in gonk's own browse shell.
fn browse_url(iri: &str) -> String {
    format!("{}/{iri}", crate::k::ROOTS_PATH)
}

#[async_trait]
impl Endpoint for QueuePage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(
                "the queue page answers Source only".to_string(),
            ));
        }
        web::html_only(inv)?;
        self.body(inv, &Params::from(inv), None).await
    }

    fn name(&self) -> &str {
        if self.fragment {
            "gonk-fragment-queue"
        } else {
            "gonk-page-queue"
        }
    }

    fn describe(&self) -> Description {
        let id = self.name();
        Description::new(id)
            .title(if self.fragment {
                "The review queue, as a fragment"
            } else {
                "The review queue"
            })
            .summary(
                "Every machine review finding this caller's grant may read, in the triage \
                 order `urn:repo:{repo}:findings` returns them, with the model's proposed \
                 severity, the human's rating once there is one, and — for a caller holding \
                 `urn:cap:annotate` — a publish/decline form whose menus are rendered from \
                 the finding resource's OWN contract rather than from any list held here. \
                 It also shows the git-event trigger's intray depth, which no other face \
                 has, so \"nothing has happened yet\" and \"39 still queued\" are different \
                 sentences. ⚠ A queue, not a gate: nothing in it blocks a commit, a push or \
                 a merge.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("state")
                    .optional()
                    .class(XSD_STRING)
                    .summary(
                        "which part of the pipeline to show — the values \
                         `urn:repo:{repo}:findings` declares (pending, published, declined, \
                         all). Validated against that contract, never against a list here.",
                    )
                    .default_value("pending"),
            )
            .input(
                ArgSpec::new("repo")
                    .optional()
                    .class(XSD_STRING)
                    .summary("one configured browse root; omitted = every root this grant reads"),
            )
            .input(
                ArgSpec::new("limit")
                    .optional()
                    .class(XSD_STRING)
                    .summary(format!(
                        "how many rows to draw: 1–{MAX_ROWS}, or `all` for {MAX_ROWS} \
                         (default {ROWS})"
                    )),
            )
            .input(
                ArgSpec::new(SCOPE_ARG)
                    .optional()
                    .class(XSD_STRING)
                    .one_of([SCOPE_SERIOUS, SCOPE_ALL])
                    .default_value(SCOPE_SERIOUS)
                    .summary(
                        "which rows ask for a decision: `serious` — the set \
                         `gonk.queue.serious` names, plus any unrated row — or `all`. The \
                         rest are minted, anchored and counted either way; they are just not \
                         queued.",
                    ),
            )
            .input(web::as_html_arg())
            .output("text/html")
    }
}

/// `xsd:string`, spelled once.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

// -------------------------------------------------------------------- the decision

/// `urn:iki:gonk:queue:decide` — one human decision, from the queue page's form.
///
/// ★ **It declares `urn:cap:annotate`, and that is not decoration.** Every call it can make
/// is `Sink urn:iki:finding:{id}`, which requires exactly that token — so declaring it here
/// is true, the kernel refuses before dispatch, and `urn:kernel:actions` does not offer the
/// action to a caller who could not complete it. (`crate::k`'s adapter cannot declare a floor
/// because its target is not known until its command is read; this one has a single family.)
pub struct Decide {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for Decide {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Endpoint(format!("`{DECIDE_IRI}` answers Sink only")));
        }
        let mut fields = web::form(inv.inline_str("content")?);
        let mut take = |name: &str| fields.remove(name).filter(|v| !v.trim().is_empty());
        let id = take("id").ok_or_else(|| Error::InvalidArgument {
            name: "id".to_string(),
            detail: "the form names no finding".to_string(),
        })?;
        let state = take("_state").unwrap_or_else(|| "pending".to_string());
        let repo = take("_repo");
        let scope = take("_severity");
        // ⚠ **A reason word travels only with a decline.** The form is ONE form with one
        // picker and two buttons, so a person can pick a word and then press Publish — and
        // browse REFUSES a word beside a publish (a publish reason has no consumer). The word
        // was chosen for the other button; dropping it here is reading the form the way the
        // person used it, where refusing would punish a pick they had already abandoned.
        // An empty `reason=` (the "no reason" option) is dropped by the loop below either way.
        if fields.get("decision").map(|d| d.trim()) != Some(REASONED_DECISION) {
            fields.remove(REASON_ARG);
        }
        let target = finding_iri(&id);
        let target_iri = Iri::parse(&target).map_err(|e| Error::InvalidArgument {
            name: "id".to_string(),
            detail: format!("`{target}`: {e}"),
        })?;

        // ★ The manifold decides what the form may send — the rule `crate::web::Act` states,
        // for the same reason: a dropped `severity=` here would not fail, it would silently
        // record the model's proposal as a human's considered rating.
        let description = self
            .web
            .hub
            .describe(&target_iri)
            .ok_or_else(|| Error::NotFound(format!("nothing is bound at `{target}`")))?;
        let spec = description
            .action_specs()
            .into_iter()
            .find(|spec| spec.verb == Verb::Sink)
            .ok_or_else(|| Error::InvalidArgument {
                name: "id".to_string(),
                detail: format!("`{target}` declares no Sink, so nothing can be decided"),
            })?;
        let mut request = Request::new(Verb::Sink, target_iri);
        for (name, value) in fields {
            if name.starts_with('_') || value.trim().is_empty() {
                continue;
            }
            let declared = spec
                .inputs
                .iter()
                .any(|input| input.name == name && input.source != InputSource::Binding);
            if !declared {
                return Err(Error::InvalidArgument {
                    name,
                    detail: format!(
                        "`{target}` does not declare this input for Sink; a form may send only \
                         what the resource's own contract names"
                    ),
                });
            }
            request = request.with_arg(
                name,
                ArgRef::Inline(value.replace("\r\n", "\n").into_bytes()),
            );
        }

        // ⚠ A refusal is RENDERED, not returned as a status. The two refusals this resource
        // makes are the interesting ones — "a decision is not overwritten, here is what is on
        // file" and the capability denial — and htmx does not swap a non-2xx response, so
        // returning one would make the page silently ignore the most informative answer the
        // system can give. The capability layer has already refused; this decides how the
        // refusal is read.
        let flash = match inv.issue(request).await {
            Ok(answer) => (
                "ok",
                String::from_utf8_lossy(&answer.bytes).trim().to_string(),
            ),
            Err(e) => ("error", format!("{e}")),
        };
        let rows = QueuePage {
            web: Arc::clone(&self.web),
            fragment: true,
        };
        let params = Params {
            state: Some(state),
            repo,
            limit: None,
            scope,
        };
        rows.body(inv, &params, Some((flash.0, flash.1.as_str())))
            .await
    }

    fn name(&self) -> &str {
        "gonk-queue-decide"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-queue-decide")
            .title("Publish or decline one finding")
            .summary(
                "The Queue page's form adapter: an urlencoded body naming `id`, `decision` \
                 and optionally `severity`, `reason` (one word from the finding's own \
                 `one_of`, forwarded ONLY with decision=decline and dropped from any other) and \
                 `content` (the human's free-text note), forwarded to \
                 `urn:iki:finding:{id}`'s Sink under the caller's own capability and answered \
                 with the queue section re-rendered. ⚠ A refusal — a capability denial, or a \
                 second decision that would CHANGE a recorded one — is rendered into that \
                 section rather than returned as a status, because the refusal is the most \
                 informative answer here and a page that drops it teaches people to ignore \
                 refusals. It declares `urn:cap:annotate` because every call it can make \
                 requires it: nothing gets published to Gonk except by a human who holds it.",
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("forward one decision and re-render the queue")
                    .requires(ikigai_browse::CAP_ANNOTATE)
                    .input(ArgSpec::new("content").class(XSD_STRING).summary(
                        "the form: `id`, `decision`, optional `severity`, `reason` (decline \
                                 only) and `content`, plus `_state` and `_repo` for the re-render",
                    ))
                    .output("text/html"),
            )
            .verb(Verb::Meta)
    }
}

#[cfg(test)]
mod tests {
    use super::meaning;

    /// The summary shape `ikigai-browse` builds: prose, then `word: meaning; …` to the end.
    /// ⚠ Made-up words: the real ones are never spelled in this file.
    const SUMMARY: &str = "why, in one word — only with one decision. Omitted = none. \
                           alpha: the first — with a comma, and \"quotes\"; beta-gamma: a \
                           hyphenated word; delta: the last one.";

    #[test]
    fn a_meaning_is_read_from_word_colon_to_the_next_separator() {
        assert_eq!(
            meaning(SUMMARY, "alpha").as_deref(),
            Some("the first — with a comma, and \"quotes\"")
        );
        assert_eq!(
            meaning(SUMMARY, "beta-gamma").as_deref(),
            Some("a hyphenated word")
        );
        assert_eq!(meaning(SUMMARY, "delta").as_deref(), Some("the last one"));
    }

    #[test]
    fn a_word_the_summary_does_not_define_has_no_meaning() {
        assert_eq!(meaning(SUMMARY, "epsilon"), None);
        // `gamma` ends a defined word; it is not itself defined.
        assert_eq!(meaning(SUMMARY, "gamma"), None);
        assert_eq!(meaning("", "alpha"), None);
    }
}
