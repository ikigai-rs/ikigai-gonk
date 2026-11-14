//! The ledger items about one file, for the browse page that opens on it (ledger #3667).
//!
//! A finding filed about a file names it by its canonical IRI, `urn:repo:file:{root}/{path}`
//! ([`crate::file_iris`]), and `items about=` finds it; the browse page names the same file
//! `urn:repo:{root}:file:{path}`. This module is the one place the page crosses between the
//! two, and it crosses through [`crate::file_iris::canonical`], never by editing the string.
//!
//! # What is listed, and in what order
//!
//! The OPEN items about the file in every ledger the caller may read, each read through that
//! ledger's own `items` resource under the CALLER's capability ([`Invocation::issue`]): a
//! ledger the grant does not read is not asked, and one that refuses anyway is skipped
//! without a word, so the page neither lists an item the caller may not read nor hints that
//! one exists. Findings come first, by the severity label `ledger:Finding` names
//! (`severity:critical`, `severity:major`, `severity:minor`, `severity:trivial`), then a
//! finding with no severity, then every other item; within each, by ledger and number.
//!
//! # ⚠ Only the view the page opens on
//!
//! The section belongs to the page the shell draws. When the browse region moves to another
//! view by htmx, `web/gonk.js` hides it rather than leave one file's items beside another.

use ikigai_core::{Invocation, Verb};
use ikigai_ledger::json::ItemsDocument;
use ikigai_ledger::Ledger;

use crate::render::{element, wrap};
use crate::web::{self, Web};

/// The `ledger:Finding` class (`kind=`).
const FINDING_CLASS: &str = crate::roborev::FINDING_CLASS;

/// The severity labels `ledger:Finding` names, most severe first, and the word each is said as.
pub const SEVERITIES: [(&str, &str); 4] = [
    ("severity:critical", "critical"),
    ("severity:major", "major"),
    ("severity:minor", "minor"),
    ("severity:trivial", "trivial"),
];

/// How many open items one ledger is asked for. A file with more than this is rare enough
/// that the page says how many it left out rather than paging.
const LIMIT: usize = 100;

/// One item the section lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Its ledger's name.
    pub ledger: String,
    /// Its number in that ledger.
    pub number: i64,
    /// `#12`, or `acme#12`.
    pub display: String,
    /// Its title.
    pub title: String,
    /// The word its severity is said as, for a finding that carries one.
    pub severity: Option<&'static str>,
    /// Whether it is a finding: of kind `ledger:Finding`, or carrying a severity label.
    pub finding: bool,
}

impl Listed {
    /// Where it sorts: findings by severity, then a finding without one, then the rest.
    fn rank(&self) -> usize {
        match (self.severity, self.finding) {
            (Some(word), _) => SEVERITIES
                .iter()
                .position(|(_, w)| *w == word)
                .unwrap_or(SEVERITIES.len()),
            (None, true) => SEVERITIES.len(),
            (None, false) => SEVERITIES.len() + 1,
        }
    }
}

/// The severity word an item's labels carry, the most severe when it carries several.
fn severity(labels: &[String]) -> Option<&'static str> {
    SEVERITIES
        .iter()
        .find(|(label, _)| labels.iter().any(|l| l == label))
        .map(|(_, word)| *word)
}

/// Put `items` in the section's order.
pub fn order(items: &mut [Listed]) {
    items.sort_by(|a, b| (a.rank(), &a.ledger, a.number).cmp(&(b.rank(), &b.ledger, b.number)));
}

/// The `view:file-items` element for the browse page opening on `start`, or `None` when
/// `start` is not one file's browse IRI.
pub async fn section(web: &Web, inv: &Invocation<'_>, start: &str) -> Option<String> {
    let file = crate::file_iris::canonical(start)?;
    let mut items = Vec::new();
    let mut more = 0usize;
    for ledger in web::readable_ledgers(web, inv.capability) {
        let Some(page) = read(inv, &ledger, &file).await else {
            continue;
        };
        more += page
            .total
            .unwrap_or(page.count)
            .saturating_sub(page.items.len());
        for item in page.items {
            let severity = severity(&item.labels);
            items.push(Listed {
                ledger: ledger.name().to_string(),
                number: item.number,
                display: item.display,
                title: item.title,
                severity,
                finding: severity.is_some() || item.kind.as_deref() == Some(FINDING_CLASS),
            });
        }
    }
    order(&mut items);
    let mut children = String::new();
    for item in &items {
        let ledger = Ledger::parse(&item.ledger).ok()?;
        let href = format!("{}/item/{}", web::ledger_href(&ledger), item.number);
        let mut attributes = vec![("href", href.as_str()), ("display", item.display.as_str())];
        if let Some(word) = item.severity {
            attributes.push(("severity", word));
        } else if item.finding {
            attributes.push(("severity", "unrated"));
        }
        children.push_str(&element("file-item", &attributes, &item.title));
    }
    let count = items.len().to_string();
    let more = more.to_string();
    Some(wrap(
        "file-items",
        &[
            ("file", file.as_str()),
            ("view", start),
            ("count", &count),
            ("more", &more),
        ],
        &children,
    ))
}

/// One ledger's open items about `file`, or `None` when it refuses or answers nothing legible.
async fn read(inv: &Invocation<'_>, ledger: &Ledger, file: &str) -> Option<ItemsDocument> {
    let request = web::request(Verb::Source, &format!("{}items", ledger.prefix())).ok()?;
    let request = [
        ("about", file),
        ("status", "open"),
        ("limit", &LIMIT.to_string()),
        ("as", ikigai_ledger::json::MEDIA_TYPE),
    ]
    .into_iter()
    .fold(request, |r, (name, value)| web::with(r, name, value));
    let bytes = web::fetch(inv, request).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(ledger: &str, number: i64, severity: Option<&'static str>, finding: bool) -> Listed {
        Listed {
            ledger: ledger.into(),
            number,
            display: format!("#{number}"),
            title: String::new(),
            severity,
            finding,
        }
    }

    #[test]
    fn findings_sort_by_severity_then_the_rest() {
        let mut items = vec![
            listed("default", 1, None, false),
            listed("default", 2, Some("trivial"), true),
            listed("default", 3, None, true),
            listed("default", 4, Some("critical"), true),
            listed("default", 5, Some("minor"), true),
            listed("default", 6, Some("major"), true),
            listed("default", 7, Some("critical"), true),
        ];
        order(&mut items);
        let numbers: Vec<i64> = items.iter().map(|i| i.number).collect();
        assert_eq!(numbers, [4, 7, 6, 5, 2, 3, 1]);
    }

    #[test]
    fn the_most_severe_label_wins() {
        let labels = ["severity:minor", "roborev", "severity:major"].map(String::from);
        assert_eq!(severity(&labels), Some("major"));
        assert_eq!(severity(&["minor".to_string()]), None);
    }
}
