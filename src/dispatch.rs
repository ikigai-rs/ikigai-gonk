//! A dispatcher's unread events, with an explicitly acknowledged, durable cursor (#3712).
//!
//! Cursor membership, rather than a date watermark, admits backdated imports and events
//! arriving during review. Each reader and each scope (ledger or item number) has its own
//! cursor in the ledger graph. Source never writes; ack inserts only the supplied event IDs.
//! Concurrent acknowledgments commute. Reads and writes use the caller's capability.
use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Expiry, Invocation, Iri,
    ReprType, Representation, Request, Result, UriTemplate, Verb,
};
use ikigai_ledger::{vocabulary as v, Ledger};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const SEEN: &str = "urn:iki:gonk:dispatch:seen";
const READER: &str = "urn:iki:gonk:dispatch:reader";
const SCOPE: &str = "urn:iki:gonk:dispatch:scope";
const MAX: usize = 500;
const NOTE: &str = "Renewals and transport refusals are not recorded as ledger events; renewal counts are unavailable. Refusals here are recorded rejections of hand-backs. Attention lists current conditions, even after acknowledgment.";

/// Bind the text/JSON read and its separate acknowledgment write behind every door.
pub fn space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(
            UriTemplate::parse("urn:iki:gonk:dispatch:{ledger}:ack").unwrap(),
            Dispatch { ack: true },
        )
        .bind(
            UriTemplate::parse("urn:iki:gonk:dispatch:{ledger}").unwrap(),
            Dispatch { ack: false },
        )
        .named(crate::spaces::iri(crate::spaces::DISPATCH))
}

struct Dispatch {
    ack: bool,
}

#[derive(Default, Serialize)]
struct Event {
    iri: String,
    item: String,
    number: i64,
    kind: String,
    at: Option<String>,
    text: Option<String>,
    /// Remaining predicate IRIs and lexical values; no target is dereferenced.
    details: BTreeMap<String, BTreeSet<String>>,
}
impl Event {
    fn one(&self, p: &str) -> Option<&str> {
        self.details.get(p)?.first().map(String::as_str)
    }
    fn has(&self, p: &str, value: &str) -> bool {
        self.details
            .get(p)
            .is_some_and(|values| values.contains(value))
    }
    fn classify(&mut self) {
        self.kind = if self.has(v::HAND_BACK, "true") || self.one(v::HANDED_BACK_BY).is_some() {
            "handback"
        } else if self.one(v::REJECTS).is_some() {
            "refusal"
        } else if self.one(v::TAKEOVER_FROM).is_some() {
            "takeover"
        } else if self.has(v::ext::TYPE, v::CLAIM_EVENT_CLASS) {
            "claim"
        } else if self.has(v::ext::TYPE, v::RELEASE_EVENT_CLASS) {
            "release"
        } else if self.has(v::ext::TYPE, v::COMMENT_CLASS) {
            "comment"
        } else if self.has(v::ext::TYPE, v::ITEM_CLASS) {
            "created"
        } else if self.has(v::ext::TYPE, v::STATUS_EVENT_CLASS) {
            if self.has(v::STATUS, v::CLOSED) {
                "closed"
            } else {
                "reopened"
            }
        } else {
            self.one(v::EDIT_KIND).unwrap_or("event")
        }
        .to_string();
        self.at = self.one(v::ext::CREATED).map(str::to_string);
        self.text = self.one(v::BODY).map(str::to_string);
    }
    fn rank(&self) -> u8 {
        match self.kind.as_str() {
            "handback" | "refusal" | "takeover" => 0,
            "link" | "unlink" if self.has(v::EDIT_LINK_TYPE, "blocks") => 0,
            "claim" => 2,
            _ => 1,
        }
    }
}

#[derive(Serialize)]
struct Document {
    schema: u32,
    ledger: String,
    reader: String,
    item: Option<i64>,
    cursor: String,
    attention: Vec<Value>,
    more_attention: bool,
    events: Vec<Event>,
    folded: Value,
    acknowledge: Vec<String>,
    more: bool,
    note: &'static str,
}

fn invalid(name: &str, detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: name.into(),
        detail: detail.into(),
    }
}
fn optional<'a>(inv: &'a Invocation<'_>, name: &str) -> Result<Option<&'a str>> {
    if inv.request.args.contains_key(name) {
        inv.inline_str(name).map(Some)
    } else {
        Ok(None)
    }
}
fn literal(text: &str) -> String {
    format!("\"{}\"", crate::rules::sparql_string(text))
}
fn require(inv: &Invocation<'_>, token: String) -> Result<()> {
    if inv.capability.allows(&token) {
        Ok(())
    } else {
        Err(Error::Denied(token))
    }
}

/// Every read and update names exactly one graph; forwarding preserves budgets and identity.
async fn store(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    verb: Verb,
    text: String,
) -> Result<Representation> {
    let (target, arg) = if verb == Verb::Source {
        ("graph-select", "query")
    } else {
        ("graph-update", "content")
    };
    let mut request = Request::new(verb, Iri::parse(format!("urn:iki:store:{target}")).unwrap())
        .with_arg("graph", ArgRef::Inline(ledger.graph().into_bytes()))
        .with_arg(arg, ArgRef::Inline(text.into_bytes()));
    if verb == Verb::Source {
        request = request.with_arg(
            "as",
            ArgRef::Inline(b"application/sparql-results+json".to_vec()),
        );
    }
    if let Some(budget) = optional(inv, "budget")? {
        request = request.with_arg("budget", ArgRef::Inline(budget.as_bytes().to_vec()));
    }
    inv.issue(request).await
}
async fn select(inv: &Invocation<'_>, ledger: &Ledger, query: String) -> Result<Vec<Value>> {
    let answer = store(inv, ledger, Verb::Source, query).await?;
    let json: Value =
        serde_json::from_slice(&answer.bytes).map_err(|e| Error::Endpoint(e.to_string()))?;
    json.pointer("/results/bindings")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| Error::Endpoint("dispatch: store returned no result rows".into()))
}
fn value<'a>(row: &'a Value, name: &str) -> &'a str {
    row.get(name)
        .and_then(|v| v.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// The membership test is also used when acknowledging: arbitrary subjects cannot become seen.
fn members(item: Option<i64>) -> String {
    format!("{{ ?event <{on}> ?item ; a ?class . FILTER(?class IN (<{comment}>, <{claim}>, <{release}>, <{edit}>, <{status}>)) }} UNION {{ ?event a <{class}> . BIND(?event AS ?item) }}\n?item <{number}> ?number . {filter}",
        on=v::ON_ITEM, class=v::ITEM_CLASS, number=v::NUMBER, comment=v::COMMENT_CLASS, claim=v::CLAIM_EVENT_CLASS, release=v::RELEASE_EVENT_CLASS, edit=v::EDIT_EVENT_CLASS, status=v::STATUS_EVENT_CLASS,
        filter=item.map(|n| format!("FILTER(?number = {n})")).unwrap_or_default())
}

#[async_trait]
impl Endpoint for Dispatch {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let verb = if self.ack { Verb::Sink } else { Verb::Source };
        if inv.request.verb != verb {
            return Err(invalid("verb", format!("dispatch expects {verb:?}")));
        }
        let ledger = Ledger::parse(
            inv.bindings
                .get("ledger")
                .ok_or_else(|| invalid("ledger", "missing ledger"))?,
        )?;
        crate::web::require_read(inv.capability, &ledger)?;
        let named = optional(inv, "reader")?;
        let reader = match (inv.capability.principal(), named) {
            (Some(principal), None) => principal,
            (Some(principal), Some(name)) if principal == name => principal,
            (None, Some(name))
                if inv.capability.is_root() && !name.is_empty() && name.len() <= 256 =>
            {
                name
            }
            _ => {
                return Err(invalid(
                    "reader",
                    "use the capability's principal; only root may name a reader (1–256 bytes)",
                ))
            }
        };
        let item = optional(inv, "item")?
            .map(|s| {
                s.parse::<i64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| invalid("item", "expected a positive item number"))
            })
            .transpose()?;
        let scope = item
            .map(|n| format!("item:{n}"))
            .unwrap_or_else(|| "ledger".into());
        let digest = Sha256::digest(serde_json::to_vec(&(reader, &scope)).unwrap());
        let cursor = format!("{}dispatch-cursor:{digest:x}", ledger.prefix());
        let graph = ledger.graph();
        let as_ = optional(inv, "as")?.unwrap_or("text/plain");
        if !matches!(as_, "text/plain" | "application/json") {
            return Err(invalid("as", "expected text/plain or application/json"));
        }
        if self.ack {
            require(inv, ledger.cap_write())?;
            require(inv, ikigai_store::cap_write_graph(&graph))?;
            let ids: Vec<String> =
                serde_json::from_str(inv.inline_str("content")?).map_err(|_| {
                    invalid(
                        "content",
                        "expected the acknowledge array returned by the view",
                    )
                })?;
            if ids.len() > MAX {
                return Err(invalid(
                    "content",
                    "at most 500 event IDs per acknowledgment",
                ));
            }
            for id in &ids {
                if !id.starts_with(&ledger.prefix())
                    || id
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control() || "<>\"{}|\\^`".contains(c))
                    || Iri::parse(id).is_err()
                {
                    return Err(invalid("content", "expected event IRIs in this ledger"));
                }
            }
            if !ids.is_empty() {
                let values = ids
                    .iter()
                    .map(|id| format!("<{id}>"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let update = format!("INSERT {{ GRAPH <{graph}> {{ <{cursor}> <{SEEN}> ?event ; <{READER}> {reader} ; <{SCOPE}> {scope} }} }} WHERE {{ GRAPH <{graph}> {{ VALUES ?event {{ {values} }} {members} }} }}",
                    reader=literal(reader), scope=literal(&scope), members=members(item));
                store(inv, &ledger, Verb::Sink, update).await?;
            }
            return answer(
                as_,
                &serde_json::json!({"schema":1,"ledger":ledger.name(),"cursor":cursor}),
                "Acknowledged the supplied event IDs.\n".into(),
            );
        }
        let limit = optional(inv, "limit")?
            .map(|s| {
                s.parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0 && *n <= MAX)
                    .ok_or_else(|| invalid("limit", "expected 1–500"))
            })
            .transpose()?
            .unwrap_or(100);
        // Limit distinct nodes BEFORE expanding their predicates; never cut half an event.
        let query = format!("SELECT ?event ?item ?number ?p ?o WHERE {{ {{ SELECT DISTINCT ?event ?item ?number ?rank WHERE {{ GRAPH <{graph}> {{ {members} FILTER NOT EXISTS {{ <{cursor}> <{SEEN}> ?event }} BIND(IF(EXISTS {{ ?event <{handback}> true }} || EXISTS {{ ?event <{by}> ?by }} || EXISTS {{ ?event <{rejects}> ?rejected }} || EXISTS {{ ?event <{takeover}> ?from }} || EXISTS {{ ?event <{link}> \"blocks\" }}, 0, IF(EXISTS {{ ?event a <{claim}> }}, 2, 1)) AS ?rank) }} }} ORDER BY ?rank ?event LIMIT {limit} }} GRAPH <{graph}> {{ ?event ?p ?o }} }}",
            members=members(item), handback=v::HAND_BACK, by=v::HANDED_BACK_BY, rejects=v::REJECTS, takeover=v::TAKEOVER_FROM, link=v::EDIT_LINK_TYPE, claim=v::CLAIM_EVENT_CLASS, limit=limit+1);
        let rows = select(inv, &ledger, query).await?;
        let mut nodes: BTreeMap<String, Event> = BTreeMap::new();
        for row in rows {
            let event = nodes
                .entry(value(&row, "event").to_string())
                .or_insert_with(|| Event {
                    iri: value(&row, "event").into(),
                    item: value(&row, "item").into(),
                    number: value(&row, "number").parse().unwrap_or_default(),
                    ..Event::default()
                });
            event
                .details
                .entry(value(&row, "p").into())
                .or_default()
                .insert(value(&row, "o").into());
        }
        let mut events: Vec<_> = nodes.into_values().collect();
        for event in &mut events {
            event.classify();
            for p in [
                v::ext::TYPE,
                v::ext::CREATED,
                v::ON_ITEM,
                v::NUMBER,
                v::BODY,
            ] {
                event.details.remove(p);
            }
        }
        events.sort_by(|a, b| (a.rank(), &a.iri).cmp(&(b.rank(), &b.iri)));
        let more = events.len() > limit;
        events.truncate(limit);
        let acknowledge = events.iter().map(|e| e.iri.clone()).collect();
        let claims = events.iter().filter(|e| e.kind == "claim").count();
        events.retain(|e| e.kind != "claim");
        let (attention, more_attention) = attention(inv, &ledger, item).await?;
        let doc = Document {
            schema: 1,
            ledger: ledger.name().into(),
            reader: reader.into(),
            item,
            cursor,
            attention,
            more_attention,
            events,
            folded: serde_json::json!({"claims":claims,"renewals":null}),
            acknowledge,
            more,
            note: NOTE,
        };
        let mut text = format!(
            "{}: unread events for {} ({scope})\n",
            ledger.name(),
            reader
        );
        for attention in &doc.attention {
            text.push_str(&format!("attention: {attention}\n"));
        }
        if doc.more_attention {
            text.push_str("More current conditions are not shown (limit 500).\n");
        }
        for event in &doc.events {
            text.push_str(&format!(
                "#{} {} <{}> {}\n",
                event.number,
                event.kind,
                event.iri,
                event.at.as_deref().unwrap_or("time unknown")
            ));
            if let Some(body) = &event.text {
                text.push_str(body);
                text.push('\n');
            }
            for (label, predicate) in [
                ("author", v::AUTHOR),
                ("committer", v::COMMITTER),
                ("title", v::ext::TITLE),
                ("holder", v::CLAIMED_BY),
                ("handed back by", v::HANDED_BACK_BY),
                ("rejects", v::REJECTS),
                ("takeover from", v::TAKEOVER_FROM),
                ("release reason", v::RELEASE_REASON),
                ("status", v::STATUS),
                ("reason", v::CLOSED_REASON),
                ("link type", v::EDIT_LINK_TYPE),
                ("target", v::EDIT_TARGET),
                ("field", v::EDIT_FIELD),
                ("label", v::EDIT_LABEL),
                ("state from", v::STATE_FROM),
                ("state to", v::STATE_TO),
                ("evidence", v::EVIDENCE),
                ("imported event key", v::EVENT_KEY),
            ] {
                if let Some(values) = event.details.get(predicate) {
                    text.push_str(&format!(
                        "  {label}: {}\n",
                        values.iter().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }
        text.push_str(&format!("Folded: {claims} claims.\n{NOTE}\n"));
        if doc.more {
            text.push_str("More unread events remain; acknowledge this batch and read again.\n");
        }
        text.push_str(&format!(
            "Acknowledge (content=): {}\n",
            serde_json::to_string(&doc.acknowledge).unwrap()
        ));
        answer(as_, &doc, text)
    }
    fn name(&self) -> &str {
        if self.ack {
            "gonk-dispatch-ack"
        } else {
            "gonk-dispatch"
        }
    }
    fn describe(&self) -> Description {
        let mut desc = Description::new(self.name()).title(if self.ack { "Acknowledge a dispatch snapshot" } else { "Unread dispatcher events in one ledger" })
            .summary(format!("Per-reader durable ledger facts, independently scoped by optional item number. Source does not acknowledge; Sink to :ack inserts exactly the returned acknowledge IDs. Concurrent acknowledgments commute. Reader defaults to the capability principal; root must name a reader. Attention is current, not an event log. {NOTE}"))
            .verb(if self.ack { Verb::Sink } else { Verb::Source }).verb(Verb::Meta)
            .requires(ikigai_ledger::CAP_READ).requires(ikigai_store::CAP_READ_GRAPH)
            .input(ArgSpec::new("ledger").class(v::ext::XSD_STRING).binding())
            .input(ArgSpec::new("reader").class(v::ext::XSD_STRING).optional())
            .input(ArgSpec::new("item").class(v::ext::XSD_INTEGER).optional())
            .input(ArgSpec::new("budget").class(v::ext::XSD_INTEGER).optional())
            .input(ArgSpec::new("as").class(v::ext::XSD_STRING).one_of(["text/plain", "application/json"]).default_value("text/plain").optional())
            .output("text/plain").output("application/json");
        if self.ack {
            desc = desc
                .requires(ikigai_ledger::CAP_WRITE)
                .requires(ikigai_store::CAP_WRITE_GRAPH)
                .input(
                    ArgSpec::new("content")
                        .class(v::ext::XSD_STRING)
                        .summary("The JSON acknowledge array from the view, at most 500 IDs."),
                );
        } else {
            desc = desc.input(
                ArgSpec::new("limit")
                    .class(v::ext::XSD_STRING)
                    .summary("1–500 events, default 100; more says another batch remains.")
                    .optional(),
            );
        }
        desc
    }
}

fn answer(as_: &str, json: &impl Serialize, text: String) -> Result<Representation> {
    let bytes = if as_ == "application/json" {
        serde_json::to_vec(json).map_err(|e| Error::Endpoint(e.to_string()))?
    } else {
        text.into_bytes()
    };
    Ok(Representation::new(ReprType::new(as_), bytes).with_expiry(Expiry::Always))
}

/// Current expired claims and open blockers, including the ledger's cross-ledger mirrors.
async fn attention(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    item: Option<i64>,
) -> Result<(Vec<Value>, bool)> {
    let now = ikigai_ledger::sparql::iso8601(crate::agents::now_ms(inv)?);
    let filter = item
        .map(|n| format!("FILTER(?number = {n})"))
        .unwrap_or_default();
    let query = format!("SELECT DISTINCT ?item ?number ?kind ?target WHERE {{ GRAPH <{graph}> {{ ?item <{number}> ?number ; <{status}> <{open}> . {filter} {{ ?item <{expires}> ?target ; <{holder}> ?holder . FILTER(?target <= \"{now}\"^^<{date}>) BIND(\"lapse\" AS ?kind) }} UNION {{ {{ ?target <{blocks}> ?item ; <{status}> <{open}> }} UNION {{ ?mirror a <{mirror}> ; <{on}> ?item ; <{link}> <{blocks}> ; <{present}> true ; <{subject_open}> true ; <{of}> ?target }} BIND(\"blocker\" AS ?kind) }} }} }} ORDER BY ?kind ?number ?target LIMIT 501",
        graph=ledger.graph(), number=v::NUMBER, status=v::STATUS, open=v::OPEN, expires=v::LEASE_EXPIRES, holder=v::CLAIMED_BY, date=v::ext::XSD_DATETIME, blocks=v::BLOCKS, mirror=v::MIRROR_CLASS, on=v::ON_ITEM, link=v::LINK_TYPE, present=v::LINK_PRESENT, subject_open=v::SUBJECT_OPEN, of=v::MIRROR_OF);
    let rows = select(inv, ledger, query).await?;
    let more = rows.len() > MAX;
    let attention = rows.iter().take(MAX).map(|row| serde_json::json!({"item":value(row,"item"), "number":value(row,"number").parse::<i64>().unwrap_or_default(), "kind":value(row,"kind"), "target":value(row,"target")})).collect();
    Ok((attention, more))
}
