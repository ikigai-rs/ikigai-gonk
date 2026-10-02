//! The **judge's verdict**, as the Queue routes on it — by ORDERING, never by hiding
//! (ledger [#696](http://localhost:1060/l/default/item/696)).
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

use ikigai_core::{Iri, Kernel};
use serde_json::Value;

/// The verdict words in the order a reader triages them: the first is read first, the last
/// is folded last. ★ The ONE place this crate spells them — see the module docs.
const TRIAGE: [&str; 3] = ["confirmed", "unsure", "refuted"];

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
/// reader knows the list is ordered and by what. `None` when no row has been judged and none
/// was refused, because then the order is browse's alone and saying otherwise would be noise.
#[must_use]
pub fn order_sentence(standings: &[Standing]) -> Option<String> {
    let mut counts = [0usize; 5];
    for standing in standings {
        counts[usize::from(standing.rank())] += 1;
    }
    let [upheld, unjudged, uncertain, unjudgeable, refuted] = counts;
    if unjudged == standings.len() {
        return None;
    }
    let [first, middle, last] = TRIAGE;
    let mut parts = vec![format!("{upheld} {first} first")];
    parts.push(format!("{unjudged} not yet judged"));
    parts.push(format!("{uncertain} {middle}"));
    if unjudgeable > 0 {
        parts.push(format!("{unjudgeable} the judge could not judge"));
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
pub fn check_verdicts(hub: &Kernel, root: &str) -> Result<Option<Vec<&'static str>>, String> {
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
