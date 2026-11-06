//! Whole-match facets, fetched after the bounded listing has been drawn (#3193).
//!
//! One grouped SELECT, scoped to one ledger graph and the viewer's capability. No item
//! documents or other ledgers are read. `pattern` mirrors the ledger's listing pattern;
//! HTTP tests compare the two over status, text, intersecting labels and exclusions.
use super::*;
use ikigai_ledger::sparql::literal;

pub(super) const TEMPLATE: &str = "urn:iki:gonk:fragment:facet:ledger:{ledger}";

pub(super) struct Facet {
    pub(super) web: Arc<Web>,
}

fn priority_term(value: &str) -> Result<(&str, bool)> {
    let (term, exclude) = value
        .strip_prefix('!')
        .map_or((value, false), |v| (v, true));
    if !matches!(term, "0" | "1" | "2" | "3" | "4" | "unset") {
        return Err(Error::InvalidArgument {
            name: "priority".into(),
            detail: "use 0–4 or unset; prefix with ! to exclude that priority".into(),
        });
    }
    Ok((term, exclude))
}

/// The same base requirements and label/title/status semantics as ikigai-ledger 0.18.
/// Values are SPARQL literals, never query syntax. Priority is this page's extra filter.
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
    if let Some(value) = listing.priority.filter(|v| !v.is_empty()) {
        let (term, exclude) = priority_term(value)?;
        let exists = if (term == "unset") != exclude {
            "NOT EXISTS"
        } else {
            "EXISTS"
        };
        let object = if term == "unset" {
            "?filteredPriority"
        } else {
            term
        };
        body.push_str(&format!(
            "FILTER {exists} {{ ?item <{LEDGER_NS}priority> {object} }}\n"
        ));
    }
    Ok(body)
}

async fn query(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    query: &str,
    construct: bool,
) -> Result<Vec<u8>> {
    let (resource, face) = if construct {
        ("urn:sparql:construct", "text/turtle")
    } else {
        ("urn:sparql:select", "application/sparql-results+json")
    };
    fetch(
        inv,
        with(
            with(
                with(request(Verb::Source, resource)?, "query", query),
                "graph",
                &ledger.graph(),
            ),
            "as",
            face,
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

/// Priority-filtered pages use the same pattern as the facets. Other pages retain the
/// ledger's existing count/items resources. Page in subjects before loading their triples.
pub(super) async fn total(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    listing: &Listing<'_>,
) -> Result<usize> {
    let q = format!(
        "SELECT (COUNT(DISTINCT ?item) AS ?count) WHERE {{ {} }}",
        pattern(listing)?
    );
    let rows = bindings(&query(inv, ledger, &q, false).await?)?;
    rows.first()
        .and_then(|r| r["count"]["value"].as_str())
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| Error::Endpoint("listing total is not an integer".into()))
}

pub(super) async fn items(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    listing: &Listing<'_>,
    limit: usize,
    offset: usize,
) -> Result<Vec<u8>> {
    let q = format!(
        "CONSTRUCT {{ ?item ?p ?o }} WHERE {{ \
         {{ SELECT ?item (MAX(COALESCE(?modified, ?created, \"\")) AS ?recency) (MAX(?number) AS ?n) \
         WHERE {{ {} OPTIONAL {{ ?item <{DCTERMS}modified> ?modified }} \
         OPTIONAL {{ ?item <{DCTERMS}created> ?created }} }} \
         GROUP BY ?item ORDER BY DESC(?recency) DESC(?n) LIMIT {limit} OFFSET {offset} }} \
         ?item ?p ?o }}", pattern(listing)?
    );
    query(inv, ledger, &q, true).await
}

pub(super) fn priority_url(listing: &Listing<'_>, page: &str, value: &str) -> String {
    let filters = LabelFilters {
        labels: listing.labels,
        without: listing.without,
    };
    let base = Listing {
        priority: None,
        ..*listing
    };
    let url = filters.url(&base, page, LabelChange::Keep);
    if value.is_empty() {
        url
    } else {
        format!("{url}&priority={}", ikigai_fn::html::url_segment(value))
    }
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
        let priority = optional(inv, "priority");
        let limit = optional(inv, "limit");
        let listing = Listing {
            status: &status,
            text: text.as_deref(),
            labels: labels.as_deref(),
            without: without.as_deref(),
            priority: priority.as_deref(),
            limit: limit.as_deref(),
            offset: None,
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
        let rows = bindings(&query(inv, &ledger, &q, false).await?)?;
        let filters = LabelFilters {
            labels: listing.labels,
            without: listing.without,
        };
        let page = ledger_href(&ledger);
        let mut terms = Vec::new();
        for row in rows {
            let term = row["value"]["value"]
                .as_str()
                .ok_or_else(|| Error::Endpoint("facet has no value".into()))?;
            let count = row["count"]["value"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| Error::Endpoint("facet count is not an integer".into()))?;
            let (name, include, exclude) = if row["kind"]["value"] == "priority" {
                // Reject invalid out-of-band priorities rather than inventing a filter.
                priority_term(term)?;
                (
                    format!("Priority {term}"),
                    priority_url(&listing, &page, term),
                    priority_url(&listing, &page, &format!("!{term}")),
                )
            } else {
                (
                    format!("Label {term}"),
                    filters.url(&listing, &page, LabelChange::Require(term)),
                    filters.url(&listing, &page, LabelChange::Exclude(term)),
                )
            };
            terms.push(element(
                "facet-term",
                &[
                    ("name", &name),
                    ("count", &count.to_string()),
                    ("include", &include),
                    ("exclude", &exclude),
                ],
                "",
            ));
        }
        let empty = terms.is_empty();
        // As on the item list, bound each transform's input and cache by its bytes.
        // The number of labels is unbounded even when the item page has only one row.
        let chunks: Vec<String> = terms
            .chunks(render::CHUNK_ROWS)
            .map(|window| render::chunk(&window.join("")))
            .collect();
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
            &[("empty", flag(empty))],
            &render::slot("facet-term"),
        ));
        let doc = envelope(
            "page",
            &[
                ("view", "facet"),
                ("full", flag(full)),
                ("ui-theme", crate::render::theme_of(inv)),
                ("title", "Labels and priority counts"),
                ("back", &filters.url(&listing, &page, LabelChange::Keep)),
            ],
            &content,
        );
        let shell = render::render(&doc, full).map_err(render_err)?;
        let terms = render::rendered_chunks(inv, &chunks).await?;
        Ok(html(
            render::splice(shell, &[("facet-term".into(), terms)]).map_err(render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-fragment-facet"
    }
    fn describe(&self) -> Description {
        Description::new(self.name()).title("Labels and priorities in the whole match")
            .summary("One grouped count over one ledger's filtered items, under the viewer's grant; independent of the list's page.")
            .verb(Verb::Source).verb(Verb::Meta)
            .requires(ikigai_ledger::CAP_READ).requires(ikigai_store::CAP_READ_GRAPH)
            .input(arg("ledger", "Which ledger.").binding())
            .input(arg("status", "open, closed, or all.").one_of(["open", "closed", "all"]).default_value("open").optional())
            .input(arg("labels", "Comma-separated required labels.").optional())
            .input(arg("without", "Comma-separated excluded labels.").optional())
            .input(arg("text", "Case-insensitive title substring.").optional())
            .input(arg("priority", "0–4 or unset, optionally prefixed with ! to exclude.").optional())
            .input(arg("limit", "Preserve this list page size in filter links; counts are unbounded.").optional())
            .input(arg("page", "A whole page for a browser without scripts.").one_of(["true", "false"]).optional())
            .input(as_html_arg()).output(HTML)
    }
}
