//! The Queue's **batch view** — a machine-proposed group of pending findings, decided once by
//! the human (ledger [#506](http://localhost:1060/l/default/item/506)).
//!
//! ```text
//! /queue?group=<kind>        urn:iki:gonk:page:queue       Source  the page, one kind at a time
//! /queue/rows?group=<kind>   urn:iki:gonk:fragment:queue   Source  the same section, for htmx
//! /queue/batch               urn:iki:gonk:queue:batch      Sink    one batch decline, fanned out
//! ```
//!
//! `ikigai-browse` 0.12.0 PROPOSES the groups: `urn:repo:{repo}:findings group=<kind>` answers
//! the pending set as groups, each with a suggested reason word, and decides nothing. This is
//! the face that lets a person decide one. There is **no batch Sink** anywhere: a batch is one
//! call per ticked member to the existing finding Sink, so every member gets an ordinary
//! decision node, and the recurrence mark on a later like claim reads a batch decline exactly
//! like a single one — because it IS the human's decision, made once for many.
//!
//! # ★ The rules, each one a decision already made
//!
//! - **Members are SHOWN before deciding, and each can be unticked.** Nothing is pre-decided:
//!   a box ticked on the page is an offer, and nothing reaches the store until the button.
//! - **A batch DECLINE requires a reason word** — the finding Sink's own `reason` `one_of`.
//!   The group's suggested word is PRE-SELECTED in the picker, never applied without the
//!   press. A batch with no word is refused whole, before any member is touched: a bulk
//!   judgment without content is exactly what ledger #506 exists to prevent.
//! - ★ **The twin-carrying kind is the one exception** (Brian, 2026-09-25). Its groups are
//!   almost all singletons — one re-raise per declined twin — so "decide once for many" only
//!   works as ALL of its groups in one form, each member declined with ITS OWN twin's word.
//!   Every member still gets a reason, and it is the human's own earlier word for the same
//!   claim on the same line. A twin with no word leaves that member's picker empty, and the
//!   batch is refused until one is picked or the member is unticked. ⚠ The mode is chosen by
//!   the DATA — a view whose groups carry a `twin` — never by the kind's name, which this
//!   crate does not spell.
//! - **Severity** (Brian, 2026-09-25): under the default `severity=serious` scope the members
//!   are filtered to `gonk.queue.serious` exactly as the rows are (`crate::queue::rated`,
//!   [`crate::config::QueuePolicy::queues`]), and the counts shown are GONK's — browse's
//!   `kinds` counts span every severity and would not match what the page lists. What the
//!   filter left out is said, with the link that lists it (`severity=all`).
//! - **One proposal is shown once.** For a doc file browse's comment-shaped group and its
//!   whole-file group hold the same members, because every line of a doc is "a comment". A
//!   twin-less, keep-less group whose members equal a LATER kind's group on the same target
//!   is shown under that later kind only (`fold_repeats`) — the rule stated over the data,
//!   so no kind word and no extension list is written here.
//! - **Publish is not batchable.** A publish mints an annotation on each line, and a bulk
//!   publish is the one act here that writes to what other readers see. The view offers
//!   Decline only, and [`Batch`] refuses any other decision by name.
//! - The group that proposes a row to KEEP shows it above the members, unticked and locked.
//!
//! # ⚠ A decline needs `urn:cap:annotate` — the brief said otherwise
//!
//! The dispatch brief assumed a decline needs no `urn:cap:annotate`. The finding Sink's
//! contract says it does: its one Sink action requires that token and the browse read
//! wildcard, for either decision ("declining writes the record that a holder of that
//! authority made"). So [`Batch`] declares both — declared = enforced — and the kernel refuses
//! a caller without them before a single member is tried.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Description, Endpoint, Error, Invocation, Iri, Representation,
    Request, Result, Verb,
};
use serde_json::Value;

use crate::queue::{
    self, browse_url, can_decide, finding_iri, findings_iri, flag, one_of, one_of_with_meanings,
    page_url, rows_url, QueuePage, REASONED_DECISION, REASON_ARG, SCOPE_ALL, SCOPE_ARG,
    SCOPE_SERIOUS,
};
use crate::render::{self, element, envelope, wrap};
use crate::web::{self, Web};

/// The findings face's grouping argument (browse 0.12.0). Its WORDS are the contract's
/// `one_of`; only the argument's name is ours.
pub const GROUP_ARG: &str = "group";
/// `urn:iki:gonk:queue:batch` — one batch decline, fanned out to the finding Sink.
pub const BATCH_IRI: &str = "urn:iki:gonk:queue:batch";
/// Where the batch form posts.
pub const BATCH_PATH: &str = "/queue/batch";

/// The checkbox name every member shares: `member=<id>`, once per ticked box.
const MEMBER_FIELD: &str = "member";
/// A member's OWN reason word, in the twin-carrying view: `reason:<id>=<word>`.
const MEMBER_REASON_PREFIX: &str = "reason:";

/// The picker's empty option when a word must still be chosen — selected, not submittable.
const CHOOSE_REASON_LABEL: &str = "choose a reason";

/// The `group=` a request asked for, checked against the contract's own set — `None` for the
/// ordinary rows view.
///
/// ⚠ **Refused, never ignored**, in the three ways browse refuses it too: a kind the contract
/// does not declare, a `state` other than pending (a group proposes PENDING findings only),
/// and a browse that declares no groups at all (0.11.x). An argument accepted and then
/// ignored is the failure invisible from the caller's side.
pub(crate) fn kind_wanted(
    asked: Option<&str>,
    kinds: Option<&[String]>,
    state: &str,
) -> Result<Option<String>> {
    let Some(asked) = asked.map(str::trim).filter(|a| !a.is_empty()) else {
        return Ok(None);
    };
    let Some(kinds) = kinds else {
        return Err(Error::InvalidArgument {
            name: GROUP_ARG.to_string(),
            detail: format!(
                "`{asked}`: the findings resource declares no `{GROUP_ARG}` set, so there is no \
                 batch view to show — it needs ikigai-browse 0.12.0 or later"
            ),
        });
    };
    if !kinds.iter().any(|k| k == asked) {
        return Err(Error::InvalidArgument {
            name: GROUP_ARG.to_string(),
            detail: format!("`{asked}` is not one of {}", kinds.join(", ")),
        });
    }
    if state != "pending" {
        return Err(Error::InvalidArgument {
            name: "state".to_string(),
            detail: format!(
                "`{GROUP_ARG}={asked}` proposes groups of PENDING findings only — drop \
                 `state={state}`, or drop `{GROUP_ARG}`"
            ),
        });
    }
    Ok(Some(asked.to_string()))
}

/// `group=<kind>`, `…&repo=x`, `…&severity=all` — the batch view's query.
pub(crate) fn group_query(kind: &str, repo: Option<&str>, scope: &str) -> String {
    let mut out = format!("{GROUP_ARG}={kind}");
    if let Some(repo) = repo.filter(|r| !r.is_empty()) {
        out.push_str(&format!("&repo={repo}"));
    }
    if scope != SCOPE_SERIOUS {
        out.push_str(&format!("&{SCOPE_ARG}={scope}"));
    }
    out
}

/// The kind nav, one entry per word of the contract's `group` set, in contract order.
///
/// ⚠ No count on the entries. Browse's `kinds` counts span every severity and would not match
/// what a serious-scoped view lists, and gonk's own counts would cost a grouped read per kind
/// per root on every page; the count is on the view itself, where it is gonk's.
pub(crate) fn kind_nav(
    kinds: &[String],
    current: Option<&str>,
    only: Option<&str>,
    scope: &str,
) -> String {
    kinds
        .iter()
        .map(|kind| {
            let query = group_query(kind, only, scope);
            element(
                "kind",
                &[
                    ("name", kind),
                    ("href", &page_url(&query)),
                    ("rows-url", &rows_url(&query)),
                    ("current", flag(current == Some(kind.as_str()))),
                ],
                "",
            )
        })
        .collect()
}

/// What the batch view is rendered from — everything [`QueuePage::body`] already settled.
pub(crate) struct Frame<'a> {
    pub(crate) kind: &'a str,
    pub(crate) kinds: &'a [String],
    pub(crate) states: Option<&'a [String]>,
    pub(crate) only: Option<&'a str>,
    pub(crate) scope: &'static str,
    pub(crate) chosen: &'a [String],
    pub(crate) no_roots: bool,
    pub(crate) flash: Option<(&'a str, &'a str)>,
}

/// One root's groups of one kind, as the findings face's JSON answers them — `groups` only;
/// the `kinds` counts are browse's and are not shown (see [`kind_nav`]).
async fn read_groups(
    inv: &Invocation<'_>,
    root: &str,
    kind: &str,
) -> std::result::Result<Vec<Value>, String> {
    let iri = findings_iri(root);
    let target = Iri::parse(&iri).map_err(|_| format!("`{iri}` is not an IRI"))?;
    let request = Request::new(Verb::Source, target)
        .with_arg("as", ArgRef::Inline(queue::JSON.as_bytes().to_vec()))
        .with_arg(GROUP_ARG, ArgRef::Inline(kind.as_bytes().to_vec()));
    let answer = inv.issue(request).await.map_err(|e| format!("{e}"))?;
    match serde_json::from_slice::<Value>(&answer.bytes) {
        Ok(Value::Object(mut body)) => match body.remove("groups") {
            Some(Value::Array(groups)) => Ok(groups),
            _ => Err(format!(
                "`{iri}` {GROUP_ARG}={kind} answered no `groups` array"
            )),
        },
        _ => Err(format!(
            "`{iri}` {GROUP_ARG}={kind} answered something that is not a JSON object"
        )),
    }
}

fn is_null_or_absent(group: &Value, key: &str) -> bool {
    group.get(key).is_none_or(Value::is_null)
}

/// A group browse proposes by TARGET alone: no declined twin rides along and no row is
/// proposed to keep. Only such a group can repeat another kind's proposal.
fn by_target_only(group: &Value) -> bool {
    is_null_or_absent(group, "twin") && is_null_or_absent(group, "kept")
}

/// The identity of a proposal: its target and the ids of its members.
fn proposal(group: &Value) -> (String, BTreeSet<String>) {
    let target = group
        .get("annotates")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let members = members(group)
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    (target, members)
}

fn members(group: &Value) -> &[Value] {
    group
        .get("members")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// ★ **One proposal, shown once.** For a doc file every quote is "a comment or doc line", so
/// browse's comment-shaped group on it holds exactly the members of its whole-file group, and
/// showing both asks the same question twice. The rule, over the data rather than a name: a
/// group proposed by target alone ([`by_target_only`]) whose target and members equal those
/// of a group of a LATER kind in the contract's order is dropped here and shown there. Returns
/// how many were folded, so the page can say so.
///
/// ⚠ It reads the later kinds only when there is something to fold, so a view whose groups
/// all carry a twin or a kept row pays nothing.
async fn fold_repeats(
    inv: &Invocation<'_>,
    root: &str,
    later: &[String],
    groups: &mut Vec<Value>,
) -> usize {
    if later.is_empty() || !groups.iter().any(by_target_only) {
        return 0;
    }
    let mut elsewhere: BTreeSet<(String, BTreeSet<String>)> = BTreeSet::new();
    for kind in later {
        if let Ok(theirs) = read_groups(inv, root, kind).await {
            elsewhere.extend(theirs.iter().filter(|g| by_target_only(g)).map(proposal));
        }
    }
    let before = groups.len();
    groups.retain(|g| !(by_target_only(g) && elsewhere.contains(&proposal(g))));
    before - groups.len()
}

/// `n thing` / `n things`.
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The batch view: the queue section, with one kind's groups in place of the rows.
pub(crate) async fn section(
    page: &QueuePage,
    inv: &Invocation<'_>,
    frame: Frame<'_>,
) -> Result<Representation> {
    let web = &page.web;
    let policy = &web.queue;
    let decide = can_decide(inv);
    let Frame {
        kind,
        kinds,
        states,
        only,
        scope,
        chosen,
        no_roots,
        flash,
    } = frame;
    let later: Vec<String> = kinds
        .iter()
        .skip_while(|k| k.as_str() != kind)
        .skip(1)
        .cloned()
        .collect();

    // The reads, one grouped read per chosen root (and the later kinds' only to fold).
    let mut read: Vec<(String, std::result::Result<Vec<Value>, String>)> = Vec::new();
    let mut folded = 0usize;
    for root in chosen {
        let mut answer = read_groups(inv, root, kind).await;
        if let Ok(groups) = &mut answer {
            folded += fold_repeats(inv, root, &later, groups).await;
        }
        read.push((root.clone(), answer));
    }

    // ★★ THE GATE, as on the rows (ledger #496): under the serious scope a member is shown
    // only when the Queue would ask about it. A group left with no member is not a proposal.
    let mut hidden = 0usize;
    for (_, answer) in &mut read {
        if let Ok(groups) = answer {
            for group in groups.iter_mut() {
                if scope != SCOPE_SERIOUS {
                    continue;
                }
                if let Some(Value::Array(members)) = group.get_mut("members") {
                    let before = members.len();
                    members.retain(|row| policy.queues(queue::rated(row)));
                    hidden += before - members.len();
                }
            }
            groups.retain(|group| !members(group).is_empty());
        }
    }
    let groups: Vec<&Value> = read
        .iter()
        .filter_map(|(_, answer)| answer.as_ref().ok())
        .flatten()
        .collect();
    let shown: usize = groups.iter().map(|g| members(g).len()).sum();
    // ★ The mode is the DATA's: a view whose groups carry a declined twin decides each
    // member with its own twin's word, all groups in one form.
    let per_twin = groups.iter().any(|g| !is_null_or_absent(g, "twin"));

    // The contract's reason words — the picker's options and the only words a batch may
    // carry. `None` when the contract cannot be read: no form, never an invented menu.
    let reasons = one_of_with_meanings(
        &web.hub,
        &finding_iri(queue::PROBE_ID),
        Verb::Sink,
        REASON_ARG,
    );

    // ---- the header: the same navs as the rows view, with this kind current
    let mut children = web::nav(web, inv, &web::readable_ledgers(web, inv), None);
    if let Some((flash_kind, text)) = flash {
        children.push_str(&element("flash", &[("kind", flash_kind)], text));
    }
    children.push_str(&page.intray_element(inv).await);
    if let Some(known) = states {
        for name in known {
            let q = queue::query(name, only, scope);
            children.push_str(&element(
                "state",
                &[
                    ("name", name),
                    ("href", &page_url(&q)),
                    ("rows-url", &rows_url(&q)),
                    ("current", "false"),
                ],
                "",
            ));
        }
    }
    let serious_label = format!("serious: {}", policy.serious.join(", "));
    for (name, label) in [
        (SCOPE_SERIOUS, serious_label.as_str()),
        (SCOPE_ALL, "all severities"),
    ] {
        let q = group_query(kind, only, name);
        children.push_str(&element(
            "scope",
            &[
                ("name", name),
                ("label", label),
                ("href", &page_url(&q)),
                ("rows-url", &rows_url(&q)),
                ("current", flag(name == scope)),
            ],
            "",
        ));
    }
    children.push_str(&kind_nav(kinds, Some(kind), only, scope));
    for (root, answer) in &read {
        if let Err(why) = answer {
            children.push_str(&element("denied", &[("repo", root)], why));
        }
    }
    let where_ = match chosen {
        [one] => one.clone(),
        many => format!("{} repositories", many.len()),
    };
    if hidden > 0 {
        let all = group_query(kind, only, SCOPE_ALL);
        let others = queue::proposal_words(&web.hub, policy);
        let rated = match &others {
            Some(words) if !words.is_empty() => format!(" rated {}", queue::join_or(words)),
            _ => String::new(),
        };
        children.push_str(&element(
            "hidden",
            &[
                ("count", &hidden.to_string()),
                ("href", &page_url(&all)),
                ("rows-url", &rows_url(&all)),
                ("label", "list all severities"),
            ],
            &format!(
                "{} in these {kind} groups{rated} {} left out: the Queue does not ask about \
                 {}.",
                plural(hidden, "other finding", "other findings"),
                if hidden == 1 { "is" } else { "are" },
                if hidden == 1 { "it" } else { "them" },
            ),
        ));
    }
    if folded > 0 {
        children.push_str(&element(
            "folded",
            &[],
            &format!(
                "{} held exactly the members of a later kind's group on the same file, so {} \
                 shown there, once.",
                plural(folded, &format!("{kind} group"), &format!("{kind} groups")),
                if folded == 1 { "it is" } else { "they are" },
            ),
        ));
    }

    // ---- the groups
    let form_attributes = |key: &str| -> Vec<(&'static str, String)> {
        vec![
            ("action", BATCH_PATH.to_string()),
            ("key", key.to_string()),
            ("group", kind.to_string()),
            ("repo", only.unwrap_or("").to_string()),
            ("scope", scope.to_string()),
            ("decide", flag(decide && reasons.is_some()).to_string()),
            ("per-twin", flag(per_twin).to_string()),
            // The single-row Decline button's own class, so the batch button reads as the
            // same act — the decision word is the wording table's, not spelled in the sheet.
            ("button-class", format!("decide-button {REASONED_DECISION}")),
        ]
    };
    if per_twin {
        let mut inner = String::new();
        for group in &groups {
            inner.push_str(&group_element(group, decide, reasons.as_deref(), true));
        }
        let mut attributes = form_attributes(kind);
        attributes.push((
            "button-label",
            "Decline the ticked findings, each with its twin's word".to_string(),
        ));
        if !groups.is_empty() {
            children.push_str(&wrap("batch", &borrowed(&attributes), &inner));
        }
    } else {
        for group in &groups {
            let key = group.get("key").and_then(Value::as_str).unwrap_or("");
            let mut inner = group_element(group, decide, reasons.as_deref(), false);
            let suggested = group.get("reason").and_then(Value::as_str);
            if let Some(reasons) = &reasons {
                inner.push_str(&reason_options(reasons, suggested));
            }
            let mut attributes = form_attributes(key);
            attributes.push(("button-label", "Decline the ticked findings".to_string()));
            children.push_str(&wrap("batch", &borrowed(&attributes), &inner));
        }
    }

    // ---- the section's own attributes
    let q = group_query(kind, only, scope);
    let mut attributes: Vec<(&str, String)> = vec![
        ("view", "queue".to_string()),
        ("mode", "batch".to_string()),
        ("full", flag(!page.fragment).to_string()),
        ("title", "Queue".to_string()),
        ("state", "pending".to_string()),
        ("scope", scope.to_string()),
        ("group", kind.to_string()),
        ("page-url", page_url(&q)),
        ("rows-url", rows_url(&q)),
        ("refresh-url", rows_url(&q)),
        ("news", queue::NEWS_EVENT.to_string()),
        (
            "stale-text",
            "New findings arrived while you were deciding. They will appear when this batch is \
             submitted or the selection is left as it was."
                .to_string(),
        ),
        (
            "message",
            "Decide in batches. The machine proposed these groups; nothing here is decided until \
             you press Decline, a batch declines only the findings still ticked, and each one \
             gets its own ordinary decision. Publishing is one finding at a time."
                .to_string(),
        ),
    ];
    if let Some(repo) = only {
        attributes.push(("repo", repo.to_string()));
    }
    let serious = if scope == SCOPE_SERIOUS {
        "serious "
    } else {
        ""
    };
    if no_roots {
        attributes.push(("empty", "true".to_string()));
        attributes.push((
            "empty-text",
            "No repository here is readable under this grant, so there is nothing to group."
                .to_string(),
        ));
    } else if groups.is_empty() && read.iter().all(|(_, a)| a.is_ok()) {
        attributes.push(("empty", "true".to_string()));
        attributes.push((
            "empty-text",
            format!(
                "No {kind} groups among the {serious}pending findings in {where_}. Nothing here \
                 is waiting for a batch decision."
            ),
        ));
    } else if !groups.is_empty() {
        attributes.push((
            "count-text",
            format!(
                "{} holding {} in {where_}",
                plural(
                    groups.len(),
                    &format!("{kind} group"),
                    &format!("{kind} groups")
                ),
                plural(
                    shown,
                    &format!("{serious}pending finding"),
                    &format!("{serious}pending findings")
                ),
            ),
        ));
    }
    if !decide && !no_roots {
        attributes.push(("posture", "read-only".to_string()));
        attributes.push((
            "posture-text",
            format!(
                "This grant may read findings and not decide them: that needs `{}`.",
                ikigai_browse::CAP_ANNOTATE
            ),
        ));
    } else if decide && reasons.is_none() && !groups.is_empty() {
        children.push_str(&element(
            "no-form",
            &[],
            "The finding contract does not describe its decline reasons, so this page will not \
             invent them, and a batch decline needs one. Nothing can be decided here until it \
             does.",
        ));
    }
    let doc = envelope("page", &borrowed(&attributes), &children);
    Ok(web::html(
        render::render(&doc, !page.fragment).map_err(web::render_err)?,
    ))
}

fn borrowed<'a>(attributes: &'a [(&'a str, String)]) -> Vec<(&'a str, &'a str)> {
    attributes.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// The contract's reason words as picker options, the suggestion PRE-SELECTED. With no
/// suggestion — or a suggestion the contract no longer declares — an empty first option is
/// selected, and the batch is refused until a word is picked.
fn reason_options(reasons: &[(String, Option<String>)], suggested: Option<&str>) -> String {
    let suggested = suggested.filter(|s| reasons.iter().any(|(w, _)| w == s));
    let mut out = String::new();
    if suggested.is_none() {
        out.push_str(&element(
            "reason-option",
            &[
                ("value", ""),
                ("label", CHOOSE_REASON_LABEL),
                ("title", ""),
                ("selected", "true"),
                ("placeholder", "true"),
            ],
            "",
        ));
    }
    for (word, meaning) in reasons {
        out.push_str(&element(
            "reason-option",
            &[
                ("value", word),
                ("label", word),
                ("title", meaning.as_deref().unwrap_or("")),
                ("selected", flag(suggested == Some(word.as_str()))),
                ("placeholder", "false"),
            ],
            "",
        ));
    }
    out
}

/// One proposed group: its label, the declined twin or the kept row, and its members.
fn group_element(
    group: &Value,
    decide: bool,
    reasons: Option<&[(String, Option<String>)]>,
    per_twin: bool,
) -> String {
    let text = |key: &str| group.get(key).and_then(Value::as_str).unwrap_or("");
    let mut children = String::new();
    let twin = group.get("twin").filter(|t| !t.is_null());
    if let Some(twin) = twin {
        children.push_str(&twin_element(twin));
    }
    if let Some(kept) = group.get("kept").filter(|k| !k.is_null()) {
        children.push_str(&row_element("kept", kept, &[]));
    }
    // A member's own word, in the twin-carrying view: the twin's, when it has one.
    let twin_word = twin
        .and_then(|t| t.get("decision"))
        .and_then(|d| d.get(REASON_ARG))
        .and_then(Value::as_str);
    for member in members(group) {
        let id = member.get("id").and_then(Value::as_str).unwrap_or("");
        // ⚠ An UNRATED finding cannot be declined without a rating the human states, and a
        // batch states none — so it is shown, and not offered, rather than offered and then
        // refused by the Sink.
        let rated = member.get("severity").and_then(Value::as_str).is_some();
        let tickable = decide && rated;
        let mut extra: Vec<(&str, String)> = vec![("tickable", flag(tickable).to_string())];
        if decide && !rated {
            extra.push((
                "untickable",
                "unrated: a decline must carry a rating, so decide this one on its own row"
                    .to_string(),
            ));
        }
        let mut inner = String::new();
        if per_twin && tickable {
            if let Some(reasons) = reasons {
                extra.push(("reason-name", format!("{MEMBER_REASON_PREFIX}{id}")));
                inner = reason_options(reasons, twin_word);
            }
        }
        children.push_str(&row_element_with("member", member, &extra, &inner));
    }
    let mut attributes: Vec<(&str, String)> = vec![
        ("key", text("key").to_string()),
        ("label", text("label").to_string()),
        ("repo", text("repo").to_string()),
    ];
    if !text("annotates").is_empty() {
        attributes.push(("browse-href", browse_url(text("annotates"))));
    }
    wrap("group", &borrowed(&attributes), &children)
}

/// The declined twin's decision line: what a human already said about this claim.
fn twin_element(twin: &Value) -> String {
    let decision = twin.get("decision").filter(|d| !d.is_null());
    let text = |key: &str| {
        decision
            .and_then(|d| d.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    let id = twin.get("id").and_then(Value::as_str).unwrap_or("");
    let mut attributes: Vec<(&str, String)> = vec![
        ("twin-id", id.to_string()),
        ("outcome", text("outcome").to_string()),
        ("severity", text("severity").to_string()),
        ("at", web::when(text("decided_at"))),
    ];
    if !text(REASON_ARG).is_empty() {
        attributes.push((REASON_ARG, text(REASON_ARG).to_string()));
    }
    let children = match text("note") {
        "" => String::new(),
        note => element("note", &[], note),
    };
    wrap("twin", &borrowed(&attributes), &children)
}

fn row_element(name: &str, row: &Value, extra: &[(&str, String)]) -> String {
    row_element_with(name, row, extra, "")
}

/// One finding in a group — the fields a person needs to judge it before unticking it.
fn row_element_with(name: &str, row: &Value, extra: &[(&str, String)], inner: &str) -> String {
    let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
    let path = text("path");
    let where_ = match row.get("line").and_then(Value::as_u64) {
        Some(line) => format!("{path}:{line}"),
        None => path.to_string(),
    };
    let proposal = row.get("severity").and_then(Value::as_str);
    let mut attributes: Vec<(&str, String)> = vec![
        ("id", text("id").to_string()),
        ("where", where_),
        ("severity", proposal.unwrap_or("unrated").to_string()),
        (
            "severity-label",
            format!("model: {}", proposal.unwrap_or("unrated")),
        ),
        (
            "orphaned",
            flag(row.get("orphaned").and_then(Value::as_bool) == Some(true)).to_string(),
        ),
        ("provenance", queue::provenance(row)),
    ];
    if !text("annotates").is_empty() {
        attributes.push(("browse-href", browse_url(text("annotates"))));
    }
    attributes.extend(extra.iter().cloned());
    let mut children = element("body", &[], text("body"));
    if !text("exact").is_empty() {
        children.push_str(&element("quote", &[], text("exact")));
    }
    if let Some(prior) = row.get("prior_decision").filter(|p| !p.is_null()) {
        children.push_str(&queue::prior_element(prior));
    }
    children.push_str(inner);
    wrap(name, &borrowed(&attributes), &children)
}

// -------------------------------------------------------------------- the fan-out

/// `urn:iki:gonk:queue:batch` — one batch DECLINE, fanned out to the finding Sink.
///
/// ★ One `Sink urn:iki:finding:{id} decision=decline reason=<word>` per ticked member, under
/// the CALLER's capability, reported back as "declined N of M". A member that fails — decided
/// elsewhere meanwhile, a refusal — is named with its error and does not stop the others. The
/// section is then re-rendered from the SOURCE, so it never shows a decision the store did not
/// record.
///
/// ⚠ **The whole batch is checked before any member is tried**: nothing ticked, a member with
/// no word, a word the contract does not declare, or a decision other than decline refuses
/// the batch outright. A refusal that arrived half-way through would leave some members
/// decided and the person unsure which.
pub struct Batch {
    pub web: Arc<Web>,
}

/// What a batch form carried.
struct Form {
    members: Vec<String>,
    word: Option<String>,
    words: BTreeMap<String, String>,
    kind: Option<String>,
    repo: Option<String>,
    scope: Option<String>,
    decision: Option<String>,
}

fn read_form(body: &str) -> Result<Form> {
    let mut form = Form {
        members: Vec::new(),
        word: None,
        words: BTreeMap::new(),
        kind: None,
        repo: None,
        scope: None,
        decision: None,
    };
    let kept = |v: String| Some(v.trim().to_string()).filter(|v| !v.is_empty());
    for (name, value) in web::form_pairs(body) {
        match name.as_str() {
            MEMBER_FIELD => {
                if let Some(id) = kept(value) {
                    if !form.members.contains(&id) {
                        form.members.push(id);
                    }
                }
            }
            REASON_ARG => form.word = kept(value),
            "decision" => form.decision = kept(value),
            "_group" => form.kind = kept(value),
            "_repo" => form.repo = kept(value),
            "_severity" => form.scope = kept(value),
            other => match other.strip_prefix(MEMBER_REASON_PREFIX) {
                Some(id) => {
                    if let Some(word) = kept(value) {
                        form.words.insert(id.to_string(), word);
                    }
                }
                None if other.starts_with('_') => {}
                None => {
                    return Err(Error::InvalidArgument {
                        name: other.to_string(),
                        detail: format!(
                            "a batch form carries `{MEMBER_FIELD}`, `{REASON_ARG}` and \
                             `{MEMBER_REASON_PREFIX}<id>`; this field is none of them"
                        ),
                    })
                }
            },
        }
    }
    Ok(form)
}

impl Batch {
    /// The whole batch's checks, and the word each member will carry — or the sentence that
    /// refuses it, before anything is written.
    fn plan(&self, form: &Form) -> std::result::Result<Vec<(String, String)>, String> {
        if let Some(other) = form.decision.as_deref().filter(|d| *d != REASONED_DECISION) {
            return Err(format!(
                "Nothing was decided: a batch only declines, and this form asked for `{other}`. \
                 Publishing mints an annotation on each line, so it is one finding at a time."
            ));
        }
        if form.members.is_empty() {
            return Err("Nothing was declined: no finding was ticked.".to_string());
        }
        let declared = one_of(
            &self.web.hub,
            &finding_iri(queue::PROBE_ID),
            Verb::Sink,
            REASON_ARG,
        )
        .ok_or_else(|| {
            "Nothing was declined: the finding contract does not declare its reason words, and \
             a batch decline needs one."
                .to_string()
        })?;
        let mut plan = Vec::new();
        let mut wordless = Vec::new();
        for id in &form.members {
            match form.words.get(id).or(form.word.as_ref()) {
                Some(word) if declared.contains(word) => plan.push((id.clone(), word.clone())),
                Some(word) => {
                    return Err(format!(
                        "Nothing was declined: `{word}` is not a reason word the finding \
                         contract declares ({}).",
                        declared.join(", ")
                    ))
                }
                None => wordless.push(id.as_str()),
            }
        }
        if !wordless.is_empty() {
            return Err(format!(
                "Nothing was declined: a batch decline needs a reason word for every finding, \
                 and {} {} none — {}. Pick a word, or untick {}.",
                plural(wordless.len(), "ticked finding", "ticked findings"),
                if wordless.len() == 1 { "has" } else { "have" },
                wordless.join(", "),
                if wordless.len() == 1 { "it" } else { "them" },
            ));
        }
        Ok(plan)
    }
}

#[async_trait]
impl Endpoint for Batch {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Endpoint(format!("`{BATCH_IRI}` answers Sink only")));
        }
        let form = read_form(inv.inline_str("content")?)?;
        // Like the single decision: with no finding family bound there is nothing to decide,
        // and that is a typed NotFound rather than a rendered sentence.
        let probe = finding_iri(queue::PROBE_ID);
        let probe_iri = Iri::parse(&probe).map_err(|e| Error::Endpoint(format!("{probe}: {e}")))?;
        if self.web.hub.describe(&probe_iri).is_none() {
            return Err(Error::NotFound(format!(
                "nothing is bound at `{}{{id}}`",
                queue::FINDING_PREFIX
            )));
        }

        // ⚠ A refusal is RENDERED, as on the single decision: htmx does not swap a non-2xx
        // response, and the refusal is the most informative answer the page can give.
        let flash = match self.plan(&form) {
            Err(refusal) => ("error", refusal),
            Ok(plan) => {
                let total = plan.len();
                let mut failed: Vec<String> = Vec::new();
                for (id, word) in &plan {
                    let target = finding_iri(id);
                    let outcome = match Iri::parse(&target) {
                        Err(e) => Err(format!("`{target}`: {e}")),
                        Ok(target) => {
                            let request = Request::new(Verb::Sink, target)
                                .with_arg(
                                    "decision",
                                    ArgRef::Inline(REASONED_DECISION.as_bytes().to_vec()),
                                )
                                .with_arg(REASON_ARG, ArgRef::Inline(word.as_bytes().to_vec()));
                            inv.issue(request)
                                .await
                                .map(|_| ())
                                .map_err(|e| format!("{e}"))
                        }
                    };
                    if let Err(why) = outcome {
                        failed.push(format!("{id} — {why}"));
                    }
                }
                let declined = total - failed.len();
                let mut sentence = format!("Declined {declined} of {total}.");
                if !failed.is_empty() {
                    sentence.push_str(&format!(" Not declined: {}.", failed.join("; ")));
                }
                (if failed.is_empty() { "ok" } else { "error" }, sentence)
            }
        };
        let rows = QueuePage {
            web: Arc::clone(&self.web),
            fragment: true,
        };
        let params = queue::Params {
            state: None,
            repo: form.repo,
            limit: None,
            scope: form.scope,
            group: form.kind,
        };
        rows.body(inv, &params, Some((flash.0, flash.1.as_str())))
            .await
    }

    fn name(&self) -> &str {
        "gonk-queue-batch"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-queue-batch")
            .title("Decline a proposed group of findings, once")
            .summary(
                "The Queue's batch form adapter (ledger #506): an urlencoded body naming every \
                 ticked `member=<id>`, one `reason` word for the batch, or `reason:<id>` per \
                 member where each is declined with its own declined twin's word. The whole \
                 batch is checked first — nothing ticked, a member with no word, or a word the \
                 finding contract does not declare refuses it before anything is written. Then \
                 ONE `Sink urn:iki:finding:{id} decision=decline reason=<word>` per member, \
                 under the caller's own capability, so each gets an ordinary decision node; a \
                 member that fails is named with its error and does not stop the others. \
                 Answered with the queue section re-rendered from the source. ⚠ Decline only: \
                 publishing mints an annotation on each line and is one finding at a time.",
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("decline every ticked member, one decision each, and re-render")
                    // Every call it can make is the finding Sink, which requires both.
                    .requires(ikigai_browse::CAP_ANNOTATE)
                    .requires(ikigai_browse::CAP_WILDCARD)
                    .input(ArgSpec::new("content").class(XSD_STRING).summary(
                        "the form: `member` (repeated), `reason` or `reason:<id>`, plus \
                         `_group`, `_repo` and `_severity` for the re-render",
                    ))
                    .output("text/html"),
            )
            .verb(Verb::Meta)
    }
}

/// `xsd:string`, spelled once here.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
