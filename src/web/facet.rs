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
use super::*;
use ikigai_ledger::sparql::literal;

pub(super) const TEMPLATE: &str = "urn:iki:gonk:fragment:facet:ledger:{ledger}";

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
async fn union(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    listing: &Listing<'_>,
) -> Result<Vec<Ledger>> {
    let Some(names) = listing.ledgers else {
        return Ok(vec![ledger.clone()]);
    };
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
        .collect()
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
        html_only(inv)?;
        let ledger = Ledger::parse(&binding(inv, "ledger")?)?;
        require_read(inv.capability, &ledger)?;
        let status = optional(inv, "status").unwrap_or_else(|| "open".into());
        let text = optional(inv, "text");
        let labels = optional(inv, "labels");
        let without = optional(inv, "without");
        let limit = optional(inv, "limit");
        let ledgers = optional(inv, "ledgers");
        let listing = Listing {
            status: &status,
            text: text.as_deref(),
            labels: labels.as_deref(),
            without: without.as_deref(),
            limit: limit.as_deref(),
            offset: None,
            ledgers: ledgers.as_deref(),
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
        for graph in union(inv, &ledger, &listing).await? {
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
                *counted.entry((kind, term)).or_default() += count;
            }
        }
        let filters = LabelFilters {
            labels: listing.labels,
            without: listing.without,
            ledgers: listing.ledgers,
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
            .input(arg("text", "Case-insensitive title substring.").optional())
            .input(arg("ledgers", "Comma-separated ledger names: count over the ledger's own union of them, as the list does.").optional())
            .input(arg("limit", "Preserve this list page size in filter links; counts are unbounded.").optional())
            .input(arg("page", "A whole page for a browser without scripts.").one_of(["true", "false"]).optional())
            .input(as_html_arg()).output(HTML)
    }
}
