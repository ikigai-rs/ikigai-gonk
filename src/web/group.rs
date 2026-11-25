//! Prefix groups: one ledger/label partition, counted afresh under the viewer's grant.
use super::*;
pub(super) const TEMPLATE: &str = "urn:iki:gonk:page:group:ledger:{ledger}:label:{label}";
pub(super) struct Group {
    pub web: Arc<Web>,
}
pub(super) fn href(ledger: &Ledger, label: &str) -> String {
    format!(
        "{}/group/label/{}",
        ledger_href(ledger),
        crate::display::group_key(label)
    )
}

/// Counts cover the whole ledger, including closed members, rather than the listing's
/// current status/filter/page. Never read an item or a graph with a process capability.
pub(super) async fn elements(
    web: &Web,
    inv: &Invocation<'_>,
    ledgers: &[Ledger],
    only: Option<&str>,
) -> Result<String> {
    if !web.display.rules.iter().any(|r| r.group) {
        return Ok(String::new());
    }
    let mut active = String::new();
    let mut closed = String::new();
    for ledger in ledgers {
        require_read(inv.capability, ledger)?;
        let prefixes: Vec<_> = web
            .display
            .rules
            .iter()
            .filter(|r| r.group && r.ledger.as_deref().is_none_or(|l| l == ledger.name()))
            .map(|r| {
                format!(
                    "STRSTARTS(STR(?label), {})",
                    ikigai_ledger::sparql::literal(&r.value)
                )
            })
            .collect();
        if prefixes.is_empty() {
            continue;
        }
        let q = format!("SELECT ?label ?status ?state (COUNT(DISTINCT ?item) AS ?count) WHERE {{ ?item a <{LEDGER_NS}Item>; <{LEDGER_NS}label> ?label; <{LEDGER_NS}status> ?status . OPTIONAL {{ ?item <{LEDGER_NS}state> ?state }} FILTER({}) }} GROUP BY ?label ?status ?state ORDER BY ?label ?status ?state", prefixes.join(" || "));
        let read = with(
            with(
                with(request(Verb::Source, "urn:sparql:select")?, "query", &q),
                "graph",
                &ledger.graph(),
            ),
            "as",
            "application/sparql-results+json",
        );
        let doc: Value = serde_json::from_slice(&fetch(inv, read).await?)
            .map_err(|e| render_err(e.to_string()))?;
        let rows = doc["results"]["bindings"]
            .as_array()
            .ok_or_else(|| render_err("group counts have no bindings".into()))?;
        let mut groups: BTreeMap<String, (u64, u64, BTreeMap<String, u64>)> = BTreeMap::new();
        for row in rows {
            let label = row["label"]["value"]
                .as_str()
                .ok_or_else(|| render_err("group has no label".into()))?;
            if !web.display.grouped(label, ledger.name())
                || only.is_some_and(|wanted| wanted != label)
            {
                continue;
            }
            let count = row["count"]["value"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| render_err("group count is not an integer".into()))?;
            let state = row["state"]["value"]
                .as_str()
                .map(ikigai_ledger::lifecycle::state_name)
                .unwrap_or_else(|| "filed".into());
            let group = groups.entry(label.into()).or_default();
            group.0 += count;
            if row["status"]["value"].as_str() == Some(&format!("{LEDGER_NS}closed")) {
                group.1 += count;
            }
            *group.2.entry(state).or_default() += count;
        }
        for (label, (total, done, states)) in groups {
            let counts = states
                .iter()
                .map(|(s, n)| format!("{s}: {n}"))
                .collect::<Vec<_>>()
                .join("; ");
            let xml = element(
                "label-group",
                &[
                    ("label", &label),
                    ("ledger", ledger.name()),
                    ("href", &href(ledger, &label)),
                    ("counts", &format!("{total} items; {done} closed; {counts}")),
                ],
                "",
            );
            if total == done {
                closed.push_str(&xml);
            } else {
                active.push_str(&xml);
            }
        }
    }
    Ok(render::wrap(
        "groups",
        &[],
        &format!(
            "{}{}",
            render::wrap("group-open", &[], &active),
            render::wrap("group-closed", &[], &closed)
        ),
    ))
}
#[async_trait]
impl Endpoint for Group {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        let ledger = Ledger::parse(&binding(inv, "ledger")?)?;
        require_read(inv.capability, &ledger)?;
        let label = crate::display::group_label(&binding(inv, "label")?).map_err(|detail| {
            Error::InvalidArgument {
                name: "label".into(),
                detail,
            }
        })?;
        if !self.web.display.grouped(&label, ledger.name()) {
            return Err(Error::InvalidArgument {
                name: "label".into(),
                detail: "This label has no grouping rule.".into(),
            });
        }
        let status = optional_nonblank(inv, "status")?.unwrap_or_else(|| "all".into());
        if !matches!(status.as_str(), "all" | "open" | "closed") {
            return Err(Error::InvalidArgument {
                name: "status".into(),
                detail: "Use open, closed or all.".into(),
            });
        }
        let limit = rows::rows_wanted(optional_nonblank(inv, "limit")?.as_deref())?;
        let offset = rows::offset_wanted(optional_nonblank(inv, "offset")?.as_deref())?;
        let status_pattern = if status == "all" {
            String::new()
        } else {
            format!("?item <{LEDGER_NS}status> <{LEDGER_NS}{status}> .")
        };
        let pattern = format!(
            "?item a <{LEDGER_NS}Item>; <{LEDGER_NS}label> {} . {status_pattern}",
            ikigai_ledger::sparql::literal(&label)
        );
        let query = |sparql: &str, face: &str, resource: &str| -> Result<Request> {
            Ok(with(
                with(
                    with(request(Verb::Source, resource)?, "query", sparql),
                    "graph",
                    &ledger.graph(),
                ),
                "as",
                face,
            ))
        };
        let counted: Value = serde_json::from_slice(
            &fetch(
                inv,
                query(
                    &format!("SELECT (COUNT(DISTINCT ?item) AS ?count) WHERE {{ {pattern} }}"),
                    "application/sparql-results+json",
                    "urn:sparql:select",
                )?,
            )
            .await?,
        )
        .map_err(|e| render_err(e.to_string()))?;
        let total = counted["results"]["bindings"][0]["count"]["value"]
            .as_str()
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| render_err("group total is not an integer".into()))?;
        if offset > 0 && offset >= total {
            return Err(Error::InvalidArgument {
                name: "offset".into(),
                detail: format!("offset={offset} is past the {total} matching items"),
            });
        }
        let q = format!("CONSTRUCT {{ ?item ?p ?o }} WHERE {{ {{ SELECT DISTINCT ?item ?modified WHERE {{ {pattern} OPTIONAL {{ ?item <{DCTERMS}modified> ?modified }} }} ORDER BY DESC(?modified) ?item LIMIT {limit} OFFSET {offset} }} ?item ?p ?o }}");
        let mut graph = Graph::from_turtle(
            &fetch(inv, query(&q, "text/turtle", "urn:sparql:construct")?).await?,
        )
        .map_err(render_err)?;
        let (shown, order) = newest_rows(&mut graph, limit);
        enrich_items(&mut graph, &ledger, inv, &status, false);
        enrich_states(&mut graph, inv).await;
        enrich_blocked(vec![&mut graph], inv).await;
        for item in &order {
            graph.view(item, "linked", "true");
        }
        let mut chunks = Vec::new();
        for window in graph.windows(&order, render::CHUNK_ROWS) {
            let xml = item_chips(&window, &self.web.display, |item| {
                LabelFilters::link_chips(&window, item, &ledger)
            })?;
            chunks.push(render::chunk(&xml));
        }
        let page = href(&ledger, &label);
        let next = (offset + shown < total).then(|| {
            format!(
                "{page}?status={status}&limit={limit}&offset={}",
                offset + shown
            )
        });
        let mut children = nav(
            &self.web,
            inv.capability,
            &readable_ledgers(&self.web, inv.capability),
            Some(ledger.name()),
        );
        children.push_str(
            &elements(&self.web, inv, std::slice::from_ref(&ledger), Some(&label)).await?,
        );
        children.push_str(&render::slot(ITEMS_SLOT));
        let doc = envelope("page", &[("view", "group"), ("full", "true"), ("ui-theme", crate::render::theme_of(inv)), ("safe", crate::page_assets::flag(crate::page_assets::safe_of(inv))), ("title", &format!("Group {label} in {}", ledger.name())), ("ledger-url", &ledger_href(&ledger)), ("group-url", &page), ("status", &status), ("count", &format!("Showing {shown} of {total} {status} items. Counts are as of this page load.")), ("next", next.as_deref().unwrap_or(""))], &children);
        let shell = render::render(&doc, true).map_err(render_err)?;
        let items = render::rendered_chunks(inv, &chunks).await?;
        Ok(html(
            render::splice(shell, &[(ITEMS_SLOT.to_string(), items)]).map_err(render_err)?,
        ))
    }
    fn name(&self) -> &str {
        "gonk-page-group"
    }
    fn describe(&self) -> Description {
        Description::new(self.name()).title("A label group").summary("The items of one configured label group, read under the viewer's capability. State counts include closed members and refresh on each request.").verb(Verb::Source).verb(Verb::Meta)
            .requires(ikigai_ledger::CAP_READ).requires(ikigai_store::CAP_READ_GRAPH)
            .input(arg("ledger", "The ledger partition.").binding())
            .input(arg("label", "The full label, encoded as URL-safe base64 without padding.").binding())
            .input(arg("status", "open, closed or all.").optional())
            .input(arg("limit", "The row count.").optional())
            .input(arg("offset", "The offset.").optional())
            .output("text/html")
    }
}
