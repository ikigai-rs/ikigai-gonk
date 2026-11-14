//! The ledger items about one file, drawn on the browse page that shows the file (ledger
//! [#3667](http://localhost:1062/l/default/item/3667)).
//!
//! ```text
//! urn:iki:gonk:fragment:file-items:{view}   Source  the section, for a browse file view
//! ```
//!
//! A finding names its file by the canonical IRI, `urn:repo:file:{root}/{path}` (ledger #3657);
//! the browse page shows it at browse's own `urn:repo:{root}:file:{path}`. The page maps one to
//! the other through [`crate::file_iris::canonical`], and nowhere else: no string edit here
//! builds either form.
//!
//! # What is listed, and in what order
//!
//! The OPEN items `about` the canonical IRI, in every ledger the caller may read
//! ([`crate::web::readable_ledgers`]), each read through the ledger's own `items about=` under
//! the CALLER's capability. A ledger the caller may not read is never asked, and one whose read
//! fails is left out, so an item the caller may not read is not listed and nothing on the page
//! says one exists: the empty sentence names "the ledgers you can read", which is true whether
//! there are none or none of them are readable.
//!
//! Findings come first, by severity: critical, major, minor, trivial, then any other severity
//! word, then a finding with none; then every other item. Ties keep the ledger order and then
//! the item number. A finding is an item of class `ledger:Finding`, or one labeled `finding`
//! (what `ikigai-gonk findings file` files) or `severity:{word}`. Its severity is its
//! `severity:{word}` label, or, on an item labeled `finding`, the bare severity word that
//! command writes.
//!
//! # Where it is drawn
//!
//! The browse shell ([`crate::k::BrowseShell`]) draws the section for the file it opens on;
//! `web/gonk.js` swaps in this resource's fragment whenever the region moves to another file
//! and hides it on any other view, the way "On GitHub" follows the region.

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, Error, Invocation, Representation, Result, Verb,
};
use ikigai_ledger::json::{Item, ItemsDocument};
use std::sync::Arc;

use crate::render::{self, element, envelope};
use crate::web::{self, Web};

/// The fragment's grammar: a trailing capture, so the view keeps its colons and slashes.
pub const TEMPLATE: &str = "urn:iki:gonk:fragment:file-items:{view}";

/// The class a review finding is filed at.
const FINDING_CLASS: &str = "https://ikigai-rs.dev/ns/ledger#Finding";

/// The severity words in the order a reader is asked to look at them.
const SEVERITIES: [&str; 4] = ["critical", "major", "minor", "trivial"];

/// At most this many items per ledger: the ledger's own `limit=0`.
const LIMIT: &str = "0";

/// One listed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub ledger: String,
    pub number: i64,
    pub display: String,
    pub title: String,
    pub href: String,
    /// `Some` for a finding: its severity word, or `None` inside when it carries none.
    pub finding: Option<Option<String>>,
}

impl Row {
    fn of(ledger: &ikigai_ledger::Ledger, item: Item) -> Self {
        let finding = is_finding(&item).then(|| severity(&item));
        Row {
            ledger: ledger.name().to_string(),
            number: item.number,
            // The item's page by its IRI's id, as the ledger page links it.
            href: format!(
                "{}/item/{}",
                web::ledger_href(ledger),
                ikigai_fn::html::url_segment(
                    item.iri
                        .rsplit_once(":item:")
                        .map_or(item.number.to_string().as_str(), |(_, id)| id)
                )
            ),
            display: item.display,
            title: item.title,
            finding,
        }
    }

    /// The sort key: findings by severity, then the other items.
    fn rank(&self) -> usize {
        match &self.finding {
            Some(Some(word)) => SEVERITIES
                .iter()
                .position(|s| s == word)
                .unwrap_or(SEVERITIES.len()),
            Some(None) => SEVERITIES.len() + 1,
            None => SEVERITIES.len() + 2,
        }
    }

    /// What the row says about its kind, in words: `critical finding`, `finding, no
    /// severity`, or nothing for an item that is not a finding.
    fn kind_words(&self) -> String {
        match &self.finding {
            Some(Some(word)) => format!("{word} finding"),
            Some(None) => "finding, no severity".to_string(),
            None => String::new(),
        }
    }
}

fn is_finding(item: &Item) -> bool {
    item.kind.as_deref() == Some(FINDING_CLASS)
        || item
            .labels
            .iter()
            .any(|l| l == crate::findings::LABEL || l.starts_with("severity:"))
}

fn severity(item: &Item) -> Option<String> {
    let labeled = item
        .labels
        .iter()
        .find_map(|l| l.strip_prefix("severity:"))
        .filter(|w| !w.is_empty());
    let bare = || {
        item.labels
            .iter()
            .any(|l| l == crate::findings::LABEL)
            .then(|| {
                item.labels
                    .iter()
                    .find(|l| SEVERITIES.contains(&l.as_str()))
                    .map(String::as_str)
            })
            .flatten()
    };
    labeled.or_else(bare).map(str::to_string)
}

/// Findings first by severity, then the other items; ties in the order they came.
pub fn order(rows: &mut [Row]) {
    rows.sort_by_key(Row::rank);
}

/// The open items about `canonical` in every ledger the caller may read, in [`order`].
pub async fn rows(web: &Web, inv: &Invocation<'_>, canonical: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for ledger in web::readable_ledgers(web, inv.capability) {
        let Ok(read) = web::request(Verb::Source, &format!("{}items", ledger.prefix())) else {
            continue;
        };
        let read = [
            ("as", "application/json"),
            ("status", "open"),
            ("about", canonical),
            ("limit", LIMIT),
        ]
        .into_iter()
        .fold(read, |read, (name, value)| web::with(read, name, value));
        // A refusal or a defect lists nothing from this ledger, and says nothing about it.
        let Ok(bytes) = web::fetch(inv, read).await else {
            continue;
        };
        let Ok(document) = serde_json::from_slice::<ItemsDocument>(&bytes) else {
            continue;
        };
        let mut items = document.items;
        items.sort_by_key(|item| item.number);
        rows.extend(items.into_iter().map(|item| Row::of(&ledger, item)));
    }
    order(&mut rows);
    rows
}

/// The `view:file-items` element the stylesheet draws, for `rows` about the browse view
/// `shown` (which `web/gonk.js` reads to skip a fetch for the file already drawn).
pub fn view(shown: &str, rows: &[Row]) -> String {
    let children: String = rows
        .iter()
        .map(|row| {
            element(
                "file-item",
                &[
                    ("href", &row.href),
                    ("display", &row.display),
                    ("kind", &row.kind_words()),
                ],
                &row.title,
            )
        })
        .collect();
    render::wrap("file-items", &[("view", shown)], &children)
}

/// The section for the browse view `view`, or `None` when `view` is not a browse file IRI.
pub async fn section(web: &Web, inv: &Invocation<'_>, view: &str) -> Option<String> {
    let canonical = crate::file_iris::canonical(view)?;
    Some(self::view(view, &rows(web, inv, &canonical).await))
}

/// `urn:iki:gonk:fragment:file-items:{view}` — the section alone, for `web/gonk.js`.
pub struct FileItems {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for FileItems {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(
                "a file's items answer Source only".to_string(),
            ));
        }
        web::html_only(inv)?;
        let view = web::binding(inv, "view")?;
        let Some(section) = section(&self.web, inv, &view).await else {
            return Err(Error::NotFound(format!(
                "`{view}` is not a browse file (urn:repo:{{root}}:file:{{path}})"
            )));
        };
        let doc = envelope(
            "page",
            &[("view", "file-items"), ("full", "false")],
            &section,
        );
        Ok(web::html(
            render::render(&doc, false).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-fragment-file-items"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-fragment-file-items")
            .title("The ledger items about one file")
            .summary(
                "The open items about a browse file view's canonical IRI \
                 (urn:repo:file:{root}/{path}), in every ledger the caller may read, findings \
                 first by severity: the section the browse page draws above a file.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("view")
                    .binding()
                    .summary("the browse file view, urn:repo:{root}:file:{path}")
                    .class("http://www.w3.org/2001/XMLSchema#anyURI"),
            )
            .input(web::as_html_arg())
            .output("text/html")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(number: i64, finding: Option<Option<&str>>) -> Row {
        Row {
            ledger: "default".to_string(),
            number,
            display: format!("#{number}"),
            title: format!("item {number}"),
            href: format!("/l/default/item/{number}"),
            finding: finding.map(|s| s.map(str::to_string)),
        }
    }

    #[test]
    fn findings_come_first_by_severity_then_the_rest_in_their_order() {
        let mut rows = vec![
            row(1, None),
            row(2, Some(Some("trivial"))),
            row(3, Some(None)),
            row(4, Some(Some("info"))),
            row(5, Some(Some("critical"))),
            row(6, None),
            row(7, Some(Some("minor"))),
            row(8, Some(Some("major"))),
            row(9, Some(Some("critical"))),
        ];
        order(&mut rows);
        let numbers: Vec<i64> = rows.iter().map(|r| r.number).collect();
        assert_eq!(numbers, [5, 9, 8, 7, 2, 4, 3, 1, 6]);
    }

    #[test]
    fn a_severity_reads_from_its_label_or_a_findings_bare_word() {
        let item = |kind: Option<&str>, labels: &[&str]| -> Item {
            serde_json::from_value(serde_json::json!({
                "number": 1, "display": "#1", "iri": "urn:x", "kind": kind, "title": "t",
                "body": "", "status": "open", "closed_reason": null, "priority": null,
                "deferred": false, "labels": labels, "about": [], "key": null,
                "author": null, "revision": null, "claim": null, "created": "", "modified": "",
                "links": [], "comments": [], "defects": [], "state": null, "awaiting": null
            }))
            .unwrap()
        };
        let finding = |item: &Item| is_finding(item).then(|| severity(item));
        assert_eq!(
            finding(&item(Some(FINDING_CLASS), &["roborev", "severity:major"])),
            Some(Some("major".to_string()))
        );
        assert_eq!(
            finding(&item(None, &["critical", "finding", "repo:demo"])),
            Some(Some("critical".to_string()))
        );
        assert_eq!(finding(&item(Some(FINDING_CLASS), &[])), Some(None));
        assert_eq!(finding(&item(None, &["critical"])), None, "not a finding");
    }
}
