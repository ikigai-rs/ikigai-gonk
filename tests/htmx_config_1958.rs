//! Ledger [#1958](http://localhost:1062/l/default/item/1958): htmx's configuration, pinned.
//!
//! Every full page carries one `<meta name="htmx-config">`, the only place htmx is configured
//! (UI conventions D7). htmx 2.0.4 runs a `<script>` inside swapped content unless
//! `allowScriptTags` is `false`, and CSP's `script-src 'self'` does not stop one whose `src` is a
//! same-origin path (ledger #1948's CSP analysis), so the page turns it off, beside `allowEval`
//! and the injected indicator `<style>` the CSP refuses anyway. A key added, removed or flipped
//! fails here with the whole config in the message.

use ikigai_gonk::render::{self, envelope};

/// Every `<meta>` element in `html` that names `htmx-config`, whichever quote the serializer
/// chose.
fn htmx_metas(html: &str) -> Vec<&str> {
    html.split("<meta")
        .skip(1)
        .map(|rest| &rest[..rest.find('>').expect("a closed meta")])
        .filter(|meta| {
            meta.contains(r#"name="htmx-config""#) || meta.contains("name='htmx-config'")
        })
        .collect()
}

/// A meta's `content`, unescaped.
fn content(meta: &str) -> String {
    let rest = &meta[meta.find("content=").expect("a content attribute") + "content=".len()..];
    let quote = rest.chars().next().expect("a quoted value");
    let value = &rest[1..];
    value[..value.find(quote).expect("a closing quote")]
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&amp;", "&")
}

#[test]
fn every_full_page_configures_htmx_once_and_strictly() {
    let page = envelope(
        "page",
        &[
            ("view", "nothing-else"),
            ("title", "Empty"),
            ("full", "true"),
        ],
        "",
    );
    let html = render::render(&page, true).expect("the page renders");
    let metas = htmx_metas(&html);
    assert_eq!(metas.len(), 1, "one htmx-config meta: {html}");
    let config: serde_json::Value =
        serde_json::from_str(&content(metas[0])).expect("the config is JSON");
    assert_eq!(
        config,
        serde_json::json!({
            "allowEval": false,
            "allowScriptTags": false,
            "includeIndicatorStyles": false,
        }),
        "{config}"
    );
}
