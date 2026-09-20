//! `cargo run --release --example render-cost -- <items.ttl> [rows|all]` — split the ledger
//! list page's render cost into its stages, over a REAL graph face dumped from a live ledger:
//!
//! ```sh
//! curl -H 'Accept: text/turtle' \
//!   'http://localhost:1060/iki/ledger/items?status=open&limit=2000' > /tmp/items.ttl
//! cargo run --release --example render-cost -- /tmp/items.ttl all   # what the page cost
//! cargo run --release --example render-cost -- /tmp/items.ttl 50    # what it costs now
//! ```
//!
//! # ★ What this measured, and what it ruled out (ledger #443)
//!
//! The page cost ~6 s and the question was which stage. Over 410 open items, 798 KB of
//! Turtle:
//!
//! ```text
//! parse Turtle          5 ms
//! view triples         52 ms
//! RDF/XML              13 ms
//! xrust XSLT          6.5 s     ← 99%
//! ```
//!
//! ⚠ **There is no transreption in that path at all.** `urn:rdf:transrept` is never issued:
//! `render::Graph` parses the Turtle with oxigraph and writes RDF/XML with oxrdfxml, in
//! process, and the two together are 18 ms. A measurement taken through a second `ikigai`
//! process would have been answering a different question.
//!
//! ⚠ **And a smaller INPUT does not help.** Projecting the graph down to the ten predicates
//! the row template reads shrank the RDF/XML 3.4× (1.96 MB → 576 KB) and moved the render
//! by 3%. xrust's cost is in the result tree it BUILDS: the same 410 rows cost 2.4 s for an
//! anonymous caller and 6.5 s for one who may close them, because the second renders a form
//! per row. So the only lever is fewer rows, which is what `web::ROWS` bounds.
//!
//! The enrichment below MIRRORS `web::enrich_items` rather than calling it (that function is
//! private and wants an `Invocation`); it is kept faithful — down to `can-write`, which is
//! true for gonk's default loopback grant — so the stage sum reconstructs the page's measured
//! latency. It did: 367,438 bytes of HTML against the live page's 368,501.

use std::collections::HashSet;
use std::time::Instant;

use ikigai_gonk::render::{self, envelope, Graph};
use oxigraph::model::NamedOrBlankNode;

const LEDGER_NS: &str = "https://ikigai-rs.dev/ns/ledger#";
const DCTERMS: &str = "http://purl.org/dc/terms/";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: render-cost <items.ttl> [rows|all]");
    let rows = match args.next().as_deref() {
        None | Some("all") => usize::MAX,
        Some(n) => n.parse().expect("a number of rows, or `all`"),
    };
    let bytes = std::fs::read(&path).expect("a readable Turtle file");
    println!("input            {:>12} bytes", bytes.len());

    let whole = Instant::now();

    let t = Instant::now();
    let mut graph = Graph::from_turtle(&bytes).expect("Turtle");
    println!(
        "parse            {:>9.1?}   {} triples",
        t.elapsed(),
        graph.triples().len()
    );

    let t = Instant::now();
    let (shown, total) = newest_rows(&mut graph, rows);
    println!(
        "bound to {shown:<5}   {:>9.1?}   {} triples of {total} items",
        t.elapsed(),
        graph.triples().len()
    );

    let t = Instant::now();
    enrich(&mut graph);
    println!(
        "enrich           {:>9.1?}   {} triples",
        t.elapsed(),
        graph.triples().len()
    );

    let t = Instant::now();
    let xml = graph.rdfxml().expect("RDF/XML");
    println!(
        "rdfxml           {:>9.1?}   {} bytes",
        t.elapsed(),
        xml.len()
    );

    let count = format!("showing the {shown} most recently updated of {total} open items");
    let attrs = [
        ("view", "ledger"),
        ("full", "true"),
        ("title", "Ledger"),
        ("ledger", "default"),
        ("status", "open"),
        ("text", ""),
        ("page-url", "/l/default"),
        ("items-url", "/l/default/items"),
        ("count", count.as_str()),
        ("more", "true"),
        ("more-label", "Show all"),
        ("more-url", "/l/default?status=open&limit=500"),
        ("more-items-url", "/l/default/items?status=open&limit=500"),
        ("can-write", "true"),
    ];
    let t = Instant::now();
    let chrome = render::render(&envelope("page", &attrs, ""), true).expect("the stylesheet");
    println!(
        "xslt: no items   {:>9.1?}   {} bytes  (compile + chrome)",
        t.elapsed(),
        chrome.len()
    );

    let doc = envelope("page", &attrs, &xml);
    let t = Instant::now();
    let page = render::render(&doc, true).expect("the stylesheet renders");
    let xslt = t.elapsed();
    println!("xslt + html      {:>9.1?}   {} bytes", xslt, page.len());
    println!("-----");
    println!(
        "total            {:>9.1?}   of which xslt {:.1?}",
        whole.elapsed(),
        xslt
    );
    if let Some(out) = std::env::var_os("GONK_RENDER_COST_OUT") {
        std::fs::write(out, &page).expect("a writable output path");
    }
}

/// A mirror of `web::newest_rows`: keep the `rows` most recently updated items and drop
/// every other subject.
fn newest_rows(graph: &mut Graph, rows: usize) -> (usize, usize) {
    let mut items: Vec<(String, String)> = graph
        .subjects_of_type(&format!("{LEDGER_NS}Item"))
        .into_iter()
        .map(|iri| {
            let modified = graph
                .value(&iri, &format!("{DCTERMS}modified"))
                .unwrap_or_default();
            (modified, iri)
        })
        .collect();
    let total = items.len();
    items.sort_by(|a, b| b.cmp(a));
    let keep: HashSet<String> = items.into_iter().take(rows).map(|(_, iri)| iri).collect();
    let shown = keep.len();
    graph.retain(
        |t| matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if keep.contains(s.as_str())),
    );
    (shown, total)
}

/// A faithful mirror of `web::enrich_items` for a caller who may write.
fn enrich(graph: &mut Graph) {
    for item in graph.subjects_of_type(&format!("{LEDGER_NS}Item")) {
        let id = item
            .rsplit_once(":item:")
            .map(|(_, id)| id)
            .unwrap_or(&item)
            .to_string();
        let number = graph
            .value(&item, &format!("{LEDGER_NS}number"))
            .unwrap_or_default();
        let open = graph
            .value(&item, &format!("{LEDGER_NS}status"))
            .is_some_and(|s| s == format!("{LEDGER_NS}open"));
        let reason = graph.value(&item, &format!("{LEDGER_NS}closedReason"));
        let priority = graph
            .value(&item, &format!("{LEDGER_NS}priority"))
            .map(|p| format!("p{p}"))
            .unwrap_or_else(|| "p-".to_string());
        let kind = graph
            .values(&item, RDF_TYPE)
            .into_iter()
            .find(|t| t != &format!("{LEDGER_NS}Item"));
        let title = graph
            .value(&item, &format!("{DCTERMS}title"))
            .unwrap_or_default();
        let body = graph
            .value(&item, &format!("{LEDGER_NS}body"))
            .unwrap_or_default();
        let created = graph
            .value(&item, &format!("{DCTERMS}created"))
            .unwrap_or_default();
        let modified = graph
            .value(&item, &format!("{DCTERMS}modified"))
            .unwrap_or_default();

        graph.view(&item, "iri", item.clone());
        graph.view(&item, "id", id.clone());
        graph.view(&item, "short", format!("#{number}"));
        graph.view(&item, "href", format!("/l/default/item/{id}"));
        graph.view(&item, "ledger", "default");
        graph.view(&item, "ledgerHref", "/l/default");
        graph.view(&item, "listStatus", "open");
        graph.view(&item, "status", if open { "open" } else { "closed" });
        if let Some(reason) = reason {
            graph.view(&item, "reason", reason);
        }
        graph.view(&item, "priority", priority);
        graph.view(&item, "deferred", "false");
        if let Some(kind) = kind {
            graph.view(&item, "kind", kind);
        }
        graph.view(&item, "created", created);
        graph.view(&item, "modified", modified);
        graph.view(
            &item,
            "content",
            if body.is_empty() {
                title
            } else {
                format!("{title}\n\n{body}")
            },
        );
        graph.view(&item, "canWrite", "true");
        graph.view(&item, "canDelete", "true");
        graph.view(&item, "canPurge", "true");

        let mut order = 0;
        for rel in ["blocks", "parent", "related", "about"] {
            for target in graph.values(&item, &format!("{LEDGER_NS}{rel}")) {
                order += 1;
                let node = format!("urn:iki:gonk:view:link:{id}:{order}");
                graph.typed(&node, &format!("{}Link", render::VIEW_NS));
                graph.view(&node, "order", format!("{order:04}"));
                graph.view(&node, "rel", rel);
                graph.view(&node, "ledger", "default");
                graph.view(&node, "id", id.clone());
                graph.view(&node, "item", item.clone());
                graph.view(&node, "target", target.clone());
                graph.view(&node, "text", target.clone());
                graph.view(&node, "canUnlink", "true");
            }
        }
    }
    for comment in graph.subjects_of_type(&format!("{LEDGER_NS}Comment")) {
        let created = graph
            .value(&comment, &format!("{DCTERMS}created"))
            .unwrap_or_default();
        graph.view(&comment, "created", created);
    }
}
