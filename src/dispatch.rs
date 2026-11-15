//! A compact, acknowledged dispatcher view, per principal and ledger or item (#3712).
//! Reads never advance a cursor. Sink `cursor=` acknowledges exactly one preview, including
//! when new work arrived in between. Cursor hashes live outside the shared RDF dataset.
mod cursors;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cursors::{digest, over_budget, Cursors, Snapshot, MAX_RECORDS};
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Invocation, Iri,
    ReprType, Representation, Request, Result, UriTemplate, Verb,
};
use ikigai_ledger::{
    json::{Item, ItemDocument, ItemsDocument},
    Ledger,
};
use serde_json::{json, Value};

const JSON: &str = "application/json";
const PLAIN: &str = "text/plain";
const NS: &str = "https://ikigai-rs.dev/ns/ledger#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#string";
const MAX_ITEMS: usize = 10_000;
const MAX_FACTS: usize = 262_144;
const MAX_ANSWER: usize = 8 * 1024 * 1024;

/// Construct the family. `path` is a file in an already private server directory; `None`
/// keeps cursors in memory, useful for embedded kernels. Production uses the store directory.
/// Invalid, public or oversized existing files fail startup rather than resetting readers.
pub fn space(path: Option<PathBuf>) -> Result<EndpointSpace> {
    let cursors = Arc::new(Mutex::new(Cursors::open(path)?));
    Ok(EndpointSpace::new()
        .bind(
            UriTemplate::parse("urn:iki:gonk:dispatch:{ledger}:item:{id}")
                .expect("static template"),
            Dispatch {
                cursors: cursors.clone(),
            },
        )
        .bind(
            UriTemplate::parse("urn:iki:gonk:dispatch:{ledger}").expect("static template"),
            Dispatch { cursors },
        ))
}
struct Dispatch {
    cursors: Arc<Mutex<Cursors>>,
}

fn req(target: &str) -> Result<Request> {
    Ok(Request::new(
        Verb::Source,
        Iri::parse(target).map_err(|e| Error::Endpoint(e.to_string()))?,
    ))
}
fn arg(request: Request, key: &str, value: &str) -> Request {
    request.with_arg(key, ArgRef::Inline(value.as_bytes().to_vec()))
}
fn invalid(name: &str, detail: &str) -> Error {
    Error::InvalidArgument {
        name: name.into(),
        detail: detail.into(),
    }
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|e| Error::Endpoint(format!("dispatch: invalid ledger response: {e}")))
}

#[async_trait]
impl Endpoint for Dispatch {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let ledger = Ledger::parse(
            inv.bindings
                .get("ledger")
                .ok_or_else(|| Error::MissingArgument("ledger".into()))?,
        )?;
        if !inv.capability.allows(&ledger.cap_read()) {
            return Err(Error::Denied(format!(
                "dispatch: requires {}",
                ledger.cap_read()
            )));
        }
        let principal = match inv.capability.principal() {
            Some(principal) => principal,
            None if inv.capability.is_root() => "local-owner",
            None => {
                return Err(Error::Denied(
                    "dispatch: a named principal is required for private cursors".into(),
                ))
            }
        };
        let id = inv
            .bindings
            .get("id")
            .map(|id| {
                id.parse::<i64>()
                    .ok()
                    .filter(|id| *id > 0)
                    .ok_or_else(|| invalid("id", "expected a positive item number"))
            })
            .transpose()?;
        // Normalize item numbers, so 01 and 1 cannot create two cursors for one item.
        let scope = format!(
            "{}:{}",
            ledger.name(),
            id.map_or_else(|| "all".into(), |id| id.to_string())
        );
        let key = digest(
            &serde_json::to_vec(&(principal, &scope))
                .map_err(|e| Error::Endpoint(e.to_string()))?,
        );
        let now = crate::agents::now_ms(inv)?;
        match inv.request.verb {
            Verb::Sink => {
                let token = if inv.request.args.contains_key("cursor") {
                    if inv.request.args.contains_key("content") {
                        return Err(invalid("cursor", "send either cursor or content, not both"));
                    }
                    inv.inline_str("cursor")?
                } else {
                    inv.inline_str("content")?
                };
                if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(invalid("cursor", "expected the cursor returned by a look"));
                }
                self.cursors
                    .lock()
                    .map_err(|_| Error::Endpoint("dispatch: cursor lock poisoned".into()))?
                    .acknowledge(&key, token, now)?;
                return Ok(Representation::new(
                    ReprType::new(JSON),
                    br#"{"acknowledged":true}"#.to_vec(),
                ));
            }
            Verb::Source => {}
            verb => {
                return Err(Error::Endpoint(format!(
                    "dispatch: unsupported verb {verb:?}"
                )))
            }
        }
        let face = if inv.request.args.contains_key("as") {
            inv.inline_str("as")?
        } else {
            PLAIN
        };
        if ![JSON, PLAIN].contains(&face) {
            return Err(invalid("as", "expected text/plain or application/json"));
        }
        let baseline = self
            .cursors
            .lock()
            .map_err(|_| Error::Endpoint("dispatch: cursor lock poisoned".into()))?
            .baseline(&key, now);
        let items: Vec<Item> = match id {
            Some(id) => {
                let answer = inv
                    .issue(arg(req(&ledger.item(&id.to_string()))?, "as", JSON))
                    .await?;
                vec![decode::<ItemDocument>(&answer.bytes)?.item]
            }
            None => {
                // One ledger listing, rather than pages whose modified sort keys could move
                // underneath the scan. Oversized store answers are propagated, never partial.
                let answer = inv
                    .issue(arg(
                        arg(
                            arg(
                                req(&format!("urn:iki:ledger:{}:items", ledger.name()))?,
                                "as",
                                JSON,
                            ),
                            "status",
                            "all",
                        ),
                        "limit",
                        &(MAX_ITEMS + 1).to_string(),
                    ))
                    .await?;
                let doc: ItemsDocument = decode(&answer.bytes)?;
                if doc
                    .total
                    .is_none_or(|total| total > MAX_ITEMS || total != doc.items.len())
                    || !doc.unreadable.is_empty()
                {
                    return Err(over_budget());
                }
                doc.items
            }
        };
        if items.len() > MAX_ITEMS {
            return Err(over_budget());
        }
        let facts = facts(
            inv,
            &ledger,
            id.and_then(|_| items.first().map(|item| item.iri.as_str())),
        )
        .await?;
        let now_iso = ikigai_ledger::sparql::iso8601(now);
        let now_time = now_iso
            .parse::<oxsdatatypes::DateTime>()
            .map_err(|e| Error::Endpoint(format!("dispatch: invalid clock: {e}")))?;
        let mut snapshot = Snapshot::new();
        let mut rows = Vec::new();
        for item in &items {
            let mut records = Vec::new();
            let nodes = facts.get(&item.iri);
            let mut blockers = Vec::new();
            let mut handbacks = BTreeSet::new();
            if let Some(nodes) = nodes {
                for (iri, node) in nodes {
                    match node.class.as_str() {
                        "OpenBlocker" => {
                            blockers.extend(node.values("target"));
                        }
                        "Comment" => {
                            if node.first("handBack") == Some("true")
                                || node.first("handedBackBy").is_some()
                            {
                                handbacks.insert(iri);
                            }
                        }
                        class => {
                            let kind = match class {
                                "ClaimEvent" => "claim",
                                "ReleaseEvent" => "release",
                                "StatusEvent"
                                    if node.first("status") == Some(&format!("{NS}closed")) =>
                                {
                                    "closed"
                                }
                                "StatusEvent" => "reopened",
                                _ => node.first("editKind").unwrap_or("edit"),
                            };
                            records.push(json!({"iri": iri, "kind": kind, "at": node.first("created"), "facts": node.facts}));
                        }
                    }
                }
            }
            blockers.sort();
            blockers.dedup();
            for comment in &item.comments {
                let kind = if !comment.rejects.is_empty() {
                    "refusal"
                } else if handbacks.contains(&comment.id) {
                    "handback"
                } else {
                    "comment"
                };
                records.push(json!({"iri": comment.id, "kind": kind, "at": comment.time, "comment": comment}));
            }
            // A digest says that the body changed without repeating its whole text.
            records.push(json!({"kind": "item", "iri": item.iri, "title": item.title, "body_digest": digest(item.body.as_bytes()), "status": item.status, "closed_reason": item.closed_reason, "state": item.state, "awaiting": item.awaiting, "priority": item.priority, "deferred": item.deferred, "labels": item.labels, "about": item.about, "links": item.links}));
            records.push(json!({"kind": "blockers", "iri": item.iri, "open": blockers}));
            if let Some(claim) = &item.claim {
                // Renewals have no event in this ledger version. This is one observed
                // current-claim change, never a claimed count of historical renewals.
                records.push(json!({"kind": "claim_update", "iri": item.iri, "claim": claim}));
                if claim.expires.as_deref().is_some_and(|expires| {
                    expires
                        .parse::<oxsdatatypes::DateTime>()
                        .is_ok_and(|expires| expires <= now_time)
                }) {
                    records.push(json!({"kind": "lapse", "iri": item.iri, "claim": claim}));
                }
            }
            let mut attention = Vec::new();
            let mut events = Vec::new();
            let mut routine = BTreeMap::<String, usize>::new();
            for record in records {
                let hash = digest(
                    &serde_json::to_vec(&record).map_err(|e| Error::Endpoint(e.to_string()))?,
                );
                snapshot.insert(hash.clone());
                if snapshot.len() > MAX_RECORDS {
                    return Err(over_budget());
                }
                if baseline.as_ref().is_some_and(|seen| seen.contains(&hash)) {
                    continue;
                }
                let kind = record["kind"].as_str().unwrap_or("event");
                match kind {
                    "claim" | "claim_update" => {
                        *routine.entry(kind.into()).or_default() += 1;
                    }
                    "handback" | "refusal" | "lapse" => attention.push(record),
                    "release"
                        if record["facts"]["releaseReason"]
                            .as_array()
                            .is_some_and(|values| {
                                values.iter().any(|v| v["value"] == "takeover")
                            }) =>
                    {
                        attention.push(record)
                    }
                    "blockers" if !blockers.is_empty() => attention.push(record),
                    "link" | "unlink"
                        if record["facts"]["editLinkType"]
                            .as_array()
                            .is_some_and(|values| {
                                values.iter().any(|v| v["value"] == format!("{NS}blocks"))
                            }) =>
                    {
                        attention.push(record)
                    }
                    _ => events.push(record),
                }
            }
            let order = |a: &Value, b: &Value| {
                (
                    a["at"].as_str().unwrap_or(""),
                    a["iri"].as_str().unwrap_or(""),
                )
                    .cmp(&(
                        b["at"].as_str().unwrap_or(""),
                        b["iri"].as_str().unwrap_or(""),
                    ))
            };
            attention.sort_by(order);
            events.sort_by(order);
            if !attention.is_empty() || !events.is_empty() || !routine.is_empty() {
                rows.push(json!({"iri": item.iri, "number": item.number, "title": item.title, "status": item.status, "awaiting": item.awaiting, "claim": item.claim, "blockers": blockers, "attention": attention, "events": events, "routine": routine}));
            }
        }
        rows.sort_by_key(|row| {
            (
                row["attention"].as_array().is_none_or(Vec::is_empty),
                row["number"].as_i64(),
            )
        });
        let mut document = json!({"schema": 1, "ledger": ledger.name(), "item": id, "cursor_reset": baseline.is_none(), "items": rows, "note": "Routine claim_update counts are observed current-claim changes, not individual renewals. Refusals are recorded hand-back rejections; failed transport requests are not ledger events."});
        // Check the answer before retaining a preview. The token adds a fixed 64 bytes.
        if serde_json::to_vec(&document)
            .map_err(|e| Error::Endpoint(e.to_string()))?
            .len()
            + 128
            > MAX_ANSWER
        {
            return Err(over_budget());
        }
        let token = self
            .cursors
            .lock()
            .map_err(|_| Error::Endpoint("dispatch: cursor lock poisoned".into()))?
            .stage(&key, &baseline, snapshot, now)?;
        document["cursor"] = token.into();
        let bytes = if face == JSON {
            serde_json::to_vec(&document).map_err(|e| Error::Endpoint(e.to_string()))?
        } else {
            text(&document).into_bytes()
        };
        if bytes.len() > MAX_ANSWER {
            return Err(over_budget());
        }
        Ok(Representation::new(ReprType::new(face), bytes))
    }
    fn name(&self) -> &str {
        "gonk-dispatch"
    }
    fn describe(&self) -> Description {
        let binding = |name: &str| ArgSpec::new(name).class(XSD).binding();
        Description::new(self.name())
            .title("Changes since this reader's acknowledged dispatcher look")
            .summary("urn:iki:gonk:dispatch:{ledger}[:item:{id}]. Private per-principal cursors; Source previews changes, Sink cursor= acknowledges exactly that preview. Hand-backs, rejections, expired current claims and open blockers first; routine claims folded into counts. Named readers need the ledger and graph read grants. Only root may use the unnamed local-owner cursor. Private bounded sidecar, never shared ledger facts: 128 scopes, 30 days idle, 262144 hashes total; four pending previews per scope for 15 minutes, not retained on restart. Eviction resets to a full look explicitly. At most 10000 items, 262144 event facts, 65536 records, 8 MiB per answer; oversized or unreadable sets refuse without advancing. Ledger and item cursors are independent. No individual renewal history exists: claim_update counts observed current-claim changes. Refusals mean stored hand-back rejections, not failed requests.")
            .verb(Verb::Meta)
            .action(ActionSpec::new(Verb::Source).requires("urn:cap:ledger:read:*")
                .input(binding("ledger")).input(binding("id").optional())
                .input(ArgSpec::new("as").class(XSD).optional().one_of([PLAIN, JSON]).default_value(PLAIN))
                .output(PLAIN).output(JSON))
            .action(ActionSpec::new(Verb::Sink).requires("urn:cap:ledger:read:*")
                .input(binding("ledger")).input(binding("id").optional())
                .input(ArgSpec::new("content").class(XSD).optional().summary("the opaque token returned by a successful look; cursor= is an alias"))
                .input(ArgSpec::new("cursor").class(XSD).optional().summary("alias for content; send one token input"))
                .output(JSON))
    }
}

#[derive(Default)]
struct Node {
    class: String,
    facts: BTreeMap<String, Vec<Value>>,
}
impl Node {
    fn first(&self, key: &str) -> Option<&str> {
        self.facts.get(key)?.first()?["value"].as_str()
    }
    fn values(&self, key: &str) -> Vec<String> {
        self.facts
            .get(key)
            .into_iter()
            .flatten()
            .filter_map(|v| v["value"].as_str().map(str::to_string))
            .collect()
    }
}
async fn facts(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    item_iri: Option<&str>,
) -> Result<BTreeMap<String, BTreeMap<String, Node>>> {
    let restrict = item_iri
        .map(|iri| {
            oxigraph::model::NamedNode::new(iri)
                .map(|iri| format!("VALUES ?item {{ {iri} }}"))
                .map_err(|e| Error::Endpoint(format!("dispatch: invalid item identity: {e}")))
        })
        .transpose()?
        .unwrap_or_default();
    // One scoped query, no per-item history fan-out. Cross-ledger mirrors expose only the
    // source IRI and open status already visible in this ledger, never a foreign title.
    let query = format!("PREFIX l: <{NS}> SELECT ?item ?event ?class ?p ?o WHERE {{ {restrict} \
        {{ ?event l:onItem ?item ; a ?class ; ?p ?o . \
        FILTER(?class IN (l:ClaimEvent, l:ReleaseEvent, l:StatusEvent, l:EditEvent, l:Comment)) \
        FILTER(?class != l:Comment || ?p IN (l:handBack, l:handedBackBy)) }} UNION \
        {{ ?event l:blocks ?item ; l:status l:open . BIND(l:OpenBlocker AS ?class) BIND(l:target AS ?p) BIND(?event AS ?o) }} UNION \
        {{ ?mirror a l:Mirror ; l:onItem ?item ; l:linkType l:blocks ; l:linkPresent true ; l:subjectOpen true ; l:mirrorOf ?event . \
        BIND(l:OpenBlocker AS ?class) BIND(l:target AS ?p) BIND(?event AS ?o) }} \
        }} ORDER BY ?item ?event ?p ?o LIMIT {}", MAX_FACTS + 1);
    let answer = inv
        .issue(arg(
            arg(
                arg(req("urn:iki:store:graph-select")?, "graph", &ledger.graph()),
                "query",
                &query,
            ),
            "as",
            "application/sparql-results+json",
        ))
        .await?;
    let value: Value = decode(&answer.bytes)?;
    let rows = value["results"]["bindings"]
        .as_array()
        .ok_or_else(|| Error::Endpoint("dispatch: invalid event facts".into()))?;
    if rows.len() > MAX_FACTS {
        return Err(over_budget());
    }
    let mut nodes = BTreeMap::<String, BTreeMap<String, Node>>::new();
    for row in rows {
        let term = |name: &str| {
            row[name]["value"]
                .as_str()
                .ok_or_else(|| Error::Endpoint(format!("dispatch: missing event fact {name}")))
        };
        let item = term("item")?;
        let iri = term("event")?;
        let class = term("class")?.strip_prefix(NS).unwrap_or(term("class")?);
        let predicate = term("p")?;
        let short = predicate
            .strip_prefix(NS)
            .or_else(|| predicate.strip_prefix("http://purl.org/dc/terms/"))
            .unwrap_or(predicate);
        let node = nodes
            .entry(item.into())
            .or_default()
            .entry(iri.into())
            .or_default();
        node.class = class.into();
        let values = node.facts.entry(short.into()).or_default();
        if !values.contains(&row["o"]) {
            values.push(row["o"].clone());
        }
    }
    Ok(nodes)
}
fn text(document: &Value) -> String {
    let mut out = format!(
        "{}: changes since the last acknowledged look\n",
        document["ledger"].as_str().unwrap_or("ledger")
    );
    if document["cursor_reset"] == true {
        out.push_str(
            "Cursor reset: showing a full look (first read, expired or evicted cursor).\n",
        );
    }
    if let Some(rows) = document["items"].as_array() {
        if rows.is_empty() {
            out.push_str("No changes.\n");
        }
        for row in rows {
            out.push_str(&format!(
                "\n#{} {}\nStatus: {}; awaiting: {}; blockers: {}\n",
                row["number"],
                row["title"].as_str().unwrap_or(""),
                row["status"],
                row["awaiting"],
                row["blockers"]
            ));
            for section in ["attention", "events"] {
                for event in row[section].as_array().into_iter().flatten() {
                    out.push_str(&format!(
                        "{}: {}\n",
                        event["kind"].as_str().unwrap_or("event"),
                        event
                    ));
                }
            }
            out.push_str(&format!("Routine counts: {}\n", row["routine"]));
        }
    }
    out.push_str(&format!(
        "\n{}\nAcknowledge with Sink cursor={}\n",
        document["note"].as_str().unwrap_or(""),
        document["cursor"].as_str().unwrap_or("")
    ));
    out
}
