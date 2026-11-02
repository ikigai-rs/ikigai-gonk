//! A summary per agent on the Dashboard, every number a click-through to the SPARQL that answers it
//! (ledger [#2820](http://localhost:1062/l/default/item/2820)).
//!
//! ```text
//! urn:iki:gonk:question:name:{name}                          Source  a stored query's text (src/questions/{name}.rq)
//! urn:iki:gonk:agent                                         Source  who ever took a claim, `ledger=` or all this grant reads
//! urn:iki:gonk:agent:holder:{holder}                         Source  that holder's answers in `ledger=`, each with its link, as JSON
//! urn:iki:gonk:agent:holder:{holder}:question:name:{name}    Source  one question, through `urn:sparql:select`
//! urn:iki:gonk:view:agent                                    Source  the panels: a table per readable ledger, as HTML
//! ```
//!
//! ★ **A question is a resource, and the number on the page IS its answer.** One text, the stored
//! question with the holder and the window written in ([`filled`]), produces the count and is the
//! link's query, so a link asks exactly what its number answered. A number is that query's row
//! count, except the median, its middle row ([`Reads::Median`]). `tests/agents_2820.rs` runs each
//! link through the SPARQL face and compares.
//!
//! ⚠ **What a number can see.** The questions read the events ikigai-ledger 0.15 began to write
//! (ledger #2819), so a claim or a release from before it is not counted, and a holder known only
//! from such a claim is not listed. A hand-back from before it names no holder until an operator
//! runs `ikigai_ledger::attribution::handed_back_by_backfill`, and one that landed while nobody
//! held the item never names one. A question's answer is one ledger's: the SPARQL page reads one
//! ledger's graph, so an agent working in two ledgers has a row in each.
//!
//! ★ **A window ends on the minute.** `?since` is floored to the minute, so its text is the same
//! for every load in that minute, and a load between writes reads from the cache. A window's
//! number can be up to a minute behind the clock, and the page says the minute.
//!
//! ★ **What one page costs**, with `A` agents across `L` ledgers: `L` queries for the agents and
//! `7A` for the answers. Each answer is cached under the store's write threads, so a load between
//! writes asks the store nothing except, once a minute, for the three windowed questions (`3A`).
//! ⚠ A write to ANY ledger costs the next load all of them, for the reason [`crate::dashboard`]
//! gives: a store read rests on the store's write thread, not on one ledger's. A panel's transform
//! is keyed on its document, which holds the headers and not the rows, so it reruns only when a
//! ledger starts or stops having agents (`tests/agents_2820.rs`). Measured on a release build with
//! one ledger of 6,000 items, ten agents with 300 hand-backs each and one with 3,000: 109 ms between
//! writes and 1.1 s after one, and no question over 60 ms, for the busiest agent.
//!
//! ⚠ **Write a question with grouping, not `NOT EXISTS`.** The store's planner runs a correlated
//! `NOT EXISTS` over the events per row, and a question past the store's budget holds up that
//! caller's next ones too (`every_question_answers_for_a_busy_agent`).
//!
//! The compositions above answer `Expiry::Always`: they are a few lines over cached reads, and
//! caching one would hold a `?since` past its minute.

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, Error, Invocation, ReprType, Representation, Result, Verb,
};
use ikigai_fn::html::{attr, html_text, url_segment};
use ikigai_ledger::Ledger;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::dashboard::{drawn_charts, Chart, ROWS_SLOT};
use crate::render::{self, element, wrap};
use crate::rules::sparql_string;
use crate::web::{
    as_html_arg, binding, fetch, html, html_only, ledger_href, menu_order, readable_ledgers,
    request, require_read, with, Web, HTML,
};

/// The set of agents.
pub const AGENTS_IRI: &str = "urn:iki:gonk:agent";
/// The panels.
pub const VIEW_IRI: &str = "urn:iki:gonk:view:agent";
/// The grammar of a stored question.
pub const QUESTION_TEMPLATE: &str = "urn:iki:gonk:question:name:{name}";
/// The grammar of one agent.
pub const AGENT_TEMPLATE: &str = "urn:iki:gonk:agent:holder:{holder}";
/// The grammar of one answer.
pub const ANSWER_TEMPLATE: &str = "urn:iki:gonk:agent:holder:{holder}:question:name:{name}";

/// The stored query behind [`AGENTS_IRI`].
const AGENTS_QUERY: &str = include_str!("questions/agents.rq");
/// The name [`AGENTS_QUERY`] is served under.
const AGENTS_NAME: &str = "agents";

const RESULTS_JSON: &str = "application/sparql-results+json";
const QUERY_TEXT: &str = "application/sparql-query";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_DATE_TIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

const MINUTE_MS: u64 = 60_000;
const DAY_MS: u64 = 24 * 60 * MINUTE_MS;

/// How a question's answer becomes the number in its cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reads {
    /// The number of rows: the link's query lists exactly what was counted.
    Rows,
    /// The `?took` of the middle row, the lower of two; no row is no answer.
    ///
    /// ★ **Picked here, not by the query.** SPARQL can order rows but cannot address the middle of
    /// them, and asking it to rank them is a self-join, quadratic in the hand-backs: measured on a
    /// release build it ran past the store's 5 s budget at an agent's 1,000.
    /// The link lists what the median was read from, which says more than one number would.
    Median,
}

/// One question: its stored text and how the panel says its answer.
#[derive(Debug)]
pub struct Question {
    /// Its name in [`QUESTION_TEMPLATE`].
    pub name: &'static str,
    /// The SPARQL, with `?holder` and, when it has a window, `?since`.
    pub text: &'static str,
    /// Its column's header.
    pub heading: &'static str,
    /// What the number counts, after the number, in the link's text.
    pub says: &'static str,
    /// How far back `?since` is from the minute of the read.
    pub window_ms: Option<u64>,
    /// How its answer is read.
    pub reads: Reads,
}

/// Every question, in the panel's order.
pub const QUESTIONS: [Question; 7] = [
    Question {
        name: "in-flight",
        text: include_str!("questions/in-flight.rq"),
        heading: "In flight",
        says: "in flight",
        window_ms: None,
        reads: Reads::Rows,
    },
    Question {
        name: "worked-on",
        text: include_str!("questions/worked-on.rq"),
        heading: "Worked on",
        says: "worked on",
        window_ms: None,
        reads: Reads::Rows,
    },
    Question {
        name: "completed-24h",
        text: include_str!("questions/completed-24h.rq"),
        heading: "Completed, 24 hours",
        says: "completed in the last 24 hours",
        window_ms: Some(DAY_MS),
        reads: Reads::Rows,
    },
    Question {
        name: "completed-7d",
        text: include_str!("questions/completed-7d.rq"),
        heading: "Completed, 7 days",
        says: "completed in the last 7 days",
        window_ms: Some(7 * DAY_MS),
        reads: Reads::Rows,
    },
    Question {
        name: "handed-back-24h",
        text: include_str!("questions/handed-back-24h.rq"),
        heading: "Handed back, 24 hours",
        says: "handed back in the last 24 hours",
        window_ms: Some(DAY_MS),
        reads: Reads::Rows,
    },
    Question {
        name: "released",
        text: include_str!("questions/released.rq"),
        heading: "Released",
        says: "released without a hand-back",
        window_ms: None,
        reads: Reads::Rows,
    },
    Question {
        name: "median-claim-to-handback",
        text: include_str!("questions/median-claim-to-handback.rq"),
        heading: "Median, claim to hand-back",
        says: "from claim to hand-back, the median",
        window_ms: None,
        reads: Reads::Median,
    },
];

fn question(name: &str) -> Result<&'static Question> {
    QUESTIONS
        .iter()
        .find(|q| q.name == name)
        .ok_or_else(|| Error::NotFound(format!("no question is named `{name}`")))
}

/// `question`'s text with `holder` and `since` written in where its parameters stand: the one query
/// both the count and the link run.
///
/// ★ **Written in, not `bindings`.** A value in `bindings` is invisible to the store's query planner,
/// which then joins a holder's hand-backs to its claims as if it could be anyone: an agent's 1,000
/// hand-backs took 439 ms that way and 17 ms with the label in the text, and the bound form grows
/// with the square. A query past its budget also holds up that caller's next ones, so that is the
/// difference between a page that works and one that does not. The label is escaped for the grammar
/// it lands in ([`sparql_string`]), so it can only ever be a string.
pub fn filled(question: &Question, holder: &str, since: Option<&str>) -> String {
    // The window first: the label is the one free text, so nothing may be substituted after it.
    let text = match since {
        Some(since) => parameter(
            question.text,
            "?since",
            &format!("\"{}\"^^<{XSD_DATE_TIME}>", sparql_string(since)),
        ),
        None => question.text.to_string(),
    };
    let holder = format!("\"{}\"", sparql_string(holder));
    parameter(&text, "?holder", &holder)
}

/// `text` with the whole variable `name` replaced by `term`, and a longer name left alone.
fn parameter(text: &str, name: &str, term: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(name) {
        let after = &rest[at + name.len()..];
        let whole = !after.starts_with(|c: char| c.is_alphanumeric() || c == '_');
        out.push_str(&rest[..at]);
        out.push_str(if whole { term } else { name });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The SPARQL page, running `query` over `ledger` as it opens: the page runs any `query` it is
/// given, so the results are there with no further click, and `#results` opens the tab at them,
/// which is otherwise below the samples and the editor.
fn sparql_href(ledger: &Ledger, query: &str) -> String {
    format!(
        "/sparql?ledger={}&query={}#results",
        url_segment(ledger.name()),
        url_segment(query)
    )
}

/// `2026-11-02T14:37:00.000Z`, `window_ms` before the minute of `now_ms`.
fn since_of(now_ms: u64, window_ms: u64) -> String {
    ikigai_ledger::sparql::iso8601((now_ms - now_ms % MINUTE_MS).saturating_sub(window_ms))
}

/// The kernel's clock, in milliseconds. A window with no clock is refused, never read as all time.
fn now_ms(inv: &Invocation<'_>) -> Result<u64> {
    inv.now().map(|t| t.as_millis()).ok_or_else(|| {
        Error::Endpoint("this kernel has no clock, so a window has no start".to_string())
    })
}

/// `YYYY-MM-DDTHH:MM:SS`, seconds perhaps fractional, ending in `Z`: a `since` the store compares
/// rather than one that matches nothing and reads as a count of zero.
fn is_instant(text: &str) -> bool {
    let Some(bare) = text.strip_suffix('Z') else {
        return false;
    };
    let (whole, fraction) = bare.split_once('.').unwrap_or((bare, "0"));
    let digits_at = |at: &[usize]| at.iter().all(|&n| whole.as_bytes()[n].is_ascii_digit());
    whole.len() == 19
        && whole.is_ascii()
        && digits_at(&[0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18])
        && whole[4..5] == *"-"
        && whole[7..8] == *"-"
        && whole[10..11] == *"T"
        && whole[13..14] == *":"
        && whole[16..17] == *":"
        && !fraction.is_empty()
        && fraction.bytes().all(|b| b.is_ascii_digit())
}

/// A captured `{holder}`: a label is free text, so its IRI carries it percent-encoded.
fn decode_holder(segment: &str) -> Result<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let byte = segment
                .get(at + 1..at + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            let Some(byte) = byte else {
                return Err(not_a_holder(segment));
            };
            out.push(byte);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).map_err(|_| not_a_holder(segment))
}

fn not_a_holder(segment: &str) -> Error {
    Error::InvalidArgument {
        name: "holder".to_string(),
        detail: format!("`{segment}` is not a percent-encoded label"),
    }
}

/// The `ledger` argument, checked against the caller's own grant.
fn asked_ledger(inv: &Invocation<'_>) -> Result<Ledger> {
    let ledger = Ledger::parse(inv.inline_str("ledger")?)?;
    require_read(inv.capability, &ledger)?;
    Ok(ledger)
}

/// `query` through the SPARQL face, under the caller's capability, over the graphs in `graph`.
async fn asked(inv: &Invocation<'_>, graph: &str, query: &str) -> Result<Representation> {
    let read = with(request(Verb::Source, "urn:sparql:select")?, "query", query);
    let read = with(read, "as", inv.inline_str("as").unwrap_or(RESULTS_JSON));
    inv.issue(with(read, "graph", graph)).await
}

/// The result-set faces, default first, as `urn:sparql:select` serves them.
const RESULT_FACES: [&str; 4] = [
    RESULTS_JSON,
    "application/sparql-results+xml",
    "text/csv",
    "text/tab-separated-values",
];

fn face_arg() -> ArgSpec {
    ArgSpec::new("as")
        .summary("The serialization `urn:sparql:select` answers in.")
        .class(XSD_STRING)
        .one_of(RESULT_FACES)
        .default_value(RESULTS_JSON)
        .optional()
}

/// `{holder}` in a template.
fn holder_arg() -> ArgSpec {
    ArgSpec::new("holder")
        .summary("The holder's label, percent-encoded.")
        .class(XSD_STRING)
        .binding()
}

/// `{name}` in a template, one of `names`.
fn name_arg(names: impl IntoIterator<Item = &'static str>) -> ArgSpec {
    ArgSpec::new("name")
        .summary("Which question.")
        .class(XSD_STRING)
        .one_of(names)
        .binding()
}

fn ledger_arg() -> ArgSpec {
    ArgSpec::new("ledger")
        .summary("Whose graph the question reads.")
        .class(XSD_STRING)
}

/// A stored question's text.
pub struct QuestionText;

#[async_trait]
impl Endpoint for QuestionText {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let name = binding(inv, "name")?;
        let text = match name.as_str() {
            AGENTS_NAME => AGENTS_QUERY,
            name => question(name)?.text,
        };
        Ok(Representation::new(
            ReprType::new(QUERY_TEXT),
            text.as_bytes().to_vec(),
        ))
    }

    fn name(&self) -> &str {
        "gonk-question"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("A stored question about the agents")
            .summary(
                "The SPARQL text of one question the Dashboard asks of an agent, with its named \
                 parameters `?holder` and `?since`. Data the binary ships, and reads no ledger.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(name_arg(
                QUESTIONS
                    .iter()
                    .map(|q| q.name)
                    .chain(std::iter::once(AGENTS_NAME)),
            ))
            .output(QUERY_TEXT)
    }
}

/// `urn:iki:gonk:agent`: every holder that ever took a claim.
///
/// ⚠ With no `ledger`, the graphs are named from the ledgers this caller may READ, never left to the
/// face's default dataset: a writing grant's store tokens reach graphs its holder may not read as
/// ledgers, and the door's refusal of them (ledger #2836) does not see a read made from in here.
pub struct Agents {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for Agents {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let graph = if inv.request.args.contains_key("ledger") {
            asked_ledger(inv)?.graph()
        } else {
            let readable: Vec<String> = readable_ledgers(&self.web, inv.capability)
                .iter()
                .map(Ledger::graph)
                .collect();
            if readable.is_empty() {
                return Err(Error::Denied(
                    "this capability may read no ledger, so it has no agents to list".to_string(),
                ));
            }
            readable.join(" ")
        };
        let answer = asked(inv, &graph, AGENTS_QUERY).await?;
        Ok(answer.with_expiry(ikigai_core::Expiry::Always))
    }

    fn name(&self) -> &str {
        "gonk-agents"
    }

    fn describe(&self) -> Description {
        let desc = Description::new(self.name())
            .title("Who ever took a claim")
            .summary(
                "Each distinct holder on a `ledger:ClaimEvent`, through `urn:sparql:select` under \
                 the caller's capability: in `ledger`, or in every ledger the caller may read.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_store::CAP_READ_GRAPH)
            .input(ledger_arg().optional())
            .input(face_arg());
        RESULT_FACES.iter().fold(desc, |d, face| d.output(*face))
    }
}

/// One question's answer for one holder.
pub struct Answer;

#[async_trait]
impl Endpoint for Answer {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let holder = decode_holder(&binding(inv, "holder")?)?;
        let question = question(&binding(inv, "name")?)?;
        let ledger = asked_ledger(inv)?;
        let given = inv.inline_str("since").ok();
        let since = match (question.window_ms, given) {
            (Some(_), Some(since)) if is_instant(since) => Some(since.to_string()),
            (None, None) => None,
            (Some(window), None) => Some(since_of(now_ms(inv)?, window)),
            (Some(_), Some(since)) => {
                return Err(Error::InvalidArgument {
                    name: "since".to_string(),
                    detail: format!("`{since}` is not a UTC dateTime ending in `Z`"),
                })
            }
            (None, Some(_)) => {
                return Err(Error::InvalidArgument {
                    name: "since".to_string(),
                    detail: format!("`{}` has no window", question.name),
                })
            }
        };
        let query = filled(question, &holder, since.as_deref());
        let answer = asked(inv, &ledger.graph(), &query).await?;
        Ok(answer.with_expiry(ikigai_core::Expiry::Always))
    }

    fn name(&self) -> &str {
        "gonk-agent-answer"
    }

    fn describe(&self) -> Description {
        let desc = Description::new(self.name())
            .title("One question, asked of one agent")
            .summary(
                "The stored question with this holder written in, through `urn:sparql:select`, \
                 over `ledger`'s graph, under the caller's capability. `{holder}` is the holder's \
                 label, percent-encoded.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_store::CAP_READ_GRAPH)
            .input(holder_arg())
            .input(name_arg(QUESTIONS.iter().map(|q| q.name)))
            .input(ledger_arg())
            .input(
                ArgSpec::new("since")
                    .summary(
                        "Where a windowed question's window starts, a UTC dateTime; the start of \
                         this minute's window when omitted. Refused on a question with no window.",
                    )
                    .class(XSD_DATE_TIME)
                    .optional(),
            )
            .input(face_arg());
        RESULT_FACES.iter().fold(desc, |d, face| d.output(*face))
    }
}

/// The number in a question's cell, from its answer's JSON: `None` is a median with no hand-back.
fn answered(reads: Reads, results: &Value) -> Result<Option<String>> {
    let rows = results["results"]["bindings"]
        .as_array()
        .ok_or_else(|| Error::Endpoint("a question's answer carried no result rows".to_string()))?;
    Ok(match reads {
        Reads::Rows => Some(rows.len().to_string()),
        Reads::Median => rows
            .get(rows.len().saturating_sub(1) / 2)
            .and_then(|row| row["took"]["value"].as_str())
            .map(str::to_string),
    })
}

/// `urn:iki:gonk:agent:holder:{holder}`: each question's answer in `ledger`, with its link.
pub struct Agent;

#[async_trait]
impl Endpoint for Agent {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let segment = binding(inv, "holder")?;
        let holder = decode_holder(&segment)?;
        let ledger = asked_ledger(inv)?;
        let mut answers = Vec::with_capacity(QUESTIONS.len());
        for question in &QUESTIONS {
            let since = match question.window_ms {
                Some(window) => Some(since_of(now_ms(inv)?, window)),
                None => None,
            };
            let mut read = with(
                request(
                    Verb::Source,
                    &format!(
                        "urn:iki:gonk:agent:holder:{segment}:question:name:{}",
                        question.name
                    ),
                )?,
                "ledger",
                ledger.name(),
            );
            if let Some(since) = &since {
                read = with(read, "since", since);
            }
            let answer = match fetch(inv, read).await {
                Err(denied @ Error::Denied(_)) => return Err(denied),
                Err(error) => Err(error.to_string()),
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|v| answered(question.reads, &v).map_err(|e| e.to_string())),
            };
            let query = filled(question, &holder, since.as_deref());
            let mut row =
                json!({ "question": question.name, "href": sparql_href(&ledger, &query) });
            match answer {
                Ok(value) => row["answer"] = json!(value),
                Err(error) => row["error"] = json!(error),
            }
            answers.push(row);
        }
        let list = json!({ "holder": holder, "ledger": ledger.name(), "answers": answers });
        Ok(Representation::new(
            ReprType::new("application/json"),
            list.to_string().into_bytes(),
        ))
    }

    fn name(&self) -> &str {
        "gonk-agent"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("One agent's answers in one ledger")
            .summary(
                "Each stored question's answer for this holder in `ledger`, under the caller's \
                 capability: its name, its `answer` (a row count, or a median as a duration, null \
                 when there is none), or an `error` saying why it could not be read, and the \
                 `href` of the SPARQL page asking the same question. `{holder}` is the holder's \
                 label, percent-encoded.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_store::CAP_READ_GRAPH)
            .input(holder_arg())
            .input(ledger_arg())
            .output("application/json")
    }
}

/// An `xsd:dayTimeDuration` in words, its two largest units: `2 d 3 h`, `4 min 5 s`. `None` for
/// anything else, which the cell then shows as the store wrote it.
fn duration_words(text: &str) -> Option<String> {
    let rest = text.strip_prefix('P')?;
    let (days, time) = rest.split_once('T').unwrap_or((rest, ""));
    let mut seconds = 0f64;
    if !days.is_empty() {
        seconds += days.strip_suffix('D')?.parse::<f64>().ok()? * 86_400.0;
    }
    let mut number = String::new();
    for c in time.chars() {
        match c {
            'H' | 'M' | 'S' => {
                let scale = match c {
                    'H' => 3_600.0,
                    'M' => 60.0,
                    _ => 1.0,
                };
                seconds += number.parse::<f64>().ok()? * scale;
                number.clear();
            }
            _ => number.push(c),
        }
    }
    if !number.is_empty() || seconds < 0.0 {
        return None;
    }
    if seconds < 1.0 {
        return Some("under 1 s".to_string());
    }
    let whole = seconds as u64;
    let units = [
        (whole / 86_400, "d"),
        (whole % 86_400 / 3_600, "h"),
        (whole % 3_600 / 60, "min"),
        (whole % 60, "s"),
    ];
    let first = units.iter().position(|(n, _)| *n > 0)?;
    let words: Vec<String> = units[first..]
        .iter()
        .take(2)
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n} {unit}"))
        .collect();
    Some(words.join(" "))
}

/// One cell: the number as a link naming what it counts and for whom, and that it opens a new tab.
fn cell(question: &Question, holder: &str, answer: &Value) -> String {
    let shown = match (answer["answer"].as_str(), question.reads) {
        (Some(duration), Reads::Median) => {
            duration_words(duration).unwrap_or_else(|| duration.to_string())
        }
        (Some(count), Reads::Rows) => count.to_string(),
        _ if answer.get("error").is_some() => "unavailable".to_string(),
        _ => "none".to_string(),
    };
    let href = answer["href"].as_str().unwrap_or_default();
    format!(
        "<td><a href=\"{}\" target=\"_blank\" rel=\"noopener\"><span class=\"sr-only\">{}: \
         </span>{}<span class=\"sr-only\"> {}, opens in a new tab</span></a></td>",
        attr(href),
        html_text(holder),
        html_text(&shown),
        html_text(question.says),
    )
}

/// One table row: the holder, then a cell per question.
fn row(holder: &str, answers: &Value) -> Result<String> {
    let answers = answers["answers"]
        .as_array()
        .filter(|list| list.len() == QUESTIONS.len())
        .ok_or_else(|| Error::Endpoint("an agent's answers did not list them all".to_string()))?;
    let cells: String = QUESTIONS
        .iter()
        .zip(answers)
        .map(|(question, answer)| cell(question, holder, answer))
        .collect();
    Ok(format!(
        "<tr><th scope=\"row\">{}</th>{cells}</tr>",
        html_text(holder)
    ))
}

/// The holders in `ledger`, in the query's order.
async fn holders(inv: &Invocation<'_>, ledger: &Ledger) -> Result<Vec<String>> {
    let read = with(request(Verb::Source, AGENTS_IRI)?, "ledger", ledger.name());
    let results: Value = serde_json::from_slice(&fetch(inv, read).await?)
        .map_err(|e| Error::Endpoint(format!("the agents were not JSON: {e}")))?;
    Ok(results["results"]["bindings"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|row| row["holder"]["value"].as_str())
        .map(str::to_string)
        .collect())
}

/// The rows of `ledger`'s table, or `Denied` when this viewer may not read it.
async fn rows(inv: &Invocation<'_>, ledger: &Ledger) -> Result<String> {
    let mut out = String::new();
    for holder in holders(inv, ledger).await? {
        let iri = format!("urn:iki:gonk:agent:holder:{}", url_segment(&holder));
        let read = with(request(Verb::Source, &iri)?, "ledger", ledger.name());
        let answers: Value = serde_json::from_slice(&fetch(inv, read).await?)
            .map_err(|e| Error::Endpoint(format!("the answers were not JSON: {e}")))?;
        out.push_str(&row(&holder, &answers)?);
    }
    Ok(out)
}

/// One ledger's panel, or `None` when this viewer may not read the ledger.
async fn panel(inv: &Invocation<'_>, ledger: &Ledger) -> Option<Chart> {
    let id = format!("agents-{}", ledger.name());
    let href = ledger_href(ledger);
    let mut attributes = vec![
        ("id", id.as_str()),
        ("title", ledger.name()),
        ("href", href.as_str()),
    ];
    let (note, rows) = match rows(inv, ledger).await {
        Err(Error::Denied(_)) => return None,
        Err(error) => (
            format!("The agents could not be read: {error}."),
            String::new(),
        ),
        Ok(rows) if rows.is_empty() => (
            "No agent has claimed an item in this ledger yet.".to_string(),
            rows,
        ),
        Ok(rows) => (String::new(), rows),
    };
    let children = if note.is_empty() {
        let columns: String = QUESTIONS
            .iter()
            .map(|q| element("agent-column", &[], q.heading))
            .collect();
        columns + &render::slot(ROWS_SLOT)
    } else {
        attributes.push(("note", note.as_str()));
        String::new()
    };
    Some(Chart {
        document: render::chunk(&wrap("agent-panel", &attributes, &children)),
        rows,
    })
}

/// `urn:iki:gonk:view:agent`: one panel per ledger the viewer may read, as HTML.
pub struct AgentView {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for AgentView {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        let ledgers = readable_ledgers(&self.web, inv.capability);
        let mut panels = Vec::new();
        for ledger in menu_order(&ledgers) {
            if let Some(panel) = panel(inv, ledger).await {
                panels.push(panel);
            }
        }
        Ok(html(drawn_charts(inv, panels).await?))
    }

    fn name(&self) -> &str {
        "gonk-view-agent"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("The agents' panels")
            .summary(
                "A table per ledger this caller may read: a row per agent, a column per stored \
                 question, each number linking to the SPARQL page asking that question in a new \
                 tab. A ledger the caller may not read is absent.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(as_html_arg())
            .output(HTML)
    }
}

/// The sentence over the panels: what a number does, and what they are as of. A kernel with no
/// clock has no windows either, and its panels say so; the rest of the page is still good.
pub(crate) fn as_of(inv: &Invocation<'_>) -> String {
    let what = "Each number opens, in a new tab, the SPARQL question that answers it, and the \
                question runs as the tab opens. Claims from before the ledger recorded them are \
                not counted.";
    match now_ms(inv) {
        Ok(now) => {
            let minute = ikigai_ledger::sparql::iso8601(now - now % MINUTE_MS);
            format!("As of {} UTC. {what}", minute[..16].replace('T', " "))
        }
        Err(_) => what.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_reads_as_its_two_largest_units() {
        assert_eq!(duration_words("PT0.5S").unwrap(), "under 1 s");
        assert_eq!(duration_words("PT45S").unwrap(), "45 s");
        assert_eq!(duration_words("PT2H14M3S").unwrap(), "2 h 14 min");
        assert_eq!(duration_words("PT8040S").unwrap(), "2 h 14 min");
        assert_eq!(duration_words("P3DT1H").unwrap(), "3 d 1 h");
        assert_eq!(duration_words("P3D").unwrap(), "3 d");
        assert_eq!(duration_words("PT1H0M5S").unwrap(), "1 h");
        assert_eq!(duration_words("nonsense"), None);
    }

    #[test]
    fn a_window_ends_on_the_minute() {
        let at = 1_793_000_000_123;
        let since = since_of(at, DAY_MS);
        assert_eq!(since, since_of(at + 1, DAY_MS));
        assert!(since.ends_with(":00.000Z"), "{since}");
        assert!(is_instant(&since));
    }

    #[test]
    fn only_a_utc_datetime_is_an_instant() {
        assert!(is_instant("2026-11-02T14:37:00Z"));
        assert!(is_instant("2026-11-02T14:37:00.000Z"));
        for not in [
            "",
            "yesterday",
            "2026-11-02",
            "2026-11-02T14:37:00",
            "2026-11-02T14:37:00+01:00",
            "2026-11-02T14:37:00.Z",
            "2026-11-02 14:37:00Z",
            "2026-11-02T14:37:0é0Z",
        ] {
            assert!(!is_instant(not), "{not}");
        }
    }

    #[test]
    fn a_holder_round_trips_through_its_segment() {
        for label in ["lawnboy-claude", "a b:\"c\"/é%", "x"] {
            assert_eq!(decode_holder(&url_segment(label)).unwrap(), label);
        }
        assert!(decode_holder("bad%zz").is_err());
        assert!(decode_holder("bad%f").is_err());
        assert!(decode_holder("bad%ff").is_err());
    }

    #[test]
    fn a_label_filled_into_a_question_can_only_be_a_string() {
        let hostile = "x\" } } #\n";
        for question in &QUESTIONS {
            let since = question.window_ms.map(|_| "2026-11-02T14:37:00.000Z");
            let query = filled(question, hostile, since);
            assert!(query.contains("\"x\\\" } } #\\n\""), "{query}");
            assert!(
                spargebra::SparqlParser::new().parse_query(&query).is_ok(),
                "{}: {query}",
                question.name
            );
            // The window's parameter is in the text exactly when the question has a window.
            assert_eq!(
                question.text.contains("?since"),
                question.window_ms.is_some(),
                "{}",
                question.name
            );
        }
    }

    /// ⚠ A label that spells a parameter is still only a label.
    #[test]
    fn a_label_spelling_a_parameter_is_not_substituted() {
        let since = Some("2026-11-02T14:37:00.000Z");
        for hostile in ["?since", "?holder", "a ?since b ?holder c"] {
            let query = filled(&QUESTIONS[2], hostile, since);
            assert!(query.contains(&format!("\"{hostile}\"")), "{query}");
            assert!(
                spargebra::SparqlParser::new().parse_query(&query).is_ok(),
                "{query}"
            );
        }
        // And a longer variable is another variable.
        assert_eq!(parameter("?a ?ab ?a_ ?a.", "?a", "x"), "x ?ab ?a_ x.");
    }

    #[test]
    fn a_cell_says_what_it_counts_for_whom_and_that_it_opens_a_tab() {
        let answer = json!({ "answer": "4", "href": "/sparql?ledger=a&query=b" });
        let cell = cell(&QUESTIONS[2], "lawnboy-<claude>", &answer);
        let text: String = cell
            .split('<')
            .map(|s| s.split_once('>').map_or("", |p| p.1))
            .collect();
        assert_eq!(
            text.replace("&lt;", "<").replace("&gt;", ">"),
            "lawnboy-<claude>: 4 completed in the last 24 hours, opens in a new tab"
        );
        assert!(
            cell.contains("target=\"_blank\" rel=\"noopener\""),
            "{cell}"
        );
    }

    #[test]
    fn a_cell_with_no_answer_says_which_kind_of_nothing() {
        let shown = |answer| {
            let cell = cell(&QUESTIONS[6], "h", &answer);
            cell.split("</span>")
                .nth(1)
                .unwrap()
                .split('<')
                .next()
                .unwrap()
                .to_string()
        };
        assert_eq!(shown(json!({ "answer": null, "href": "/" })), "none");
        assert_eq!(
            shown(json!({ "error": "timed out", "href": "/" })),
            "unavailable"
        );
        assert_eq!(
            shown(json!({ "answer": "PT90S", "href": "/" })),
            "1 min 30 s"
        );
    }
}
