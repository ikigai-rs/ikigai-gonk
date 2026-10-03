//! The **judge's verdict**, as the Queue routes on it — by ORDERING, and by hiding the
//! refuted ONE CLICK AWAY (ledgers [#696](http://localhost:1060/l/default/item/696) and
//! [#704](http://localhost:1060/l/default/item/704)).
//!
//! `ikigai-browse` (0.16.0 on) runs a JUDGE on each serious finding a review pass mints: a second
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
//! - **Every row says its standing in TEXT**, with the judge's tag (`judge-v2@<model>`), so the
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
//!   (`?`[`refuted_arg`]`=`[`SHOW`], declared on the page's own `describe()`); shown, they
//!   render exactly as before — folded last, with the judge's answers — beside a link back.
//! - **The badge counts what is shown** (serious, not refuted) and its tooltip names how many
//!   it does not count, so the number never drops in silence.
//! - **Nothing is written.** No decision, no store change: the judge is not proof (on the eval
//!   set it refuted 30 of 65 published findings), so a refuted finding stays pending and
//!   decidable, and a later verdict that does not refute brings it back on its own.
//!
//! # ★ The verdict words are the CONTRACT'S (browse 0.16.1, ledger #702 item 1)
//!
//! Every menu on the Queue is read from a resource's own `one_of` (`crate::queue`), and since
//! `ikigai-browse` 0.16.1 the verdict is no exception: the findings Source declares an
//! optional `verdict` filter whose `one_of` is the closed set of verdict words. [`Verdicts`]
//! reads that set from the hub ([`adopt`]); this crate spells none of the words.
//! `tests/judge.rs::no_verdict_word_is_written_down_in_this_crate` holds every page file, and
//! this one, to that.
//!
//! **The ORDER is gonk's policy, expressed over the contract's words by POSITION**: the set's
//! first word leads the Queue, its second is the one folded last and hidden by default, and any
//! further word is ordered with the uncertain ([`Verdicts`]). ⚠ The contract does not STATE
//! those roles: browse's set is "in the order a reader triages them" only in a crate-private
//! doc comment, and core's `ArgSpec` has no per-value metadata to say which word refutes
//! (ledger #708, the same gap the decision words have). So the roles are PINNED BY BEHAVIOR,
//! not by prose: `tests/judge.rs` runs browse's own judge rule over a fake judge's answers and
//! asserts that all-supporting answers yield the first word and a refuting answer the second.
//! A browse release that reordered the set fails that test on the lock bump that adopts it.
//!
//! ⚠ **One set per process, because it is the LINKED crate's.** gonk composes
//! `ikigai-browse` in-process ([`crate::compose_with`]), so every hub this binary builds
//! declares the same set — it is a property of the build, reachable only through a kernel.
//! [`crate::compose_with`] adopts it once, the free functions below read the adopted set
//! ([`words`]), and `main` adopts it again at start through [`adopt`], which REFUSES when the
//! contract declares no set (a browse below the floor) and names [`BROWSE_FLOOR`]. A hub
//! composed without browse adopts nothing; its words are empty, so nothing is ordered, folded
//! or hidden by a verdict — and it serves no Queue to order.
//!
//! ★ That includes the page argument that shows the hidden rows: it is NAMED by the contract's
//! hiding word ([`refuted_arg`]), so it is not a spelling, and every sentence that says the word
//! is built here from the adopted set.
//!
//! # ★ judge-v2 and the CURRENT tag (ledger #483)
//!
//! browse 0.16.1 ships `judge-v2`: a new tag (`judge-v2@<model>`), the words and the shapes
//! unchanged, the v1 verdicts left archived beside it. A verdict is keyed by (finding, tag), so:
//!
//! - **The backfill re-judges under v2.** judge-finding's Exists asks about the CONFIGURED
//!   judge's current tag, so a finding judged only under v1 is unjudged for v2 and a run judges
//!   it again ([`crate::backfill`]; pinned in `tests/judge.rs`).
//! - **The Queue routes on the LATEST verdict, labeled with its tag.** browse orders a row's
//!   `judges` by when each was made, and `judge` is the last, so once a v2 verdict exists it is
//!   the one the row is ordered, folded and hidden by. A row judged only under v1 meanwhile is
//!   ordered by its v1 verdict and SAYS so (`judge: … · judge-v1@<model>`), and the order line
//!   counts the latest verdicts per tag while more than one tag is in play. It is not treated as
//!   unjudged: browse does not publish its current tag (the prompt version is crate-private, and
//!   reading it off an answer would cost a judge call per row), and treating the v1 verdicts on
//!   file as absent would bring back the 680 rows v1 refuted (2026-10-02's count) for the hour a
//!   backfill takes, only to hide most of them again. A label is the honest middle: nothing moves twice,
//!   and every row names the judge it is ordered by.

use std::sync::OnceLock;

use ikigai_core::{Error, Kernel, Result, Verb};
use serde_json::Value;

/// The release whose findings contract first declares the verdict set — what [`adopt`]'s
/// refusal names.
pub const BROWSE_FLOOR: &str = "ikigai-browse 0.16.1";

/// The findings Source's verdict filter — an argument NAME (browse's), never a word.
pub const VERDICT_INPUT: &str = "verdict";

/// The verdict words this server's browse declares, in CONTRACT order, read by role.
///
/// The roles are gonk's policy over that order (see the module docs): the first word is
/// UPHELD (read first), the second REFUTES (folded last, hidden by default), and every
/// further word is UNCERTAIN (after the unjudged). An empty set (no browse composed) gives no
/// word any role.
///
/// ```
/// use ikigai_gonk::verdict::Verdicts;
///
/// let set = Verdicts::declared(vec!["ayes".into(), "nays".into(), "dunno".into()]).unwrap();
/// assert_eq!(set.upheld(), Some("ayes"));
/// assert_eq!(set.refuting(), Some("nays"));
/// assert_eq!(set.uncertain(), ["dunno".to_string()]);
/// assert_eq!(set.triage(), ["ayes", "dunno", "nays"]);
/// assert!(Verdicts::declared(vec!["ayes".into()]).is_err());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdicts {
    words: Vec<String>,
}

impl Verdicts {
    /// A declared set, refused when it is too small to carry the policy's two named roles.
    ///
    /// # Errors
    ///
    /// Fewer than two words: there would be no word to read first and one to fold last.
    pub fn declared(words: Vec<String>) -> std::result::Result<Verdicts, String> {
        if words.len() < 2 {
            return Err(format!(
                "the findings contract declares {} verdict word{} ({}); the Queue's order needs                  at least two — the first leads, the second is folded last and hidden",
                words.len(),
                if words.len() == 1 { "" } else { "s" },
                words.join(", ")
            ));
        }
        Ok(Verdicts { words })
    }

    /// The set `hub` declares on `urn:repo:{root}:findings`'s Source `verdict` input.
    ///
    /// # Errors
    ///
    /// When the input declares no closed set — a browse below [`BROWSE_FLOOR`], or a changed
    /// contract — or a set too small to order by ([`Verdicts::declared`]).
    pub fn read(hub: &Kernel, root: &str) -> std::result::Result<Verdicts, String> {
        let findings = crate::queue::findings_iri(root);
        let Some(words) = crate::queue::one_of(hub, &findings, Verb::Source, VERDICT_INPUT) else {
            return Err(format!(
                "`{findings}` declares no closed `{VERDICT_INPUT}` set for Source. The Queue \
                 orders findings by the judge's verdict and reads the words from that set, \
                 which {BROWSE_FLOOR} is the first release to declare; this server composes \
                 ikigai-browse itself, so a build below that floor (or a changed contract) is \
                 the cause, not a configuration to fix"
            ));
        };
        Verdicts::declared(words)
    }

    /// Every word, in contract order.
    #[must_use]
    pub fn words(&self) -> &[String] {
        &self.words
    }

    /// The word the Queue reads first: the contract's first.
    #[must_use]
    pub fn upheld(&self) -> Option<&str> {
        self.words.first().map(String::as_str)
    }

    /// The word the Queue folds last and hides by default: the contract's second.
    #[must_use]
    pub fn refuting(&self) -> Option<&str> {
        self.words.get(1).map(String::as_str)
    }

    /// Every other word, in contract order: ordered after the unjudged.
    #[must_use]
    pub fn uncertain(&self) -> &[String] {
        self.words.get(2..).unwrap_or_default()
    }

    /// The words in the order a reader triages them on the Queue: upheld, the uncertain,
    /// refuting (the unjudged and the could-not-judge sit between, and have no word).
    #[must_use]
    pub fn triage(&self) -> Vec<&str> {
        self.upheld()
            .into_iter()
            .chain(self.uncertain().iter().map(String::as_str))
            .chain(self.refuting())
            .collect()
    }
}

/// The set this process adopted ([`adopt`]).
static ADOPTED: OnceLock<Verdicts> = OnceLock::new();
/// What [`words`] answers before anything is adopted: no word has a role.
static NONE: Verdicts = Verdicts { words: Vec::new() };

/// ★ Read the set `hub` declares and adopt it for this process — what [`words`] answers from
/// then on. Called by [`crate::compose_with`] on every hub it composes with browse, and by
/// `main` at start, where an `Err` stops the server.
///
/// # Errors
///
/// [`Verdicts::read`]'s, or a set that differs from the one already adopted — which cannot
/// happen while the set is the linked crate's, and would mean two browse builds in one process.
pub fn adopt(hub: &Kernel, root: &str) -> std::result::Result<&'static Verdicts, String> {
    let read = Verdicts::read(hub, root)?;
    let held = ADOPTED.get_or_init(|| read.clone());
    if *held != read {
        return Err(format!(
            "two different verdict sets in one process: {} adopted, then {} — every hub this \
             binary composes links the same ikigai-browse, so this is a bug",
            held.words.join(", "),
            read.words.join(", ")
        ));
    }
    Ok(held)
}

/// The adopted set, or the empty one when no hub with browse has been composed.
#[must_use]
pub fn words() -> &'static Verdicts {
    ADOPTED.get().unwrap_or(&NONE)
}

/// The Queue's argument that shows the rows it hides by default — `?refuted=show` today
/// (ledger #704). ★ NAMED by the contract's refuting word ([`Verdicts::refuting`]), so it is
/// not a spelling: a browse release that renamed the word would rename this argument with it.
/// `None` when no set is adopted, and then the page declares no such argument.
#[must_use]
pub fn refuted_arg() -> Option<&'static str> {
    words().refuting()
}

/// The refuting word in a sentence — only ever called when a row was refuted, so the set
/// is adopted.
fn refuting_word() -> &'static str {
    words().refuting().unwrap_or_default()
}

/// [`refuted_arg`]'s value that shows them, folded last as before.
pub const SHOW: &str = "show";
/// [`refuted_arg`]'s default: hidden, counted, one click away.
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
        Some(other) => {
            let word = refuting_word();
            Err(Error::InvalidArgument {
                name: word.to_string(),
                detail: format!(
                    "`{other}`: `{HIDE}` (the default — an undecided finding the judge {word} \
                     is left out of the Queue and counted) or `{SHOW}` (listed, folded last)"
                ),
            })
        }
    }
}

/// The form field that carries the shown mode through a decision's re-render — `_` and the
/// argument's name, as `_severity` carries `severity`. Built here so no form spells the word.
/// With no set adopted it is a bare `_`, which no form this server draws carries.
#[must_use]
pub fn form_field() -> String {
    format!("_{}", refuted_arg().unwrap_or_default())
}

/// `&refuted=show` when `shown`, else nothing — the default is left out of a URL, as the
/// severity scope's is.
#[must_use]
pub fn query_part(shown: bool) -> String {
    match refuted_arg().filter(|_| shown) {
        Some(arg) => format!("&{arg}={SHOW}"),
        None => String::new(),
    }
}

/// Whether the row's LATEST verdict refutes it — read off `judge` alone, so it needs nothing
/// a caller has not already read (the badge counts it from rows it already holds).
#[must_use]
pub fn refuted(row: &Value) -> bool {
    let word = row
        .get("judge")
        .and_then(|v| v.get("verdict"))
        .and_then(Value::as_str);
    word.is_some() && word == words().refuting()
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
    let word = refuting_word();
    let (what, is) = findings(n);
    let (still, back) = if n == 1 {
        ("it is", "it")
    } else {
        ("they are", "one")
    };
    (
        format!(
            "{what} the judge {word} {is} hidden. Nothing was recorded: {still} still \
             waiting and decidable, and a later verdict that does not refute {back} brings it \
             back on its own."
        ),
        if n == 1 { "show it" } else { "show them" },
    )
}

/// The line above a list that SHOWS `n` such rows on request, and the link back.
#[must_use]
pub fn shown_sentence(n: usize) -> (String, &'static str) {
    let word = refuting_word();
    let (what, is) = findings(n);
    (
        format!(
            "{what} the judge {word} {is} shown, folded last, because this page was \
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
    let word = refuting_word();
    let (what, is) = findings(n);
    format!("{what} the judge {word} {is} waiting, hidden above")
}

/// The badge tooltip's clause for `n` serious findings it does not count.
#[must_use]
pub fn badge_clause(n: usize) -> String {
    let word = refuting_word();
    format!(
        " {n} more serious finding{} the judge {word} {} waiting too, hidden from the \
         Queue and not counted here.",
        if n == 1 { "" } else { "s" },
        if n == 1 { "is" } else { "are" },
    )
}

/// The note on a batch group that lost `n` members to the filter.
#[must_use]
pub fn group_clause(n: usize) -> String {
    let word = refuting_word();
    let (what, is) = findings(n);
    format!("{what} in this group the judge {word} {is} hidden")
}

/// The batch view's line for `n` hidden members, `emptied` of whose groups had no other.
#[must_use]
pub fn batch_sentence(n: usize, emptied: usize) -> (String, &'static str) {
    let word = refuting_word();
    let (what, is) = findings(n);
    let mut out = format!(
        "{what} the judge {word} {is} left out of these groups, so no batch here \
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
    /// The latest verdict is neither — any word of the contract's after its second, or a word
    /// the adopted set does not hold, which is ordered with the uncertain and labeled with its
    /// own word rather than dropped.
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
            let set = words();
            return if set.upheld() == Some(word.as_str()) {
                Standing::Upheld { word, tag }
            } else if set.refuting() == Some(word.as_str()) {
                Standing::Refuted { word, tag }
            } else {
                Standing::Uncertain { word, tag }
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

    /// The tag of the judge whose verdict placed the row — `None` for the two standings that
    /// carry no verdict.
    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        match self {
            Standing::Upheld { tag, .. }
            | Standing::Uncertain { tag, .. }
            | Standing::Refuted { tag, .. } => Some(tag),
            Standing::Unjudged | Standing::Unjudgeable { .. } => None,
        }
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
///
/// ★ While the listed rows' latest verdicts come from MORE THAN ONE judge tag — the judge-v2
/// backfill's hour, when some rows still carry only a judge-v1 verdict — the sentence counts
/// them per tag, so a reader sees how far the re-judging has got.
#[must_use]
pub fn order_sentence(standings: &[Standing], hidden: usize) -> Option<String> {
    let mut counts = [0usize; 5];
    let mut tags: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for standing in standings {
        counts[usize::from(standing.rank())] += 1;
        if let Some(tag) = standing.tag() {
            *tags.entry(tag).or_default() += 1;
        }
    }
    let [upheld, unjudged, uncertain, unjudgeable, refuted] = counts;
    if unjudged == standings.len() && hidden == 0 {
        return None;
    }
    let set = words();
    let first = set.upheld().unwrap_or_default();
    let last = set.refuting().unwrap_or_default();
    let middle = crate::queue::join_or(set.uncertain());
    let mut parts = vec![format!("{upheld} {first} first")];
    parts.push(format!("{unjudged} not yet judged"));
    parts.push(format!("{uncertain} {middle}"));
    if unjudgeable > 0 {
        parts.push(format!("{unjudgeable} the judge could not judge"));
    }
    let by_tag = if tags.len() > 1 {
        format!(
            " The listed rows' latest verdicts come from {} judges: {}.",
            tags.len(),
            tags.iter()
                .map(|(tag, n)| format!("{n} by {tag}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };
    if hidden > 0 {
        parts.push(format!("{hidden} {last}, hidden (see above)"));
        if refuted > 0 {
            // Decided rows: records, never hidden, and never folded.
            parts.push(format!("{refuted} {last} and already decided"));
        }
        return Some(format!(
            "Ordered by the judge's verdict — {}. The judge's answers are on each row.{by_tag}",
            parts.join(", ")
        ));
    }
    parts.push(format!("{refuted} {last}, folded last and still decidable"));
    Some(format!(
        "Ordered by the judge's verdict — {}. Nothing is hidden; the judge's answers are on \
         each row.{by_tag}",
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
