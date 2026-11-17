//! Whole-match facets, fetched after the bounded listing has been drawn (#3193).
//!
//! One grouped SELECT, scoped to one ledger graph and the viewer's capability. No item
//! documents or other ledgers are read. `pattern` mirrors the ledger's listing pattern;
//! HTTP tests compare the two over status, text, intersecting labels and exclusions.
//!
//! Under `ledgers=` (ledger #3216) the same SELECT runs once per ledger of the union, and
//! the ledger itself says which those are: its `count` face lists the ledgers it read under
//! the viewer's grants, so the counts cover exactly the listing's rows. An item is in one
//! graph only, so the per-graph counts add up.
//!
//! # The JSON face (ledger #3782)
//!
//! `as=application/json`, or an `Accept` that names it, answers the same counts as a
//! [`FacetDocument`], for a deployment that draws its own pages and would otherwise parse the
//! `.facet-term` markup. It is built from the very map the HTML is drawn from, after the same
//! query and the same display rules, so it can say no count the HTML does not; a browser
//! still gets HTML. Field order is fixed (the order below), absent values are `null`, never
//! missing, and `"schema": 1` versions it as [`ikigai_ledger::json`] versions its documents:
//! adding a field keeps the number, renaming, removing or retyping one changes it.
//!
//! - `ledger`: the resource's bound ledger, even under `ledgers=`;
//! - `ledgers`: under `ledgers=` or `projects=`, the ledgers the counts read (the ledger's
//!   own union for this viewer, as its `count` face lists them); `null` for one ledger;
//! - `labels`: `{"label", "count"}` in the HTML's order;
//! - `priorities`: `{"priority", "count"}` in the HTML's order, `null` for items with none
//!   (the HTML's `unset`).
//!
//! The HTML shows no total (an item has any number of labels, so the label counts do not add
//! up to one), and neither does this face.
//!
//! ```
//! use ikigai_gonk::web::FacetDocument;
//! // `/l/default/facet?ledgers=default,crew&as=application/json`, formatted.
//! let served = r#"{
//!   "schema": 1,
//!   "ledger": "default",
//!   "ledgers": ["crew", "default"],
//!   "labels": [{"label": "shared", "count": 1}, {"label": "team", "count": 3}],
//!   "priorities": [{"priority": 1, "count": 1}, {"priority": null, "count": 2}]
//! }"#;
//! let doc: FacetDocument = serde_json::from_str(served).unwrap();
//! assert_eq!(doc.schema, 1);
//! assert_eq!(doc.labels[1].label, "team");
//! assert_eq!(doc.priorities[1].priority, None);
//! // Absent is `null`, never missing, and the fields keep their order.
//! let one = FacetDocument { ledgers: None, labels: Vec::new(), priorities: Vec::new(), ..doc };
//! assert_eq!(
//!     serde_json::to_string(&one).unwrap(),
//!     r#"{"schema":1,"ledger":"default","ledgers":null,"labels":[],"priorities":[]}"#
//! );
//! ```
use super::*;
use ikigai_ledger::sparql::literal;
use serde::{Deserialize, Serialize};

pub(super) const TEMPLATE: &str = "urn:iki:gonk:fragment:facet:ledger:{ledger}";

const JSON: &str = "application/json";

/// The JSON face's version.
pub const FACET_SCHEMA: u32 = 1;

/// The JSON face of `/l/{ledger}/facet`: the counts the HTML shows, in its order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetDocument {
    /// [`FACET_SCHEMA`].
    pub schema: u32,
    /// The resource's bound ledger name, even when `ledgers=` selects a union.
    pub ledger: String,
    /// The ledgers counted under `ledgers=` or `projects=`; `None` for one ledger.
    pub ledgers: Option<Vec<String>>,
    /// Each label of the match with how many items carry it.
    pub labels: Vec<LabelCount>,
    /// Each priority of the match with how many items have it.
    pub priorities: Vec<PriorityCount>,
}

/// One label's count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelCount {
    /// The label.
    pub label: String,
    /// How many matching items carry it.
    pub count: u64,
}

/// One priority's count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorityCount {
    /// 0 to 4, or `None` for the items with no priority.
    pub priority: Option<u8>,
    /// How many matching items have it.
    pub count: u64,
}

pub(super) struct Facet {
    pub(super) web: Arc<Web>,
}

/// The same base requirements and label/title/status semantics as ikigai-ledger 0.18.
/// Values are SPARQL literals, never query syntax.
fn pattern(listing: &Listing<'_>) -> Result<String> {
    let mut body = format!(
        "?item a <{LEDGER_NS}Item> ; <{LEDGER_NS}number> ?number ; \
         <{DCTERMS}title> ?title ; <{LEDGER_NS}status> ?status .\n"
    );
    match listing.status {
        "open" | "closed" => body.push_str(&format!(
            "?item <{LEDGER_NS}status> <{LEDGER_NS}{}> .\n",
            listing.status
        )),
        "all" => {}
        status => {
            return Err(Error::InvalidArgument {
                name: "status".into(),
                detail: format!("`{status}` is not one of open, closed, all"),
            })
        }
    }
    if let Some(priority) = listing.priority {
        let mut clauses = Vec::new();
        for value in priority.split(',').map(str::trim) {
            clauses.push(match value {
                "none" => format!("NOT EXISTS {{ ?item <{LEDGER_NS}priority> ?filterPriority }}"),
                "0" | "1" | "2" | "3" | "4" => {
                    format!("EXISTS {{ ?item <{LEDGER_NS}priority> {value} }}")
                }
                _ => {
                    return Err(Error::InvalidArgument {
                        name: "priority".into(),
                        detail: "Use comma-separated 0, 1, 2, 3, 4 or none.".into(),
                    })
                }
            });
        }
        body.push_str(&format!("FILTER({})\n", clauses.join(" || ")));
    }
    let labels = LabelFilters::terms(listing.labels);
    if let Some((first, rest)) = labels.split_first() {
        body.push_str(&format!("?item <{LEDGER_NS}label> {} .\n", literal(first)));
        if !rest.is_empty() {
            let wanted = rest
                .iter()
                .map(|t| literal(t))
                .collect::<Vec<_>>()
                .join(" ");
            body.push_str(&format!(
                "FILTER NOT EXISTS {{ VALUES ?wantedLabel {{ {wanted} }} \
                 FILTER NOT EXISTS {{ ?item <{LEDGER_NS}label> ?wantedLabel }} }}\n"
            ));
        }
    }
    for term in LabelFilters::terms(listing.without) {
        body.push_str(&format!(
            "FILTER NOT EXISTS {{ ?item <{LEDGER_NS}label> {} }}\n",
            literal(term)
        ));
    }
    if let Some(text) = listing.text {
        body.push_str(&format!(
            "FILTER(CONTAINS(LCASE(STR(?title)), LCASE({})))\n",
            literal(text)
        ));
    }
    Ok(body)
}

/// The ledgers whose graphs the counts read: the page's own, or under `ledgers=` the ones
/// the ledger's union read for this viewer, from its own `count` face (never chosen here).
/// `None` when no union was asked for.
async fn union(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    listing: &Listing<'_>,
    projects: &crate::projects::Projects,
) -> Result<Option<Vec<Ledger>>> {
    let expanded = projects.resolve(listing.projects, listing.ledgers)?;
    let Some(names) = expanded.as_deref() else {
        return Ok(None);
    };
    if names.is_empty() && listing.projects.is_some_and(|p| !p.trim().is_empty()) {
        return Ok(Some(Vec::new()));
    }
    let read = with(
        with(
            with(
                request(Verb::Source, &format!("{}count", ledger.prefix()))?,
                "as",
                TOTAL_FACE,
            ),
            "status",
            listing.status,
        ),
        "ledgers",
        names,
    );
    ledger_count_document(&fetch(inv, read).await?)?
        .ledgers
        .unwrap_or_default()
        .iter()
        .map(|name| Ledger::parse(name))
        .collect::<Result<_>>()
        .map(Some)
}

async fn query(inv: &Invocation<'_>, ledger: &Ledger, query: &str) -> Result<Vec<u8>> {
    fetch(
        inv,
        with(
            with(
                with(request(Verb::Source, "urn:sparql:select")?, "query", query),
                "graph",
                &ledger.graph(),
            ),
            "as",
            "application/sparql-results+json",
        ),
    )
    .await
}

fn bindings(bytes: &[u8]) -> Result<Vec<Value>> {
    let doc: Value = serde_json::from_slice(bytes)
        .map_err(|e| Error::Endpoint(format!("listing count: {e}")))?;
    doc["results"]["bindings"]
        .as_array()
        .cloned()
        .ok_or_else(|| Error::Endpoint("listing count has no bindings".into()))
}

#[async_trait]
impl Endpoint for Facet {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let json = match inv.inline_str("as").map(bare) {
            Ok(JSON) => true,
            Ok(HTML) | Err(_) => false,
            Ok(asked) => {
                return Err(Error::InvalidArgument {
                    name: "as".to_string(),
                    detail: format!("`{asked}` is not a face this resource serves: {HTML}, {JSON}"),
                })
            }
        };
        let ledger = Ledger::parse(&binding(inv, "ledger")?)?;
        require_read(inv.capability, &ledger)?;
        let status = optional(inv, "status").unwrap_or_else(|| "open".into());
        let text = optional(inv, "text");
        let labels = optional(inv, "labels");
        let without = optional(inv, "without");
        let limit = optional(inv, "limit");
        let ledgers = optional(inv, "ledgers");
        let projects = optional(inv, "projects");
        let priority = optional(inv, "priority");
        let sort = optional(inv, "sort");
        let listing = Listing {
            status: &status,
            text: text.as_deref(),
            labels: labels.as_deref(),
            without: without.as_deref(),
            limit: limit.as_deref(),
            offset: None,
            ledgers: ledgers.as_deref(),
            projects: projects.as_deref(),
            priority: priority.as_deref(),
            sort: sort.as_deref(),
        };
        // Expand each match into label/priority buckets, including an absent priority.
        // The OPTIONALs join the matched items before expansion: an OPTIONAL standing
        // alone in a UNION would omit unset priorities whenever any other item has one.
        // DISTINCT prevents labels or extra property values multiplying an item's count.
        let q = format!(
            "SELECT ?kind ?value (COUNT(DISTINCT ?item) AS ?count) WHERE {{ {} \
             OPTIONAL {{ ?item <{LEDGER_NS}label> ?label }} \
             OPTIONAL {{ ?item <{LEDGER_NS}priority> ?p }} \
             VALUES ?kind {{ \"label\" \"priority\" }} \
             BIND(IF(?kind = \"label\", ?label, COALESCE(STR(?p), \"unset\")) AS ?value) \
             FILTER(BOUND(?value)) \
             }} GROUP BY ?kind ?value ORDER BY ?kind ?value",
            pattern(&listing)?
        );
        let mut counted: BTreeMap<(String, String), u64> = BTreeMap::new();
        let selected = union(inv, &ledger, &listing, &self.web.projects).await?;
        let graphs = selected.clone().unwrap_or_else(|| vec![ledger.clone()]);
        for graph in graphs {
            for row in bindings(&query(inv, &graph, &q).await?)? {
                let kind = row["kind"]["value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let term = row["value"]["value"]
                    .as_str()
                    .ok_or_else(|| Error::Endpoint("facet has no value".into()))?
                    .to_string();
                let count = row["count"]["value"]
                    .as_str()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or_else(|| Error::Endpoint("facet count is not an integer".into()))?;
                if kind == "label"
                    && self
                        .web
                        .display
                        .matching("label", &term, graph.name())
                        .is_some_and(|r| !r.counts)
                {
                    continue;
                }
                *counted.entry((kind, term)).or_default() += count;
            }
        }
        if json {
            return document(&ledger, selected.as_deref(), &counted);
        }
        let filters = LabelFilters {
            labels: listing.labels,
            without: listing.without,
            ledgers: listing.ledgers,
            projects: listing.projects,
            priority: listing.priority,
            sort: listing.sort,
        };
        let page = ledger_href(&ledger);
        // Two lists under their own headings, so a row starts with its term and count and
        // never repeats the kind ("Labels: http (3) Include Exclude").
        let (mut labels, mut priorities) = (Vec::new(), Vec::new());
        for ((kind, term), count) in &counted {
            let term = term.as_str();
            if kind == "priority" {
                // A ledger priority reads `p1` everywhere else on the page; `unset` stays a word.
                let shown = if term.bytes().all(|b| b.is_ascii_digit()) {
                    format!("p{term}")
                } else {
                    term.to_string()
                };
                priorities.push(element(
                    "facet-term",
                    &[
                        ("kind", "priority"),
                        ("term", &shown),
                        ("count", &count.to_string()),
                        ("include", ""),
                        ("exclude", ""),
                    ],
                    "",
                ));
            } else {
                labels.push(element(
                    "facet-term",
                    &[
                        ("kind", "label"),
                        ("term", term),
                        ("count", &count.to_string()),
                        (
                            "include",
                            &filters.url(&listing, &page, LabelChange::Require(term)),
                        ),
                        (
                            "exclude",
                            &filters.url(&listing, &page, LabelChange::Exclude(term)),
                        ),
                    ],
                    "",
                ));
            }
        }
        let empty = labels.is_empty() && priorities.is_empty();
        // As on the item list, bound each transform's input and cache by its bytes.
        // The number of labels is unbounded even when the item page has only one row.
        let chunked = |terms: &[String]| -> Vec<String> {
            terms
                .chunks(render::CHUNK_ROWS)
                .map(|window| render::chunk(&window.join("")))
                .collect()
        };
        let (label_chunks, priority_chunks) = (chunked(&labels), chunked(&priorities));
        let full = optional(inv, "page").as_deref() == Some("true");
        let mut content = if full {
            nav(
                &self.web,
                inv.capability,
                &readable_ledgers(&self.web, inv.capability),
                Some(ledger.name()),
            )
        } else {
            String::new()
        };
        content.push_str(&render::wrap(
            "facet",
            &[
                ("empty", flag(empty)),
                ("labels", flag(!labels.is_empty())),
                ("priorities", flag(!priorities.is_empty())),
            ],
            &format!(
                "{}{}",
                render::slot("facet-label"),
                render::slot("facet-priority")
            ),
        ));
        let doc = envelope(
            "page",
            &[
                ("view", "facet"),
                ("full", flag(full)),
                ("ui-theme", crate::render::theme_of(inv)),
                (
                    "safe",
                    crate::page_assets::flag(crate::page_assets::safe_of(inv)),
                ),
                ("title", "Labels and priority counts"),
                ("back", &filters.url(&listing, &page, LabelChange::Keep)),
            ],
            &content,
        );
        let shell = render::render(&doc, full).map_err(render_err)?;
        let label_rows = render::rendered_chunks(inv, &label_chunks).await?;
        let priority_rows = render::rendered_chunks(inv, &priority_chunks).await?;
        Ok(html(
            render::splice(
                shell,
                &[
                    ("facet-label".into(), label_rows),
                    ("facet-priority".into(), priority_rows),
                ],
            )
            .map_err(render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-fragment-facet"
    }
    fn describe(&self) -> Description {
        Description::new(self.name()).title("Labels and priorities in the whole match")
            .summary("One grouped count over one ledger's filtered items, or a ledgers= union's, under the viewer's grant; independent of the list's page.")
            .verb(Verb::Source).verb(Verb::Meta)
            .requires(ikigai_ledger::CAP_READ).requires(ikigai_store::CAP_READ_GRAPH)
            .input(arg("ledger", "Which ledger.").binding())
            .input(arg("status", "open, closed, or all.").one_of(["open", "closed", "all"]).default_value("open").optional())
            .input(arg("labels", "Comma-separated required labels.").optional())
            .input(arg("without", "Comma-separated excluded labels.").optional())
            .input(arg("priority", "Comma-separated 0, 1, 2, 3, 4 or none.").optional())
            .input(arg("sort", "Preserve the listing sort in filter links.").optional())
            .input(arg("text", "Case-insensitive title substring.").optional())
            .input(arg("projects", "Comma-separated project names, resolved as on the list.").optional())
            .input(arg("ledgers", "Comma-separated ledger names: count over the ledger's own union of them, as the list does.").optional())
            .input(arg("limit", "Preserve this list page size in filter links; counts are unbounded.").optional())
            .input(arg("page", "A whole page for a browser without scripts.").one_of(["true", "false"]).optional())
            .input(arg("as", "The face: text/html, or application/json for the same counts as a document.").one_of([HTML, JSON]).default_value(HTML).optional())
            .output(HTML).output(JSON)
    }
}

/// The JSON face, from the map the HTML would be drawn from: no second query.
fn document(
    ledger: &Ledger,
    selected: Option<&[Ledger]>,
    counted: &BTreeMap<(String, String), u64>,
) -> Result<Representation> {
    let (mut labels, mut priorities) = (Vec::new(), Vec::new());
    for ((kind, term), &count) in counted {
        if kind == "priority" {
            let priority = match term.as_str() {
                "unset" => None,
                n => Some(n.parse::<u8>().map_err(|_| {
                    Error::Endpoint(format!("facet priority `{n}` is not a number"))
                })?),
            };
            priorities.push(PriorityCount { priority, count });
        } else {
            labels.push(LabelCount {
                label: term.clone(),
                count,
            });
        }
    }
    let doc = FacetDocument {
        schema: FACET_SCHEMA,
        ledger: ledger.name().to_string(),
        ledgers: selected.map(|s| s.iter().map(|l| l.name().to_string()).collect()),
        labels,
        priorities,
    };
    let bytes = serde_json::to_vec(&doc).map_err(|e| Error::Endpoint(format!("facet: {e}")))?;
    Ok(Representation::new(ReprType::new(JSON), bytes))
}
