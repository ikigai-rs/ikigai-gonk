//! The Dashboard's two operator tabs, Cache and System (ledger
//! [#2972](http://localhost:1062/l/default/item/2972)). Tabs and pages are [`crate::dashboard`]'s;
//! this is what each panel holds.
//!
//! ```text
//! Cache   urn:kernel:cache, :threads, :uncached   the HUB's, under the viewer's capability
//! System  urn:iki:gonk:system:{disk,store,gonk}   crate::system
//!         urn:iki:gonk:system:workers             here: it reads the ledgers the viewer may
//! ```
//!
//! ★ **Each part is its own fragment** ([`Part`]), polled as a chart is, so a part that fails says
//! so where it would be and the rest of the panel is still good, and a person's scroll or focus in
//! one is not disturbed by another's answer.
//!
//! ★ **A viewer whose grant does not include a tab never sees it**, the rule for a ledger they may
//! not read: `dashboard::offered` leaves it off the page, and its page, panel and parts
//! each declare the capability and answer `403` without it.
//!
//! ⚠ **The hub's cache, not this door's.** A door kernel caches nothing (`crate::doors`), and it
//! answers `urn:kernel:*` itself, so `urn:kernel:cache` asked through the invocation is an empty
//! readout of the wrong kernel. `hub_read` issues to the hub under the VIEWER's capability, which
//! is the one place a page reaches past its invocation, and it grants nothing: the hub checks
//! `urn:cap:kernel:inspect` against that same capability.
//!
//! ★ **Names, never argument values.** A cache entry's name can say what someone else read, so it
//! shows only under the operator grant, and an entry's arguments are not in the readout at all: a
//! SPARQL query's text is an argument. [`names_only`] holds that for a name written with a query
//! string, and a line this module cannot read is counted, never printed
//! (`tests/dashboard_operator_2972.rs`).
//!
//! ⚠ **The readouts are a human face, parsed here**, because core offers no other for
//! `urn:kernel:uncached`. The tests read them from a real kernel, so a change of grammar goes red.
//! core counts no hits and misses, and the tab says that rather than showing a zero.
//!
//! ★ **Ledger text is data.** A worker's line and an item's title go through `html_text` like every
//! other title on these pages, and a line is drawn as text, with no link made from it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{Description, Endpoint, Error, Expiry, Invocation, Representation, Result, Verb};
use ikigai_fn::html::{attr, html_text, url_segment};
use ikigai_ledger::Ledger;
use serde_json::{json, Value};

use crate::backup::{humanize, stamp_iso};
use crate::dashboard::{live, Tab};
use crate::system;
use crate::web::{
    as_html_arg, fetch, html, html_only, ledger_href, menu_order, readable_ledgers, request, with,
    Web, HTML,
};

/// What the Cache tab requires: core's own token for its readouts.
pub const CAP_INSPECT: &str = "urn:cap:kernel:inspect";

/// Each worker status item's latest lines, and what is claimed now.
pub const WORKERS: &str = "urn:iki:gonk:system:workers";

/// The most cache rows a part draws. The cache holds up to 4096, and a poll redraws them.
pub const MAX_ROWS: usize = 200;

const RESULTS_JSON: &str = "application/sparql-results+json";
const JSON: &str = "application/json";

/// The most of a worker's line a row shows: the first line of a comment anyone could write.
const MAX_LINE_CHARS: usize = 300;

/// A part of an operator tab, each its own fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Entries,
    Threads,
    Uncached,
    Disk,
    Store,
    Gonk,
    Workers,
    Claimed,
}

impl Part {
    /// Every part, in the order the panels list them.
    pub const ALL: [Part; 8] = [
        Part::Entries,
        Part::Threads,
        Part::Uncached,
        Part::Disk,
        Part::Store,
        Part::Gonk,
        Part::Workers,
        Part::Claimed,
    ];

    /// The tab it is on.
    pub fn tab(self) -> Tab {
        match self {
            Part::Entries | Part::Threads | Part::Uncached => Tab::Cache,
            _ => Tab::System,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Part::Entries => "entries",
            Part::Threads => "threads",
            Part::Uncached => "uncached",
            Part::Disk => "disk",
            Part::Store => "store",
            Part::Gonk => "gonk",
            Part::Workers => "workers",
            Part::Claimed => "claimed",
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Part::Entries => "Cache entries",
            Part::Threads => "Golden threads",
            Part::Uncached => "Reads not cached",
            Part::Disk => "Disk",
            Part::Store => "Store",
            Part::Gonk => "gonk",
            Part::Workers => "Workers",
            Part::Claimed => "Items claimed now",
        }
    }

    /// Its fragment's IRI.
    pub fn iri(self) -> String {
        format!("urn:iki:gonk:fragment:dashboard-part:{}", self.name())
    }

    /// Its fragment's path.
    pub fn path(self) -> String {
        format!("/dashboard/part/{}", self.name())
    }
}

/// A sentence to show in a part in place of a resource that did not answer. Written by class, as
/// `dashboard::unread` writes its own: what an error carries is written for a log.
fn unread(what: &str, error: &Error) -> String {
    let why = match error {
        Error::Unresolved(_) | Error::NotFound(_) => {
            "this server does not bind it. A server started without that family has no such row"
        }
        Error::Timeout(_) => "it took longer than your time budget",
        Error::Unavailable(_) | Error::RetryAfter { .. } => {
            "the store is unavailable right now. Try again shortly"
        }
        Error::Denied(_) => "your grant no longer reaches it",
        _ => "this server failed while reading it",
    };
    format!(
        "<p class=\"note\">{} could not be read: {why}.</p>",
        html_text(what)
    )
}

/// `text`, with anything a name's query string or fragment would hold cut from each word: a
/// readout's words are names and counts, and what follows a `?` in one is an argument's value.
pub fn names_only(text: &str) -> String {
    text.split_whitespace()
        .map(|word| word.split(['?', '#']).next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first line of `text`, at most [`MAX_LINE_CHARS`] characters.
fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let mut out: String = line.chars().take(MAX_LINE_CHARS).collect();
    if line.chars().count() > MAX_LINE_CHARS {
        out.push('…');
    }
    out
}

/// `1234567890` as `1.1 GiB`.
pub fn in_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `<dl>` of `(term, value)`, both escaped.
fn totals(rows: &[(&str, String)]) -> String {
    let rows: String = rows
        .iter()
        .map(|(term, value)| {
            format!(
                "<div><dt>{}</dt><dd>{}</dd></div>",
                html_text(term),
                html_text(value)
            )
        })
        .collect();
    format!("<dl class=\"readout-totals\">{rows}</dl>")
}

/// A table in the frame that scrolls at phone width. Most hold no link, so the frame is itself a
/// stop on the keyboard's way (WCAG 2.1.1). `rows` is already written; `columns` are plain text.
fn table(label: &str, columns: &[&str], rows: &str) -> String {
    let head: String = columns
        .iter()
        .map(|column| format!("<th scope=\"col\">{}</th>", html_text(column)))
        .collect();
    format!(
        "<div class=\"table-wrap\" role=\"region\" tabindex=\"0\" aria-label=\"{label}\">\
         <table class=\"readout-table\"><caption class=\"sr-only\">{caption}</caption>\
         <thead><tr>{head}</tr></thead><tbody>{rows}</tbody></table></div>",
        label = attr(label),
        caption = html_text(label),
    )
}

/// One row: the first cell a row header, every cell escaped.
fn row(cells: &[&str]) -> String {
    let mut out = String::from("<tr>");
    for (n, cell) in cells.iter().enumerate() {
        if n == 0 {
            out.push_str(&format!("<th scope=\"row\">{}</th>", html_text(cell)));
        } else {
            out.push_str(&format!("<td>{}</td>", html_text(cell)));
        }
    }
    out.push_str("</tr>");
    out
}

/// A row's verdict, in words whatever the color: what is wrong and what to do, or that it is fine.
/// `.needs-attention` is what `web/gonk.js` counts to say a change aloud.
fn verdict(attention: bool, fine: &str, wrong: &str) -> String {
    if attention {
        format!(
            "<p class=\"readout-state needs-attention\"><strong>Needs attention:</strong> {}</p>",
            html_text(wrong)
        )
    } else {
        format!(
            "<p class=\"readout-state\"><strong>OK:</strong> {}</p>",
            html_text(fine)
        )
    }
}

// ------------------------------------------------------------------------------- the readouts

/// The totals `urn:kernel:cache` opens with, and its rows.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CacheReadout {
    pub entries: u64,
    pub max_entries: u64,
    pub stale: u64,
    pub size: String,
    pub max_size: String,
    pub evictions: u64,
    pub rows: Vec<CacheEntry>,
    /// Lines that were not understood: counted, and never printed.
    pub unread: usize,
}

/// One entry, as `urn:kernel:cache` prints it.
#[derive(Debug, PartialEq, Eq)]
pub struct CacheEntry {
    pub name: String,
    pub state: String,
    pub media_type: String,
    pub size: String,
    pub threads: String,
    pub chain: String,
}

/// Parse the text face of `urn:kernel:cache`. Columns are two spaces apart or more, a name holds
/// no space, and the section after the entries (`failures …`) starts with no indent.
pub fn parse_cache(text: &str) -> CacheReadout {
    let mut out = CacheReadout::default();
    for line in text.lines().skip(1) {
        let Some(body) = line.strip_prefix("  ") else {
            break;
        };
        if let Some(rest) = body.strip_prefix("entries  ") {
            let mut words = rest.split_whitespace();
            out.entries = words.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            out.max_entries = words.nth(1).and_then(|n| n.parse().ok()).unwrap_or(0);
            out.stale = words
                .next()
                .and_then(|n| n.trim_start_matches('(').parse().ok())
                .unwrap_or(0);
        } else if let Some(rest) = body.strip_prefix("size  ") {
            if let Some((size, max)) = rest.split_once(" / ") {
                out.size = size.trim().to_string();
                out.max_size = max.trim().to_string();
            }
        } else if body.starts_with("policy  ") {
            continue;
        } else if let Some(rest) = body.strip_prefix("evictions  ") {
            out.evictions = rest.trim().parse().unwrap_or(0);
        } else {
            let cells: Vec<&str> = body
                .split("  ")
                .map(str::trim)
                .filter(|cell| !cell.is_empty())
                .collect();
            match cells[..] {
                [name, state, media_type, size, threads, ref chain @ ..]
                    if matches!(state, "live" | "cut" | "expired") =>
                {
                    out.rows.push(CacheEntry {
                        name: names_only(name),
                        state: state.to_string(),
                        media_type: media_type.to_string(),
                        size: size.to_string(),
                        threads: threads.to_string(),
                        chain: names_only(&chain.join(" ")),
                    })
                }
                _ => out.unread += 1,
            }
        }
    }
    out
}

/// One row of `urn:kernel:uncached`: its name, how many times, why, and its chain.
#[derive(Debug, PartialEq, Eq)]
pub struct UncachedRow {
    pub name: String,
    pub count: String,
    pub reason: String,
    pub chain: String,
}

/// Parse the text face of `urn:kernel:uncached`, as `  {name}  ×{n}  {reason}  [{chain}]`. The
/// count in `.1` is how many lines were not understood.
pub fn parse_uncached(text: &str) -> (Vec<UncachedRow>, usize) {
    let mut rows = Vec::new();
    let mut unread = 0;
    for line in text.lines().skip(1) {
        let Some(body) = line.strip_prefix("  ") else {
            continue;
        };
        if body.starts_with('(') {
            continue; // "(nothing computed uncached)"
        }
        let parsed = (|| {
            let (head, chain) = body.rsplit_once('[')?;
            let chain = chain.strip_suffix(']')?;
            let (name, rest) = head.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (count, reason) = rest.split_once(char::is_whitespace)?;
            count.starts_with('×').then(|| UncachedRow {
                name: names_only(name),
                count: count.trim_start_matches('×').to_string(),
                reason: names_only(reason),
                chain: names_only(chain),
            })
        })();
        match parsed {
            Some(row) => rows.push(row),
            None => unread += 1,
        }
    }
    (rows, unread)
}

/// Parse the text face of `urn:kernel:threads`, as `  {thread}  gen {n}`.
pub fn parse_threads(text: &str) -> (Vec<(String, String)>, usize) {
    let mut rows = Vec::new();
    let mut unread = 0;
    for line in text.lines().skip(1) {
        let Some(body) = line.strip_prefix("  ") else {
            continue;
        };
        if body.starts_with('(') {
            continue; // "(none cut)"
        }
        match body.rsplit_once("  gen ") {
            Some((thread, generation)) => {
                rows.push((names_only(thread), generation.trim().to_string()))
            }
            None => unread += 1,
        }
    }
    (rows, unread)
}

/// How many readout lines were left out, if any.
fn unread_lines(unread: usize) -> String {
    match unread {
        0 => String::new(),
        n => format!(
            "<p class=\"note\">{n} line(s) of this readout could not be read, so they are not \
             shown. This gonk and the ikigai-core it links may disagree on the format.</p>"
        ),
    }
}

/// A readout of the HUB, under the viewer's capability. See the module's note.
async fn hub_read(web: &Web, inv: &Invocation<'_>, target: &str) -> Result<String> {
    let read = request(Verb::Source, target)?;
    let answer = web.hub.issue(read, inv.capability).await?;
    Ok(String::from_utf8_lossy(&answer.bytes).into_owned())
}

fn entries_html(readout: &CacheReadout) -> String {
    let live = readout.entries.saturating_sub(readout.stale);
    let mut out = totals(&[
        (
            "Entries",
            format!("{} of at most {}", readout.entries, readout.max_entries),
        ),
        ("Live", live.to_string()),
        ("Stale", readout.stale.to_string()),
        (
            "Size",
            format!("{} of at most {}", readout.size, readout.max_size),
        ),
        ("Evictions", readout.evictions.to_string()),
        ("Hits and misses", "not counted by this kernel".to_string()),
    ]);
    out.push_str(
        "<p class=\"note\">A stale entry stays until a read replaces it, so some are normal \
         after a write. Evictions climbing with entries at the limit mean the cache is too small \
         for what is being read.</p>",
    );
    if !readout.rows.is_empty() {
        let rows: String = readout
            .rows
            .iter()
            .take(MAX_ROWS)
            .map(|e| {
                row(&[
                    &e.name,
                    &e.state,
                    &e.media_type,
                    &e.size,
                    &e.threads,
                    &e.chain,
                ])
            })
            .collect();
        out.push_str(&table(
            "Cache entries, by name",
            &["Name", "State", "Type", "Size", "Depends on", "Chain"],
            &rows,
        ));
        if readout.rows.len() > MAX_ROWS {
            out.push_str(&format!(
                "<p class=\"note\">The first {MAX_ROWS} of {} entries, by name.</p>",
                readout.rows.len()
            ));
        }
    }
    out.push_str(&unread_lines(readout.unread));
    out
}

fn threads_html(rows: &[(String, String)], unread: usize) -> String {
    let mut out = if rows.is_empty() {
        "<p class=\"note\">No golden thread has been cut since this server started.</p>".to_string()
    } else {
        let body: String = rows
            .iter()
            .take(MAX_ROWS)
            .map(|(thread, generation)| row(&[thread, generation]))
            .collect();
        let mut out = format!(
            "<p class=\"note\">{} threads tracked. A thread that is cut often makes what hangs \
             from it recompute as often.</p>",
            rows.len()
        );
        out.push_str(&table(
            "Cut golden threads and their generations",
            &["Thread", "Generation"],
            &body,
        ));
        out
    };
    out.push_str(&unread_lines(unread));
    out
}

fn uncached_html(rows: &[UncachedRow], unread: usize) -> String {
    let mut out = if rows.is_empty() {
        "<p class=\"note\">Every read computed lately was cached.</p>".to_string()
    } else {
        let body: String = rows
            .iter()
            .map(|r| row(&[&r.name, &r.count, &r.reason, &r.chain]))
            .collect();
        let mut out = String::from(
            "<p class=\"note\">Most recent first. A slow page's read that shows here with a \
             high count recomputes every time: its reason says why, and that is what to \
             change.</p>",
        );
        out.push_str(&table(
            "Reads computed and not cached",
            &["Name", "Times", "Why", "Chain"],
            &body,
        ));
        out
    };
    out.push_str(&unread_lines(unread));
    out
}

// ------------------------------------------------------------------------------------- System

fn disk_html(disk: &Value, workers: Option<&Value>) -> String {
    let volumes = disk["volumes"].as_array().cloned().unwrap_or_default();
    let mut rows = String::new();
    let mut low = Vec::new();
    let mut floor = 0;
    for volume in &volumes {
        let name = volume["name"].as_str().unwrap_or_default();
        floor = volume["floor_bytes"].as_u64().unwrap_or(floor);
        let path = volume["path"].as_str().unwrap_or_default();
        let attention = volume["attention"].as_bool().unwrap_or(true);
        if attention {
            low.push(name.to_string());
        }
        let (free, total) = match volume["free_bytes"].as_u64() {
            Some(free) => (
                in_bytes(free),
                in_bytes(volume["total_bytes"].as_u64().unwrap_or(0)),
            ),
            None => ("could not be read".to_string(), "unknown".to_string()),
        };
        let state = if attention { "Under the floor" } else { "OK" };
        rows.push_str(&row(&[name, &free, &total, state, path]));
    }
    let floor = in_bytes(floor);
    let mut out = verdict(
        !low.is_empty(),
        &format!("every volume has more than the {floor} floor free."),
        &format!(
            "{} under the {floor} floor, or could not be read. Free space on that volume: old \
             worktrees and logs first, then fewer kept backups (gonk.backup.keep). The store \
             stops taking writes when its volume fills.",
            low.join(" and ")
        ),
    );
    out.push_str(&table(
        "This server's volumes",
        &["Volume", "Free", "Total", "State", "Path"],
        &rows,
    ));
    let lines: Vec<String> = workers
        .and_then(|w| w["workers"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|worker| {
            let line = worker["disk"].as_str()?;
            Some(format!(
                "<li><strong>{}</strong>: {}</li>",
                html_text(worker["name"].as_str().unwrap_or_default()),
                html_text(line)
            ))
        })
        .collect();
    if lines.is_empty() {
        out.push_str(
            "<p class=\"note\">No worker in a ledger you may read has posted a Disk: line. A \
             worker posts one when its own volume is under its floor.</p>",
        );
    } else {
        out.push_str(&format!(
            "<p class=\"note\">The latest Disk: line of each worker, as it wrote it. A worker \
             under its floor takes no new item until space returns on its machine.</p>\
             <ul class=\"readout-lines\">{}</ul>",
            lines.concat()
        ));
    }
    out
}

fn store_html(store: &Value, now_ms: u64) -> String {
    let backup = &store["backup"];
    let attention = backup["attention"].as_bool().unwrap_or(true);
    let every = backup["every_seconds"].as_u64().map(Duration::from_secs);
    let schedule = every.map_or("nothing scheduled".to_string(), |e| {
        format!("every {}", humanize(e))
    });
    let taken = backup["taken_at_ms"].as_u64();
    let fine = match taken {
        Some(_) => format!("the last backup is within its schedule ({schedule})."),
        None => "a backup exists.".to_string(),
    };
    let failures = backup["failures_in_a_row"].as_u64().unwrap_or(0);
    let wrong = if taken.is_none() {
        "there is no backup to restore. Take one over the owner's socket with Source \
         urn:iki:gonk:backup, and check gonk.backup.every."
            .to_string()
    } else if failures > 0 {
        format!(
            "the last {failures} scheduled backup(s) failed. Read urn:iki:gonk:backup:status over \
             the owner's socket for the reason, then take one with Source urn:iki:gonk:backup."
        )
    } else {
        format!(
            "the last backup is older than its schedule ({schedule}). Read \
             urn:iki:gonk:backup:status over the owner's socket for the timer's health, then \
             take one with Source urn:iki:gonk:backup."
        )
    };
    let mut out = verdict(attention, &fine, &wrong);
    let quads = store["quads"]
        .as_u64()
        .map_or("could not be read".to_string(), |q| q.to_string());
    let backup_at = taken.map_or("none".to_string(), |at| {
        format!(
            "{} UTC, {} ago",
            stamp_iso(at),
            humanize(Duration::from_millis(now_ms.saturating_sub(at)))
        )
    });
    out.push_str(&totals(&[
        ("Quads", quads),
        (
            "Backing",
            store["backing"].as_str().unwrap_or("unknown").to_string(),
        ),
        (
            "Size on disk",
            in_bytes(store["size_bytes"].as_u64().unwrap_or(0)),
        ),
        ("Last backup", backup_at),
        ("Schedule", schedule),
    ]));
    match store["graphs"].as_object() {
        Some(graphs) if !graphs.is_empty() => {
            let rows: String = graphs
                .iter()
                .map(|(graph, quads)| row(&[graph, &quads.to_string()]))
                .collect();
            out.push_str(
                "<p class=\"note\">Quads per graph as of the last backup: counting them live \
                 would scan the store on every poll.</p>",
            );
            out.push_str(&table("Quads per graph", &["Graph", "Quads"], &rows));
        }
        _ => out.push_str(
            "<p class=\"note\">Quads per graph come from the last backup's record, and there is \
             none to read.</p>",
        ),
    }
    out
}

fn gonk_html(gonk: &Value) -> String {
    let seconds = gonk["built_at_seconds"].as_u64().unwrap_or(0);
    let built = if seconds == 0 {
        "unknown".to_string()
    } else {
        format!("{} UTC", stamp_iso(seconds * 1000))
    };
    let text = |key: &str| gonk[key].as_str().unwrap_or("unknown").to_string();
    let doors: Vec<String> = gonk["doors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|door| {
            format!(
                "{} at {}",
                door["name"].as_str().unwrap_or_default(),
                door["at"].as_str().unwrap_or_default()
            )
        })
        .collect();
    let mut out = totals(&[
        ("Version", text("version")),
        ("Commit", text("commit")),
        ("Built", built),
        (
            "Up for",
            humanize(Duration::from_secs(
                gonk["uptime_seconds"].as_u64().unwrap_or(0),
            )),
        ),
        (
            "Doors listening",
            if doors.is_empty() {
                "none recorded".to_string()
            } else {
                doors.join("; ")
            },
        ),
        ("ikigai-ledger", text("ledger_version")),
        ("ikigai-browse", text("browse_version")),
    ]);
    out.push_str(
        "<p class=\"note\">If a door you expect is missing, read this server's startup banner, \
         where a door that failed says why, and check gonk.http and --no-quic. If the commit is \
         not the one you deployed, the running process is older than the binary: restart it.</p>",
    );
    out
}

fn workers_html(workers: &Value) -> String {
    let list = workers["workers"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return format!(
            "<p class=\"note\">No open item in a ledger you may read is labeled \
             worker-status:&lt;name&gt;. A worker finds its status item by that label, so label \
             one for each worker that should report here.</p>{}",
            unread_ledgers(workers)
        );
    }
    let mut rows = String::new();
    let mut silent = Vec::new();
    for worker in &list {
        let name = worker["name"].as_str().unwrap_or_default();
        let line = worker["line"].as_str();
        if line.is_none() {
            silent.push(name.to_string());
        }
        let at = worker["line_at"].as_str().unwrap_or("never");
        rows.push_str(&format!(
            "<tr><th scope=\"row\">{}</th><td><a href=\"{}\">#{}</a></td><td>{}</td><td>{}</td>\
             <td>{}</td></tr>",
            html_text(name),
            attr(worker["href"].as_str().unwrap_or_default()),
            html_text(worker["item"].as_str().unwrap_or_default()),
            html_text(line.unwrap_or("no Worker: line")),
            html_text(at),
            html_text(worker["disk"].as_str().unwrap_or("none")),
        ));
    }
    let mut out = verdict(
        !silent.is_empty(),
        "every worker has said who it is. A worker's line is its own words, as of the time beside \
         it.",
        &format!(
            "{} has posted no Worker: line. A worker posts one when its loop starts, so start the \
             loop on that machine, or close the status item if the worker is gone.",
            silent.join(", ")
        ),
    );
    out.push_str(&table(
        "Each worker's status item and its latest lines",
        &[
            "Worker",
            "Item",
            "Latest Worker: line",
            "Posted",
            "Latest Disk: line",
        ],
        &rows,
    ));
    out.push_str(&unread_ledgers(workers));
    out
}

/// The ledgers a row could not read, if any: a row that left one out says so.
fn unread_ledgers(workers: &Value) -> String {
    let names: Vec<&str> = workers["unread_ledgers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if names.is_empty() {
        return String::new();
    }
    format!(
        "<p class=\"note\">Could not read {}, so this leaves that out. Try again shortly.</p>",
        html_text(&names.join(", "))
    )
}

fn claimed_html(workers: &Value) -> String {
    let list = workers["claimed"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return format!(
            "<p class=\"note\">Nothing is claimed in a ledger you may read.</p>{}",
            unread_ledgers(workers)
        );
    }
    let rows: String = list
        .iter()
        .map(|claim| {
            format!(
                "<tr><th scope=\"row\"><a href=\"{}\">#{}</a></th><td>{}</td><td>{}</td><td>{}</td></tr>",
                attr(claim["href"].as_str().unwrap_or_default()),
                html_text(claim["item"].as_str().unwrap_or_default()),
                html_text(claim["ledger"].as_str().unwrap_or_default()),
                html_text(claim["title"].as_str().unwrap_or_default()),
                html_text(claim["holder"].as_str().unwrap_or_default()),
            )
        })
        .collect();
    let mut out = format!(
        "<p class=\"note\">{} claimed. An item held long past its lease is what to release.</p>",
        list.len()
    );
    out.push_str(&table(
        "Items claimed now",
        &["Item", "Ledger", "Title", "Holder"],
        &rows,
    ));
    out.push_str(&unread_ledgers(workers));
    out
}

async fn json_of(inv: &Invocation<'_>, target: &str) -> Result<Value> {
    let bytes = fetch(inv, request(Verb::Source, target)?).await?;
    serde_json::from_slice(&bytes).map_err(|e| Error::Endpoint(format!("{target}: {e}")))
}

/// A part's body, under the viewer's capability. Its own failure is said here, in the part.
async fn body_of(web: &Web, inv: &Invocation<'_>, part: Part) -> String {
    let answer = match part {
        Part::Entries => hub_read(web, inv, "urn:kernel:cache")
            .await
            .map(|text| entries_html(&parse_cache(&text))),
        Part::Threads => hub_read(web, inv, "urn:kernel:threads").await.map(|text| {
            let (rows, unread) = parse_threads(&text);
            threads_html(&rows, unread)
        }),
        Part::Uncached => hub_read(web, inv, "urn:kernel:uncached").await.map(|text| {
            let (rows, unread) = parse_uncached(&text);
            uncached_html(&rows, unread)
        }),
        Part::Disk => match json_of(inv, system::DISK).await {
            Ok(disk) => Ok(disk_html(&disk, json_of(inv, WORKERS).await.ok().as_ref())),
            Err(error) => Err(error),
        },
        Part::Store => match crate::agents::now_ms(inv) {
            Ok(now) => json_of(inv, system::STORE)
                .await
                .map(|store| store_html(&store, now)),
            Err(error) => Err(error),
        },
        Part::Gonk => json_of(inv, system::GONK).await.map(|g| gonk_html(&g)),
        Part::Workers => json_of(inv, WORKERS).await.map(|w| workers_html(&w)),
        Part::Claimed => json_of(inv, WORKERS).await.map(|w| claimed_html(&w)),
    };
    answer.unwrap_or_else(|error| unread(part.heading(), &error))
}

/// A part with its heading, as its fragment. `data-live="readout"` because these change on every
/// poll (the uptime, the cache): `web/gonk.js` says only a change in what needs attention.
async fn part_fragment(web: &Web, inv: &Invocation<'_>, part: Part) -> String {
    let inner = format!(
        "<section class=\"readout\"><h3>{}</h3>{}</section>",
        html_text(part.heading()),
        body_of(web, inv, part).await
    );
    live("readout", &part.path(), &inner)
}

/// What an operator tab's panel holds.
pub(crate) async fn panel(web: &Web, inv: &Invocation<'_>, tab: Tab) -> String {
    let mut out = format!(
        "<h2 class=\"sr-only\" id=\"dashboard-{}\">{}</h2>",
        tab.name(),
        tab.label()
    );
    for part in Part::ALL.into_iter().filter(|part| part.tab() == tab) {
        out.push_str(&part_fragment(web, inv, part).await);
    }
    out
}

/// `urn:iki:gonk:fragment:dashboard-part:{part}`: what an open tab asks for one part.
pub struct PartFragment {
    pub web: Arc<Web>,
    pub part: Part,
}

#[async_trait]
impl Endpoint for PartFragment {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        self.part.tab().require(inv.capability)?;
        Ok(html(part_fragment(&self.web, inv, self.part).await).with_expiry(Expiry::Always))
    }

    fn name(&self) -> &str {
        match self.part.tab() {
            Tab::Cache => "gonk-fragment-dashboard-cache",
            _ => "gonk-fragment-dashboard-system",
        }
    }

    fn describe(&self) -> Description {
        let description = Description::new(self.name())
            .title("One part of an operator tab on the Dashboard, as a fragment")
            .summary(
                "A section of the Dashboard's Cache or System tab, with no page around it, under \
                 the caller's capability. Names are shown; argument values never are.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(as_html_arg())
            .output(HTML);
        self.part.tab().declared(description)
    }
}

// ----------------------------------------------------------------------------------- workers

const STATUS_QUERY: &str = "\
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>
SELECT ?number ?label ?body ?created WHERE {
  ?item a ledger:Item ; ledger:number ?number ; ledger:status ledger:open ; ledger:label ?label .
  FILTER(STRSTARTS(STR(?label), \"worker-status:\"))
  OPTIONAL {
    ?comment ledger:onItem ?item ; ledger:body ?body ; dcterms:created ?created .
    FILTER(STRSTARTS(STR(?body), \"Worker:\") || STRSTARTS(STR(?body), \"Disk:\"))
  }
}
ORDER BY DESC(?created)
LIMIT 500";

const CLAIMED_QUERY: &str = "\
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>
SELECT ?number ?title ?holder WHERE {
  ?item a ledger:Item ; ledger:number ?number ; ledger:claimedBy ?holder ; dcterms:title ?title .
}
ORDER BY ?number
LIMIT 500";

async fn select(inv: &Invocation<'_>, ledger: &Ledger, query: &str) -> Result<Vec<Value>> {
    let read = with(request(Verb::Source, "urn:sparql:select")?, "query", query);
    let read = with(with(read, "as", RESULTS_JSON), "graph", &ledger.graph());
    let results: Value = serde_json::from_slice(&fetch(inv, read).await?)
        .map_err(|e| Error::Endpoint(format!("urn:sparql:select: {e}")))?;
    Ok(results["results"]["bindings"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn cell<'a>(row: &'a Value, name: &str) -> Option<&'a str> {
    row[name]["value"].as_str()
}

/// A worker's newest line of each kind.
#[derive(Default)]
struct Latest {
    line: Option<String>,
    line_at: Option<String>,
    disk: Option<String>,
}

/// One worker from `rows`, newest first: its latest line of each kind.
fn workers_of(ledger: &Ledger, rows: &[Value]) -> Vec<Value> {
    let mut found: BTreeMap<(String, String), Latest> = BTreeMap::new();
    for row in rows {
        let (Some(number), Some(label)) = (cell(row, "number"), cell(row, "label")) else {
            continue;
        };
        let Some(name) = label.strip_prefix("worker-status:") else {
            continue;
        };
        let latest = found
            .entry((name.to_string(), number.to_string()))
            .or_default();
        let (Some(body), Some(created)) = (cell(row, "body"), cell(row, "created")) else {
            continue;
        };
        if body.starts_with("Worker:") && latest.line.is_none() {
            latest.line = Some(one_line(body));
            latest.line_at = Some(created.to_string());
        } else if body.starts_with("Disk:") && latest.disk.is_none() {
            latest.disk = Some(one_line(body));
        }
    }
    found
        .into_iter()
        .map(|((name, number), latest)| {
            json!({
                "name": name,
                "ledger": ledger.name(),
                "item": number,
                "href": item_href(ledger, &number),
                "line": latest.line,
                "line_at": latest.line_at,
                "disk": latest.disk,
            })
        })
        .collect()
}

fn item_href(ledger: &Ledger, number: &str) -> String {
    format!("{}/item/{}", ledger_href(ledger), url_segment(number))
}

/// `urn:iki:gonk:system:workers`: JSON, under the viewer's capability, for the ledgers they may
/// read. A ledger whose read fails is left out and named, so its rows say what they could not see.
pub struct Workers {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for Workers {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        system::require(inv.capability)?;
        let ledgers = readable_ledgers(&self.web, inv.capability);
        let mut workers = Vec::new();
        let mut claimed = Vec::new();
        let mut failed = Vec::new();
        for ledger in menu_order(&ledgers) {
            match select(inv, ledger, STATUS_QUERY).await {
                Ok(rows) => workers.extend(workers_of(ledger, &rows)),
                Err(_) => failed.push(ledger.name().to_string()),
            }
            match select(inv, ledger, CLAIMED_QUERY).await {
                Ok(rows) => claimed.extend(rows.iter().map(|row| {
                    let number = cell(row, "number").unwrap_or_default();
                    json!({
                        "ledger": ledger.name(),
                        "item": number,
                        "href": item_href(ledger, number),
                        "title": one_line(cell(row, "title").unwrap_or_default()),
                        "holder": cell(row, "holder"),
                    })
                })),
                Err(_) => failed.push(ledger.name().to_string()),
            }
        }
        Ok(Representation::new(
            ikigai_core::ReprType::new(JSON).with_param("charset", "utf-8"),
            json!({ "workers": workers, "claimed": claimed, "unread_ledgers": failed })
                .to_string()
                .into_bytes(),
        )
        .with_expiry(Expiry::Always))
    }

    fn name(&self) -> &str {
        "gonk-system-workers"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("Each worker's status item, and what is claimed now")
            .summary(
                "For each open item labeled worker-status:<name> in a ledger this caller may \
                 read: its latest Worker: and Disk: lines, as the worker wrote them, and every \
                 item claimed now in those ledgers.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(system::CAP_READ)
            .output(JSON)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_string_in_a_name_is_cut() {
        assert_eq!(names_only("urn:a:b?secret=1"), "urn:a:b");
        assert_eq!(names_only("volatile urn:a#x urn:b"), "volatile urn:a urn:b");
    }

    #[test]
    fn a_line_is_the_first_and_is_bounded() {
        assert_eq!(one_line("Worker: a\nmore"), "Worker: a");
        let long = "x".repeat(MAX_LINE_CHARS + 1);
        assert_eq!(one_line(&long).chars().count(), MAX_LINE_CHARS + 1);
        assert!(one_line(&long).ends_with('…'));
    }

    #[test]
    fn bytes_read_in_the_largest_whole_unit() {
        assert_eq!(in_bytes(12), "12 B");
        assert_eq!(in_bytes(2048), "2.0 KiB");
        assert_eq!(in_bytes(30 * (1 << 30)), "30.0 GiB");
    }

    #[test]
    fn a_line_of_a_readout_that_does_not_parse_is_counted_and_not_kept() {
        let text = "cache\n  entries  1 / 9 (1 stale)\n  size     5 B / 64.0 MiB\n  \
                    policy   lru\n  evictions  3\n  SECRET unparsable\n  urn:a?x=1  cut      \
                    text/plain                    5 B  1 thread    root\nfailures (…)\n  entries  0\n";
        let out = parse_cache(text);
        assert_eq!((out.entries, out.max_entries, out.stale), (1, 9, 1));
        assert_eq!(out.evictions, 3);
        assert_eq!(out.size, "5 B");
        assert_eq!(out.unread, 1);
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0].name, "urn:a");
        assert_eq!(out.rows[0].threads, "1 thread");
        assert_eq!(out.rows[0].chain, "root");
        assert!(!format!("{out:?}").contains("SECRET"));
    }

    #[test]
    fn the_other_readouts_parse_and_count_the_rest() {
        let (rows, unread) = parse_uncached(
            "uncached (why)\n  urn:a?x=1   ×12  volatile urn:b  [root chain]\n  nonsense\n",
        );
        assert_eq!(unread, 1);
        assert_eq!(rows[0].name, "urn:a");
        assert_eq!(rows[0].count, "12");
        assert_eq!(rows[0].reason, "volatile urn:b");
        assert_eq!(rows[0].chain, "root chain");
        assert_eq!(
            parse_uncached("uncached\n  (nothing computed uncached)\n").0,
            vec![]
        );
        assert_eq!(
            parse_threads("threads\n  urn:t  gen 3\n  bad\n"),
            (vec![("urn:t".to_string(), "3".to_string())], 1)
        );
        assert_eq!(parse_threads("threads\n  (none cut)\n").0, vec![]);
    }
}
