//! The **judge's verdict**, as the Queue routes on it — by ORDERING, and by hiding the
//! refuted ONE CLICK AWAY (ledgers [#696](http://localhost:1060/l/default/item/696) and
//! [#704](http://localhost:1060/l/default/item/704)).
//!
//! `ikigai-browse` 0.16.0 runs a JUDGE on each serious finding a review pass mints: a second
//! call, with the context the reviewer did not have, answering four narrow questions and
//! deriving a verdict from them by rule. The verdict is ATTACHED to the finding row (`judge`,
//! the latest; `judges`, all of them) and browse acts on none of it — routing is the host's,
//! made in the open (browse's `judge` module docs).
//!
//! # ★ Brian's decision (2026-10-02): order, never hide
//!
//! Confirmed first, then the findings no judge has looked at, then the unsure, then the ones a
//! judge could not judge, then the refuted — **collapsed, last, with the judge's answers and
//! reasons, and still decidable.** Nothing leaves the page because of a verdict: the measured
//! judge (ledger #483's comments) refutes most known-false findings and still loses some
//! verified-real ones, so a verdict is a reason to read a row later, never a reason not to.
//!
//! - **Every row says its standing in TEXT**, with the judge's tag (`judge-v1@<model>`), so the
//!   order is never carried by position or color alone ([`Standing::label`]).
//! - **The sort is STABLE.** Within one standing the findings resource's own triage order
//!   (severity rank, then path, then position) is kept exactly: this module adds one key in
//!   front of browse's, it does not replace it.
//! - **The badge's counts do not change.** It counts what is waiting for a decision, and a
//!   refuted finding is still waiting for one.
//! - It routes on `judge` — the LATEST verdict, whatever judge gave it — and names that judge.
//!   With one configured judge (`gonk.review.judge`) that is the configured judge's verdict;
//!   a second judge's verdict sits beside it in `judges` and, being later, is the one shown.
//!
//! # ★ Revised the same day (Brian, 2026-10-02, ledger #704): the refuted are HIDDEN by default
//!
//! After the backfill judged the existing queue (1,330 serious pending: 243 confirmed, 392
//! unsure, 680 refuted, 15 unjudged), Brian asked to suppress the refuted, and chose a
//! READ-SIDE FILTER over a bulk decline. So, for the refuted only:
//!
//! - **The Queue and the batch views leave out an UNDECIDED row whose latest verdict refutes
//!   it** ([`hidden_by_default`]) — the same rows that were folded. A decided row is a record
//!   and is never hidden; an unjudged, unsure or could-not-judge row is never hidden.
//! - **The page says how many it left out, with the link that shows them**
//!   (`?`[`REFUTED_ARG`]`=`[`SHOW`], declared on the page's own `describe()`); shown, they
//!   render exactly as before — folded last, with the judge's answers — beside a link back.
//! - **The badge counts what is shown** (serious, not refuted) and its tooltip names how many
//!   it does not count, so the number never drops in silence.
//! - **Nothing is written.** No decision, no store change: the judge is not proof (on the eval
//!   set it refuted 30 of 65 published findings), so a refuted finding stays pending and
//!   decidable, and a later verdict that does not refute brings it back on its own.
//!
//! # ⚠ The verdict words are spelled HERE, once — the contract declares no closed set
//!
//! Every other menu on the Queue is read from a resource's own `one_of` (`crate::queue`). The
//! verdict has none: `ikigai-browse` 0.16.0 keeps its three words in a crate-private constant
//! and states them only in prose, in the summary of `urn:repo:{repo}:judge:{path}` ("refuted
//! when any answer refutes, confirmed when all support, else unsure"). So the triage order
//! below is the ONE place this crate spells them — the same shape as
//! [`crate::config::DEFAULT_QUEUE_SERIOUS`] for the severity default — and it is CHECKED rather
//! than trusted: `main` runs [`check_verdicts`] before a door opens, and a word that summary no
//! longer states stops this server naming both, instead of ordering every row as if no judge
//! had looked. `tests/judge.rs::no_verdict_word_is_written_down_in_this_crate` holds the page
//! code, the stylesheet and the script to that. A `one_of` on the browse side would retire the
//! copy (reported to the hub).
//!
//! ★ That includes the page argument that shows the hidden rows: it is NAMED by the verdict
//! word ([`REFUTED_ARG`] is the last word of [`triage`]), so it is not a second spelling, and
//! every sentence that says the word is built here.

use ikigai_core::{Error, Iri, Kernel, Result};
use serde_json::Value;

/// The verdict words in the order a reader triages them: the first is read first, the last
/// is folded last. ★ The ONE place this crate spells them — see the module docs.
const TRIAGE: [&str; 3] = ["confirmed", "unsure", "refuted"];

/// The Queue's argument that shows the rows it hides by default — `?refuted=show` (ledger
/// #704). ★ Named BY the verdict word, the last of [`triage`], so the word is still spelled
/// once; a browse release that renamed it would rename this argument with it, after
/// [`check_verdicts`] had stopped the server to say so.
pub const REFUTED_ARG: &str = TRIAGE[TRIAGE.len() - 1];
/// [`REFUTED_ARG`]'s value that shows them, folded last as before.
pub const SHOW: &str = "show";
/// [`REFUTED_ARG`]'s default: hidden, counted, one click away.
pub const HIDE: &str = "hide";

/// Whether a request asked to SEE the hidden rows: `show`, else `hide` (the default).
///
/// # Errors
///
/// Any other value is refused by name rather than read as the default — an argument accepted
/// and then ignored is the failure invisible from the caller's side.
pub fn shown_wanted(asked: Option<&str>) -> Result<bool> {
    match asked.map(str::trim) {
        None | Some("") | Some(HIDE) => Ok(false),
        Some(SHOW) => Ok(true),
        Some(other) => Err(Error::InvalidArgument {
            name: REFUTED_ARG.to_string(),
            detail: format!(
                "`{other}`: `{HIDE}` (the default — an undecided finding the judge {REFUTED_ARG} \
                 is left out of the Queue and counted) or `{SHOW}` (listed, folded last)"
            ),
        }),
    }
}

/// The form field that carries the shown mode through a decision's re-render — `_` and the
/// argument's name, as `_severity` carries `severity`. Built here so no form spells the word.
#[must_use]
pub fn form_field() -> String {
    format!("_{REFUTED_ARG}")
}

/// `&refuted=show` when `shown`, else nothing — the default is left out of a URL, as the
/// severity scope's is.
#[must_use]
pub fn query_part(shown: bool) -> String {
    if shown {
        format!("&{REFUTED_ARG}={SHOW}")
    } else {
        String::new()
    }
}

/// Whether the row's LATEST verdict refutes it — read off `judge` alone, so it needs nothing
/// a caller has not already read (the badge counts it from rows it already holds).
#[must_use]
pub fn refuted(row: &Value) -> bool {
    row.get("judge")
        .and_then(|v| v.get("verdict"))
        .and_then(Value::as_str)
        == Some(REFUTED_ARG)
}

/// How many verdicts the row carries: every judge's (`judges`), else the latest alone
/// (`judge`), else none. What the badge's revision counts so a verdict ARRIVING is news to an
/// open Queue, whatever it says (ledger #702 item 5) — read without naming a single word.
///
/// ```
/// use ikigai_gonk::verdict::verdicts_on;
/// use serde_json::json;
///
/// assert_eq!(verdicts_on(&json!({"judge": null, "judges": []})), 0);
/// assert_eq!(verdicts_on(&json!({"judge": {"tag": "a"}})), 1);
/// assert_eq!(verdicts_on(&json!({"judge": {"tag": "b"}, "judges": [{}, {}]})), 2);
/// ```
#[must_use]
pub fn verdicts_on(row: &Value) -> usize {
    match row.get("judges").and_then(Value::as_array) {
        Some(all) if !all.is_empty() => all.len(),
        _ => usize::from(row.get("judge").is_some_and(Value::is_object)),
    }
}

/// ★ **The rows the Queue leaves out by default** (ledger #704): undecided, and refuted by
/// the latest verdict — exactly the rows [`Standing::folds`]. A decided row is a record and is
/// never hidden; no other standing is ever hidden.
#[must_use]
pub fn hidden_by_default(row: &Value, decided: bool) -> bool {
    !decided && refuted(row)
}

/// `n finding` / `n findings`, and the verb that agrees with it.
fn findings(n: usize) -> (String, &'static str) {
    if n == 1 {
        (format!("{n} undecided finding"), "is")
    } else {
        (format!("{n} undecided findings"), "are")
    }
}

/// The line above a list that left `n` rows out, and its link's label.
#[must_use]
pub fn hidden_sentence(n: usize) -> (String, &'static str) {
    let (what, is) = findings(n);
    let (still, back) = if n == 1 {
        ("it is", "it")
    } else {
        ("they are", "one")
    };
    (
        format!(
            "{what} the judge {REFUTED_ARG} {is} hidden. Nothing was recorded: {still} still \
             waiting and decidable, and a later verdict that does not refute {back} brings it \
             back on its own."
        ),
        if n == 1 { "show it" } else { "show them" },
    )
}

/// The line above a list that SHOWS `n` such rows on request, and the link back.
#[must_use]
pub fn shown_sentence(n: usize) -> (String, &'static str) {
    let (what, is) = findings(n);
    (
        format!(
            "{what} the judge {REFUTED_ARG} {is} shown, folded last, because this page was \
             asked to show {}.",
            if n == 1 { "it" } else { "them" }
        ),
        if n == 1 { "hide it" } else { "hide them" },
    )
}

/// The clause an empty list adds when every row it had was left out — "nothing is waiting"
/// would be false.
#[must_use]
pub fn empty_clause(n: usize) -> String {
    let (what, is) = findings(n);
    format!("{what} the judge {REFUTED_ARG} {is} waiting, hidden above")
}

/// The badge tooltip's clause for `n` serious findings it does not count.
#[must_use]
pub fn badge_clause(n: usize) -> String {
    format!(
        " {n} more serious finding{} the judge {REFUTED_ARG} {} waiting too, hidden from the \
         Queue and not counted here.",
        if n == 1 { "" } else { "s" },
        if n == 1 { "is" } else { "are" },
    )
}

/// The note on a batch group that lost `n` members to the filter.
#[must_use]
pub fn group_clause(n: usize) -> String {
    let (what, is) = findings(n);
    format!("{what} in this group the judge {REFUTED_ARG} {is} hidden")
}

/// The batch view's line for `n` hidden members, `emptied` of whose groups had no other.
#[must_use]
pub fn batch_sentence(n: usize, emptied: usize) -> (String, &'static str) {
    let (what, is) = findings(n);
    let mut out = format!(
        "{what} the judge {REFUTED_ARG} {is} left out of these groups, so no batch here \
         declines {}.",
        if n == 1 { "it" } else { "them" }
    );
    if emptied > 0 {
        out.push_str(&format!(
            " {emptied} group{} had no other member and {} not shown.",
            if emptied == 1 { "" } else { "s" },
            if emptied == 1 { "is" } else { "are" },
        ));
    }
    out.push_str(" Nothing was recorded.");
    (out, if n == 1 { "show it" } else { "show them" })
}

/// Where a row stands with the judge — the first sort key on the Queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// The latest verdict confirms the claim.
    Upheld { word: String, tag: String },
    /// No judge has looked at this finding.
    Unjudged,
    /// The latest verdict is neither — or is a word this build does not know, which is ordered
    /// with the uncertain and labeled with its own word rather than dropped.
    Uncertain { word: String, tag: String },
    /// A judge was asked and could not judge it (`status: cannot` from
    /// `urn:repo:{repo}:judge-finding:{id}`), with the reason it gave.
    Unjudgeable { reason: String },
    /// The latest verdict refutes the claim: folded, last, still decidable.
    Refuted { word: String, tag: String },
}

impl Standing {
    /// One row's standing: its `judge` verdict, else what `cannot` says a judge could not do
    /// with it, else unjudged.
    pub fn of(row: &Value, cannot: &dyn Fn(&str) -> Option<String>) -> Standing {
        let verdict = row.get("judge").filter(|v| v.is_object());
        if let Some(verdict) = verdict {
            let text = |key: &str| {
                verdict
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let (word, tag) = (text("verdict"), text("tag"));
            return match TRIAGE.iter().position(|w| *w == word) {
                Some(0) => Standing::Upheld { word, tag },
                Some(last) if last == TRIAGE.len() - 1 => Standing::Refuted { word, tag },
                _ => Standing::Uncertain { word, tag },
            };
        }
        let id = row.get("id").and_then(Value::as_str).unwrap_or("");
        match cannot(id) {
            Some(reason) => Standing::Unjudgeable { reason },
            None => Standing::Unjudged,
        }
    }

    /// The sort key: lower is read first.
    #[must_use]
    pub fn rank(&self) -> u8 {
        match self {
            Standing::Upheld { .. } => 0,
            Standing::Unjudged => 1,
            Standing::Uncertain { .. } => 2,
            Standing::Unjudgeable { .. } => 3,
            Standing::Refuted { .. } => 4,
        }
    }

    /// Whether the row is drawn folded — refuted, and only while it still waits for a
    /// decision (a decided row is a record, and a record is never folded away).
    #[must_use]
    pub fn folds(&self, decided: bool) -> bool {
        !decided && matches!(self, Standing::Refuted { .. })
    }

    /// The row's standing in words, with the judge's tag — what every row carries, so the
    /// order is never said by position or color alone.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Standing::Upheld { word, tag }
            | Standing::Uncertain { word, tag }
            | Standing::Refuted { word, tag } => format!("judge: {word} · {tag}"),
            Standing::Unjudged => "judge: not yet judged".to_string(),
            Standing::Unjudgeable { reason } => format!("judge: could not judge — {reason}"),
        }
    }

    /// The word a stylesheet may key emphasis on (a class; never the only signal): the
    /// verdict's own word, or a word for the two standings that have none.
    #[must_use]
    pub fn class(&self) -> &str {
        match self {
            Standing::Upheld { word, .. }
            | Standing::Uncertain { word, .. }
            | Standing::Refuted { word, .. } => word,
            Standing::Unjudged => "unjudged",
            Standing::Unjudgeable { .. } => "unjudgeable",
        }
    }
}

/// Order `rows` by [`Standing`], STABLY: within one standing the order they arrived in (the
/// findings resource's triage order) is kept. `key` picks the row out of each item.
pub fn order<T>(
    rows: &mut [T],
    key: impl Fn(&T) -> &Value,
    cannot: &dyn Fn(&str) -> Option<String>,
) {
    rows.sort_by_cached_key(|item| Standing::of(key(item), cannot).rank());
}

/// How many rows stand where, after [`order`] — the sentence the page prints above them, so a
/// reader knows the list is ordered and by what. `hidden` is how many rows the default left
/// out ([`hidden_by_default`]); they are named in the sentence, never counted as absent.
/// `None` when no row has been judged, none was refused and none was hidden, because then the
/// order is browse's alone and saying otherwise would be noise.
#[must_use]
pub fn order_sentence(standings: &[Standing], hidden: usize) -> Option<String> {
    let mut counts = [0usize; 5];
    for standing in standings {
        counts[usize::from(standing.rank())] += 1;
    }
    let [upheld, unjudged, uncertain, unjudgeable, refuted] = counts;
    if unjudged == standings.len() && hidden == 0 {
        return None;
    }
    let [first, middle, last] = TRIAGE;
    let mut parts = vec![format!("{upheld} {first} first")];
    parts.push(format!("{unjudged} not yet judged"));
    parts.push(format!("{uncertain} {middle}"));
    if unjudgeable > 0 {
        parts.push(format!("{unjudgeable} the judge could not judge"));
    }
    if hidden > 0 {
        parts.push(format!("{hidden} {last}, hidden (see above)"));
        if refuted > 0 {
            // Decided rows: records, never hidden, and never folded.
            parts.push(format!("{refuted} {last} and already decided"));
        }
        return Some(format!(
            "Ordered by the judge's verdict — {}. The judge's answers are on each row.",
            parts.join(", ")
        ));
    }
    parts.push(format!("{refuted} {last}, folded last and still decidable"));
    Some(format!(
        "Ordered by the judge's verdict — {}. Nothing is hidden; the judge's answers are on \
         each row.",
        parts.join(", ")
    ))
}

/// The judge's four answers on one row, as view elements — `<answer question=… answer=…>
/// reason</answer>` in the order the verdict carries them. The question names are the
/// verdict's own keys, never spelled here. Empty when the row has no verdict. `open` draws
/// them unfolded — inside a folded refuted row, where opening the fold is asking for them.
#[must_use]
pub fn answers_element(row: &Value, open: bool) -> String {
    let Some(verdict) = row.get("judge").filter(|v| v.is_object()) else {
        return String::new();
    };
    let mut children = String::new();
    if let Some(Value::Object(answers)) = verdict.get("answers") {
        for (question, answer) in answers {
            let said = answer.get("answer").and_then(Value::as_str).unwrap_or("");
            let reason = answer.get("reason").and_then(Value::as_str).unwrap_or("");
            children.push_str(&crate::render::element(
                "answer",
                &[("question", question), ("answer", said)],
                reason,
            ));
        }
    }
    let text = |key: &str| verdict.get(key).and_then(Value::as_str).unwrap_or("");
    let at = crate::web::when(text("judged_at"));
    crate::render::wrap(
        "judge",
        &[
            ("tag", text("tag")),
            ("at", &at),
            ("open", crate::queue::flag(open)),
            (
                "test-code",
                crate::queue::flag(verdict.get("test_code").and_then(Value::as_bool) == Some(true)),
            ),
        ],
        &children,
    )
}

/// An IRI to read the judge's CONTRACT through — `urn:repo:{root}:judge:{path}`, whose summary
/// is the one place browse states its verdict words. The description is the template's, so the
/// path names no file and nothing is read.
fn judge_probe(root: &str) -> String {
    format!("urn:repo:{root}:judge:{}", crate::queue::PROBE_ID)
}

/// ★ **The triage order checked against the judge's own contract**, before a door opens.
///
/// Every word of [`triage`] must appear, as a whole word, in the summary of
/// `urn:repo:{root}:judge:{path}`. `Ok(None)` when no judge is bound (no explain families:
/// no mount, so no `urn:llm:*`), because then there is no contract to check and no pass can
/// mint a verdict; `Ok(Some(words))` when every word was found.
///
/// # Errors
///
/// When the judge is bound and its summary does not state a word — a browse release that
/// renamed a verdict, which would otherwise order every judged row as uncertain in silence.
pub fn check_verdicts(
    hub: &Kernel,
    root: &str,
) -> std::result::Result<Option<Vec<&'static str>>, String> {
    let probe = judge_probe(root);
    let target = Iri::parse(&probe).map_err(|e| format!("`{probe}`: {e}"))?;
    let Some(description) = hub.describe(&target) else {
        return Ok(None);
    };
    let summary = description.summary;
    let stated = |word: &str| {
        summary
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| token == word)
    };
    let missing: Vec<&str> = TRIAGE.iter().copied().filter(|w| !stated(w)).collect();
    if missing.is_empty() {
        return Ok(Some(TRIAGE.to_vec()));
    }
    Err(format!(
        "the Queue orders findings by the judge's verdict in the order {}, and `{probe}`'s \
         contract no longer states {}. ikigai-browse declares no closed verdict set, so this \
         build spells the three words in src/verdict.rs and checks them here; a browse release \
         that renamed one would leave every judged row ordered as uncertain. Its summary \
         says: {summary}",
        TRIAGE.join(", "),
        missing.join(", ")
    ))
}

/// The triage order, for a test or a banner — the same words [`check_verdicts`] checks.
#[must_use]
pub fn triage() -> [&'static str; 3] {
    TRIAGE
}
