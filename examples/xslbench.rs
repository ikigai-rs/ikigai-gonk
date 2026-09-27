use std::fmt::Write as _;
use std::time::Instant;

use ikigai_gonk::render::STYLESHEET;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

/// A synthetic queue page: `n` finding rows under one root, shaped like the rows
/// `src/queue.rs` builds (attributes the `view:finding` template reads, a body, a quote,
/// a decide form with its option lists).
/// `shape` (the second argument) is the bisection of ledger #519's step 3 — what a row
/// holds: `attrs` (the finding element and its attributes, one template dispatch per row),
/// `bigbody` (attrs plus a ~2 KB body: many bytes, few nodes), `nodecide` (attrs, body and
/// quote), `full` (the decide form and its option lists too: ~20 dispatches per row).
fn page(n: usize, container: &str, shape: &str) -> String {
    let mut s = String::new();
    s.push_str(r#"<view:page xmlns:view="urn:iki:gonk:view#" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:ledger="https://ikigai-rs.dev/ns/ledger#" view="queue" full="true" title="Queue" page-url="/queue">"#);
    s.push_str(r#"<view:ledger name="default" href="/l/default" current="true"/>"#);
    s.push_str(r#"<view:queue href="/queue" depth-url="/queue/depth" every="10s"/>"#);
    if container != "none" {
        let _ = write!(s, "<view:{container} title=\"Review queue\" message=\"{n} findings\" refresh-url=\"/queue?rows\" news=\"queue-news\">");
    }
    s.push_str(r#"<view:state name="pending" href="/queue" rows-url="/queue?rows" current="true"/><view:state name="published" href="/queue?state=published" rows-url="/queue?state=published&amp;rows"/>"#);
    s.push_str(r#"<view:scope name="serious" href="/queue" rows-url="/queue?rows" label="serious only" current="true"/>"#);
    for i in 0..n {
        let _ = write!(
            s,
            r#"<view:finding id="f{i:06}" state="pending" severity="major" severity-label="major" repo="ikigai-cli" where="crates/ikigai-embedded/src/lib.rs:{line}" browse-href="/browse/urn:repo:ikigai-cli:file:crates/ikigai-embedded/src/lib.rs" provenance="minted by review-v5@coder · 2026-09-25 18:04 UTC · pass 01m3cvtjzmyby3f6" reanchored="false" orphaned="false">"#,
            line = 100 + i * 7
        );
        let body = "The comment above documents a hazard that the code below does not guard against: a caller holding no authority reaches the write path when the host name is unset, and nothing in this branch refuses it. Consider checking the origin before the capability is minted, or say in the doc comment why the ordering is safe.";
        if shape == "attrs" {
            s.push_str("</view:finding>");
            continue;
        }
        if shape == "bigbody" {
            let _ = write!(
                s,
                "<view:body>{}</view:body></view:finding>",
                esc(&body.repeat(6))
            );
            continue;
        }
        let _ = write!(s, "<view:body>{}</view:body>", esc(body));
        let _ = write!(
            s,
            "<view:quote>{}</view:quote>",
            esc("    if !host_is_ours(request.header(\"host\"), door.port) {")
        );
        if shape == "nodecide" {
            s.push_str("</view:finding>");
            continue;
        }
        let _ = write!(
            s,
            r#"<view:decide action="/queue/decide" id="f{i:06}" state="pending" repo="ikigai-cli" severity="major" scope="serious">"#
        );
        for (w, sel) in [
            ("critical", ""),
            ("major", " selected='true'"),
            ("minor", ""),
            ("info", ""),
            ("praise", ""),
        ] {
            let _ = write!(
                s,
                "<view:severity-option value=\"{w}\"{sel}>{w}</view:severity-option>"
            );
        }
        for w in ["publish", "decline"] {
            let _ = write!(
                s,
                "<view:decision-option value=\"{w}\">{w}</view:decision-option>"
            );
        }
        for w in ["misread", "restates", "no-issue", "wont-fix", "duplicate"] {
            let _ = write!(
                s,
                "<view:reason-option value=\"{w}\" title=\"why\">{w}</view:reason-option>"
            );
        }
        s.push_str("</view:decide></view:finding>");
    }
    if container != "none" {
        let _ = write!(s, "</view:{container}>");
    }
    s.push_str("</view:page>");
    s
}

fn main() {
    // ⚠ `none`: the rows are direct children of the page, as src/queue.rs builds them. Under
    // `root` (any container) the queue template applies nothing and the output is the
    // shell alone — 2 KB — which is a measurement of nothing.
    let container = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "none".to_string());
    let shape = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "full".to_string());
    // warm the stylesheet compile cache
    let _ = ikigai_xslt::transform_xml(&page(1, &container, &shape), STYLESHEET, false)
        .expect("render");
    println!("shape: {shape}");
    println!(
        "{:>5} {:>8} {:>9} {:>9} {:>8} {:>8}",
        "rows", "in KB", "out KB", "ms", "ms/KB", "ms/row"
    );
    for n in [10usize, 25, 50, 100, 200, 400] {
        let doc = page(n, &container, &shape);
        let t = Instant::now();
        let out = ikigai_xslt::transform_xml(&doc, STYLESHEET, false).expect("render");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let kb = doc.len() as f64 / 1024.0;
        println!(
            "{n:>5} {kb:>8.1} {:>9.1} {ms:>9.0} {:>8.1} {:>8.2}",
            out.len() as f64 / 1024.0,
            ms / kb,
            ms / n as f64
        );
    }
    if shape != "full" {
        return;
    }
    // The same 400 rows as documents of `k` rows each: which chunk size sits on the floor.
    for k in [1usize, 2, 5, 10, 20, 25, 50, 100] {
        let doc = page(k, &container, &shape);
        let t = Instant::now();
        let mut total = 0usize;
        for _ in 0..(400 / k) {
            let out = ikigai_xslt::transform_xml(&doc, STYLESHEET, false).expect("render");
            total += out.len();
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "400 rows as {:>3} x {k:>3}-row documents: {ms:>6.0} ms total, {:>7.1} KB out",
            400 / k,
            total as f64 / 1024.0
        );
    }
}
