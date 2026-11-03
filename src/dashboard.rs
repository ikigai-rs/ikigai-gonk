//! A ledger's open and closed counts as a chart, and the Dashboard page: one chart per ledger the
//! viewer may read (ledger [#2542](http://localhost:1062/l/default/item/2542)). Its agent panels are
//! [`crate::agents`].
//!
//! ```text
//! urn:iki:ledger:{name}:counts days=365     the ledger's own daily series, as CSV
//!   → the chart's geometry and sentences, computed here (the xrust subset has no arithmetic)
//!   → one chunk document per chart (`render::chunk`)
//!   → web/gonk.xsl's `view:chart`: inline SVG, a sentence and a data table's shell
//!   → the table's rows spliced in (`table_rows`)
//! ```
//!
//! ★ **A view over the counts resource, read under the VIEWER's capability.** A ledger whose
//! counts answer `Denied` is left out, the rule `web::readable_ledgers` already applies
//! to the grants it can see. Any other failure is said where the chart would be, because the
//! rest of the page is still good: `counts` refuses a ledger holding a closed item with no
//! `closedAt` until its operator has run `backfill-closed-at`, and its sentence says so; a read
//! past the viewer's time budget says that instead (`unread`).
//!
//! ★ **What one page costs.** The counts are cached and a chart's render is keyed on its document
//! (`render::rendered_chunks`), so a load between writes asks the store nothing for any ledger. ⚠
//! A write to ANY ledger costs the next load one store query per ledger, because a store read
//! rests on the store's write thread rather than on one ledger's, and one transform, for the chart
//! whose numbers moved (`tests/dashboard_2542.rs`). The agent panels' cost is [`crate::agents`]'s.
//!
//! ★ **Two tabs, and only the one shown costs anything** (ledger
//! [#2971](http://localhost:1062/l/default/item/2971)). [`Tab::Ledgers`] and [`Tab::Agents`] each
//! have a URL that answers a whole page drawing that panel alone, and a fragment that is the panel
//! (`panel_html`, which both come through). The other panel is an empty, `hidden` element: it holds
//! no timer to hold back and its questions are not asked, on a load or a poll. With no script the
//! tabs are plain links between the pages; `web/gonk.js` turns them into the ARIA pattern.
//!
//! ★ **Two more, for the operator** (ledger [#2972](http://localhost:1062/l/default/item/2972)).
//! [`Tab::Cache`] and [`Tab::System`] are built the same way and hold [`crate::operator`]'s parts.
//! A viewer whose grant does not include one never sees it ([`offered`]), and asking its URL answers
//! `403` ([`Tab::capability`] is declared and enforced).
//!
//! ★ **Live by polling** (ledger [#2967](http://localhost:1062/l/default/item/2967)). Each chart and
//! each agent table is its own fragment (`live`), and an open page asks for it every
//! [`POLL_EVERY`] with the validator of the answer it holds, so an unchanged one costs no body:
//! the transport answers `If-None-Match` from the bytes this module composed, after composing them
//! under the viewer's capability, so a viewer who lost the grant gets the `403` rather than a `304`.
//! The answers stay `no-store` (`Expiry::Always`), so no browser cache holds a viewer's numbers
//! and the validator is `web/gonk.js`'s to send; that file also holds the pause control, the
//! hidden tab, a focused panel and the one polite sentence. With no script the page is as of its
//! load and says so.
//!
//! ⚠ **Polling, not push.** gonk's door has no push stream to declare `data-ui-push` against: no
//! `EdgeConfig::push`, no `urn:cap:kernel:listen` in any grant, and door kernels that cache
//! nothing, so a door's stream would hear no cut.
//!
//! ⚠ **A panel with agents changes its bytes once a minute** even when no number moved, because a
//! link's query holds the window's minute ([`crate::agents`]). So that panel's next poll is a `200`
//! with a body, and `web/gonk.js` compares it without its links and leaves the page alone: a link
//! keeps asking what its number answered.
//!
//! ★ **Why the plot has two coordinate systems.** Lengths along y are CSS pixels, so label spacing
//! holds at any width. Lengths along x are percentages of the plot (the lines, in a nested `svg`
//! stretched over a 0–1000 `viewBox`), so one chart fits 320 CSS px and a wide page with its text
//! at the same size: a single `viewBox` would shrink the labels along with the lines.

use async_trait::async_trait;
use ikigai_core::{
    Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Representation,
    Result, Verb,
};
use ikigai_fn::html::{attr, html_text, url_segment};
use ikigai_ledger::Ledger;
use std::sync::Arc;

use crate::render::{self, element, envelope, wrap};
use crate::web::{
    arg, as_html_arg, binding, fetch, html, html_only, ledger_href, menu_order, nav_on_dashboard,
    readable_ledgers, request, require_read, with, Lacking, Web, HTML,
};

/// The Dashboard page.
pub const PAGE_IRI: &str = "urn:iki:gonk:page:dashboard";
/// [`PAGE_IRI`]'s path, the header's "Dashboard".
pub const PAGE_PATH: &str = "/dashboard";

/// The Dashboard's slot for the panel it shows.
const PANEL_SLOT: &str = "panel";

/// A chart's slot for its table's rows, and an agent panel's ([`crate::agents`]).
pub(crate) const ROWS_SLOT: &str = "rows";

/// The Ledgers tab, as a page.
pub const LEDGERS_PAGE_IRI: &str = "urn:iki:gonk:page:dashboard:ledgers";
/// The Agents tab, as a page.
pub const AGENTS_PAGE_IRI: &str = "urn:iki:gonk:page:dashboard:agents";
/// The Ledgers tab's panel, as a fragment.
pub const LEDGERS_PANEL_IRI: &str = "urn:iki:gonk:fragment:dashboard-panel:ledgers";
/// The Agents tab's panel, as a fragment.
pub const AGENTS_PANEL_IRI: &str = "urn:iki:gonk:fragment:dashboard-panel:agents";

/// The Cache tab, as a page.
pub const CACHE_PAGE_IRI: &str = "urn:iki:gonk:page:dashboard:cache";
/// The System tab, as a page.
pub const SYSTEM_PAGE_IRI: &str = "urn:iki:gonk:page:dashboard:system";
/// The Cache tab's panel, as a fragment.
pub const CACHE_PANEL_IRI: &str = "urn:iki:gonk:fragment:dashboard-panel:cache";
/// The System tab's panel, as a fragment.
pub const SYSTEM_PANEL_IRI: &str = "urn:iki:gonk:fragment:dashboard-panel:system";

/// A tab of the Dashboard. `/dashboard` itself shows the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Ledgers,
    Agents,
    /// An operator's ([`crate::operator`]): needs `urn:cap:kernel:inspect`.
    Cache,
    /// An operator's ([`crate::system`]): needs `urn:cap:gonk:system:read`.
    System,
}

impl Tab {
    /// Every tab, in the order the page lists them.
    pub const ALL: [Tab; 4] = [Tab::Ledgers, Tab::Agents, Tab::Cache, Tab::System];
    /// The tabs anyone who may read a ledger is offered.
    pub const EVERYONE: [Tab; 2] = [Tab::Ledgers, Tab::Agents];
    /// The tabs only their grant's holder is offered, or may ask for.
    pub const OPERATOR: [Tab; 2] = [Tab::Cache, Tab::System];

    /// What its ids and paths call it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Tab::Ledgers => "ledgers",
            Tab::Agents => "agents",
            Tab::Cache => "cache",
            Tab::System => "system",
        }
    }

    /// What a person reads on it.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Tab::Ledgers => "Ledgers",
            Tab::Agents => "Agents",
            Tab::Cache => "Cache",
            Tab::System => "System",
        }
    }

    /// What its page, panel and parts require, or `None` for a tab anyone may ask for.
    pub fn capability(self) -> Option<&'static str> {
        match self {
            Tab::Ledgers | Tab::Agents => None,
            Tab::Cache => Some(crate::operator::CAP_INSPECT),
            Tab::System => Some(crate::system::CAP_READ),
        }
    }

    /// `description`, declaring [`Tab::capability`].
    pub(crate) fn declared(self, description: Description) -> Description {
        match self.capability() {
            Some(capability) => description.requires(capability),
            None => description,
        }
    }

    /// Enforce [`Tab::capability`], as well as declaring it.
    pub(crate) fn require(self, capability: &Capability) -> Result<()> {
        match self.capability() {
            Some(needed) if !capability.allows(needed) => Err(Error::Denied(format!(
                "the Dashboard's {} tab needs {needed} in the grant",
                self.label()
            ))),
            _ => Ok(()),
        }
    }

    /// Its page's path.
    pub fn path(self) -> String {
        format!("{PAGE_PATH}/{}", self.name())
    }

    /// Its panel's path, what the script fetches on a switch.
    pub fn panel_path(self) -> String {
        format!("{PAGE_PATH}/panel/{}", self.name())
    }
}

/// One chart, as a fragment.
pub const CHART_TEMPLATE: &str = "urn:iki:gonk:fragment:dashboard-chart:{ledger}";
/// [`CHART_TEMPLATE`]'s path.
pub const CHART_ROUTE: &str = "/dashboard/chart/{ledger}";
/// One ledger's agent table, as a fragment.
pub const AGENTS_TEMPLATE: &str = "urn:iki:gonk:fragment:dashboard-agents:{ledger}";
/// [`AGENTS_TEMPLATE`]'s path.
pub const AGENTS_ROUTE: &str = "/dashboard/agents/{ledger}";
/// The sentence over the agent tables, as a fragment: it names the minute they are as of.
pub const NOTE_IRI: &str = "urn:iki:gonk:fragment:dashboard-agents-note";
/// [`NOTE_IRI`]'s path. Literal, and `agents-note` is not `agents/{ledger}`.
pub const NOTE_PATH: &str = "/dashboard/agents-note";

/// How often an open Dashboard asks for each fragment of the panel it shows, as `hx-trigger` spells it.
///
/// A constant rather than a setting: nothing has asked to tune it, and a floor would have to be
/// held for whatever a setting said. `POLL_EVERY_WORDS` is the same cadence for a person.
pub const POLL_EVERY: &str = "30s";
/// [`POLL_EVERY`] as the page says it.
const POLL_EVERY_WORDS: &str = "30 seconds";

/// How many days a chart reaches back. It bounds the polyline and the data table, and so a chart's
/// document and its one transform, however old a ledger is.
const CHART_DAYS: &str = "365";

/// The plot's height and the band under it, in CSS px: y grows down, 0 items at [`PLOT_BOTTOM`].
const HEIGHT: u32 = 176;
const PLOT_TOP: f64 = 20.0;
const PLOT_BOTTOM: f64 = 140.0;
/// The width of the `viewBox` the lines are drawn in, stretched over the plot.
const LINE_WIDTH: f64 = 1000.0;
/// The closest two end-of-line labels may sit, baseline to baseline, in CSS px.
const LABEL_GAP: f64 = 14.0;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Which lines a chart draws: the ledger page's status filter, or both on the Dashboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lines {
    Open,
    Closed,
    Both,
}

impl Lines {
    /// The lines for a ledger page's `status` (`count` has already refused any other value).
    pub fn for_status(status: &str) -> Lines {
        match status {
            "closed" => Lines::Closed,
            "all" => Lines::Both,
            _ => Lines::Open,
        }
    }

    fn open(self) -> bool {
        self != Lines::Closed
    }

    fn closed(self) -> bool {
        self != Lines::Open
    }

    /// The chart's heading on a ledger's own page.
    fn title(self) -> &'static str {
        match self {
            Lines::Open => "Open items over time",
            Lines::Closed => "Closed items over time",
            Lines::Both => "Open and closed items over time",
        }
    }
}

/// One row of the counts resource.
#[derive(Debug, PartialEq, Eq)]
struct Day {
    date: String,
    open: u64,
    closed: u64,
}

/// The counts' CSV face. A row that does not parse is an error, never a gap in a line.
fn parse_counts(csv: &str) -> std::result::Result<Vec<Day>, String> {
    csv.lines()
        .skip(1)
        .map(|line| {
            let mut cells = line.split(',');
            let row = (cells.next(), cells.next(), cells.next(), cells.next());
            match row {
                (Some(date), Some(open), Some(closed), None) if date.len() == 10 => Ok(Day {
                    date: date.to_string(),
                    open: open.parse().map_err(|_| format!("the row `{line}`"))?,
                    closed: closed.parse().map_err(|_| format!("the row `{line}`"))?,
                }),
                _ => Err(format!("the row `{line}`")),
            }
        })
        .collect()
}

/// `2026-11-02` as `(2026, "Nov", 2)`, or `None` for anything else.
fn date_parts(date: &str) -> Option<(&str, &'static str, u32)> {
    let mut parts = date.split('-');
    let year = parts.next()?;
    let month: usize = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    Some((year, MONTHS.get(month.checked_sub(1)?)?, day))
}

/// `2026-11-02` in words: `Nov 2, 2026`.
fn in_words(date: &str) -> String {
    match date_parts(date) {
        Some((year, month, day)) => format!("{month} {day}, {year}"),
        None => date.to_string(),
    }
}

/// An axis tick: `Nov 2`, with the year when it is not the year the chart ends in.
fn tick(date: &str, last: &str) -> String {
    match date_parts(date) {
        Some((year, month, day)) if last.starts_with(year) => format!("{month} {day}"),
        _ => in_words(date),
    }
}

/// The top of the y axis and the distance between its ticks: a step of 1, 2 or 5 times a power of
/// ten, the smallest that reaches `max` in at most four steps. Counts are whole, so a step is too.
fn y_scale(max: u64) -> (u64, u64) {
    let mut power = 1;
    loop {
        for step in [power, 2 * power, 5 * power] {
            if max <= 4 * step {
                return (max.div_ceil(step).max(1) * step, step);
            }
        }
        power *= 10;
    }
}

fn y_of(value: u64, top: u64) -> f64 {
    PLOT_BOTTOM - value as f64 / top as f64 * (PLOT_BOTTOM - PLOT_TOP)
}

/// Where two end-of-line labels go: at their lines' ends, or moved apart just enough to be read,
/// staying in order and over the plot.
fn label_ys(open: f64, closed: f64) -> (f64, f64) {
    if (open - closed).abs() >= LABEL_GAP {
        return (open, closed);
    }
    let middle =
        ((open + closed) / 2.0).clamp(PLOT_TOP + LABEL_GAP / 2.0, PLOT_BOTTOM - LABEL_GAP / 2.0);
    // Open is the upper label when they tie: it is the one a reader is most often after.
    if open <= closed {
        (middle - LABEL_GAP / 2.0, middle + LABEL_GAP / 2.0)
    } else {
        (middle + LABEL_GAP / 2.0, middle - LABEL_GAP / 2.0)
    }
}

/// The sentence that is the chart's text alternative: the latest numbers and the range, in words.
fn summary(days: &[Day], lines: Lines) -> String {
    let (first, last) = (&days[0], &days[days.len() - 1]);
    let numbers = match lines {
        Lines::Open => format!("{} open", last.open),
        Lines::Closed => format!("{} closed", last.closed),
        Lines::Both => format!("{} open, {} closed", last.open, last.closed),
    };
    let range = if days.len() == 1 {
        "One day of counts so far, in UTC.".to_string()
    } else {
        format!(
            "Daily counts from {} to {}, in UTC.",
            in_words(&first.date),
            in_words(&last.date)
        )
    };
    format!("As of {}: {numbers}. {range}", in_words(&last.date))
}

/// The `points` of one line, x in the `viewBox` of [`LINE_WIDTH`] and y in CSS px.
fn points(days: &[Day], value: impl Fn(&Day) -> u64, top: u64) -> String {
    let last = days.len().saturating_sub(1).max(1) as f64;
    let solo = days.len() == 1;
    days.iter()
        .enumerate()
        .map(|(n, day)| {
            let x = if solo {
                LINE_WIDTH
            } else {
                n as f64 / last * LINE_WIDTH
            };
            format!("{x:.1},{:.1}", y_of(value(day), top))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The x axis's ticks: both ends and the middle, as far as there are days for them.
fn x_ticks(days: &[Day]) -> String {
    let last = &days[days.len() - 1].date;
    let tick_at = |n: usize, anchor: &str| {
        let percent = if days.len() == 1 {
            100.0
        } else {
            n as f64 / (days.len() - 1) as f64 * 100.0
        };
        element(
            "x-tick",
            &[
                ("x", &format!("{percent:.1}%")),
                ("anchor", anchor),
                ("label", &tick(&days[n].date, last)),
            ],
            "",
        )
    };
    match days.len() {
        1 => tick_at(0, "end"),
        2 => tick_at(0, "start") + &tick_at(1, "end"),
        n => tick_at(0, "start") + &tick_at(n / 2, "middle") + &tick_at(n - 1, "end"),
    }
}

/// The y axis: a gridline and a number at each step, from zero.
fn y_ticks(top: u64, step: u64) -> String {
    (0..=top / step)
        .map(|n| {
            let value = n * step;
            let y = y_of(value, top);
            element(
                "y-tick",
                &[
                    ("y", &format!("{y:.1}")),
                    ("label-y", &format!("{:.1}", y + 4.0)),
                    (
                        "class",
                        if value == 0 {
                            "chart-axis"
                        } else {
                            "chart-rule"
                        },
                    ),
                    ("label", &value.to_string()),
                ],
                "",
            )
        })
        .collect()
}

/// One series' element: a line, the mark at its end and the label beside it.
fn series(name: &str, days: &[Day], value: impl Fn(&Day) -> u64, top: u64, label_y: f64) -> String {
    let latest = value(&days[days.len() - 1]);
    element(
        "series",
        &[
            ("class", &format!("chart-{name}")),
            ("points", &points(days, value, top)),
            ("end-y", &format!("{:.1}", y_of(latest, top))),
            ("label-y", &format!("{:.1}", label_y + 4.0)),
            ("label", &format!("{name} {latest}")),
        ],
        "",
    )
}

/// The data table's rows as HTML, newest first: the first day, the last, and each day a shown
/// count changed. The counts hold between them, and the table's caption says so, so a quiet year
/// in an old ledger is not 365 identical rows.
///
/// ★ **Written here, not by the stylesheet** (ledger #2542): a transform's cost is quadratic in the
/// nodes it creates (see `render`), and measured on a release build a chart with 300 rows took
/// 1.4 s, on the next load after every write to its ledger, filing an item included. These cells
/// are a date and two numbers, with nothing for a template to decide.
fn table_rows(days: &[Day], lines: Lines) -> String {
    let same = |a: &Day, b: &Day| {
        (!lines.open() || a.open == b.open) && (!lines.closed() || a.closed == b.closed)
    };
    let mut rows = String::new();
    for (n, day) in days.iter().enumerate().rev() {
        if n != 0 && n != days.len() - 1 && same(day, &days[n - 1]) {
            continue;
        }
        rows.push_str(&format!(
            "<tr><th scope=\"row\">{}</th>",
            html_text(&in_words(&day.date))
        ));
        if lines.open() {
            rows.push_str(&format!("<td>{}</td>", day.open));
        }
        if lines.closed() {
            rows.push_str(&format!("<td>{}</td>", day.closed));
        }
        rows.push_str("</tr>");
    }
    rows
}

/// A chart's element, from its series, with a slot for [`table_rows`]. `days` is not empty.
fn drawn(common: &[(&str, &str)], days: &[Day], lines: Lines) -> String {
    let max = days
        .iter()
        .map(|d| {
            let open = if lines.open() { d.open } else { 0 };
            let closed = if lines.closed() { d.closed } else { 0 };
            open.max(closed)
        })
        .max()
        .unwrap_or(0);
    let (top, step) = y_scale(max);
    let last = &days[days.len() - 1];
    let (open_label, closed_label) = match lines {
        Lines::Both => label_ys(y_of(last.open, top), y_of(last.closed, top)),
        _ => (y_of(last.open, top), y_of(last.closed, top)),
    };
    let mut children = y_ticks(top, step) + &x_ticks(days);
    if lines.open() {
        children += &series("open", days, |d| d.open, top, open_label);
    }
    if lines.closed() {
        children += &series("closed", days, |d| d.closed, top, closed_label);
    }
    children += &render::slot(ROWS_SLOT);
    let summary = summary(days, lines);
    let height = HEIGHT.to_string();
    let view_box = format!("0 0 {LINE_WIDTH} {HEIGHT}");
    let mut attributes = common.to_vec();
    attributes.extend([
        ("drawn", "true"),
        ("summary", summary.as_str()),
        ("height", height.as_str()),
        ("view-box", view_box.as_str()),
        ("shows-open", if lines.open() { "true" } else { "false" }),
        (
            "shows-closed",
            if lines.closed() { "true" } else { "false" },
        ),
    ]);
    wrap("chart", &attributes, &children)
}

/// What a read of the counts that failed says, where the chart would have been: why, in a sentence
/// for a person, so an operator can tell a slow ledger from a broken one (ledger
/// [#2970](http://localhost:1062/l/default/item/2970)). The words for each class are written here,
/// because what an error carries is written for a log: a time-budget refusal's cites a ledger item
/// and talks about SPARQL. Only the ledger's own sentences about its counts are repeated.
fn unread(error: &Error) -> String {
    let why = match error {
        Error::Endpoint(reason) if from_the_counts(reason) => reason,
        Error::Timeout(_) => {
            "they took longer than your time budget. An operator can raise it with a \
             urn:cap:store:budget:<ms> grant"
        }
        Error::Unavailable(_) | Error::RetryAfter { .. } => {
            "the store is unavailable right now. Try again shortly"
        }
        Error::Unresolved(_) | Error::NotFound(_) => "the store holding them is missing",
        Error::OverBudget { .. } => "they are more than the store will answer with at once",
        _ => "this server failed while reading them",
    };
    format!("No chart: the counts could not be read: {why}.")
}

/// Whether `reason` is one of the sentences `counts` writes about a ledger's own data, each of
/// which tells its operator what is wrong with the ledger. ⚠ It has to go by the words: `Endpoint`
/// is also what the store's internal failures arrive as (`store: {e}`, `internal: a SPARQL worker
/// ended without answering`), and nothing else tells them apart. If the ledger rewords one, it
/// falls to the plain sentence, and the backfill sentence's test goes red.
fn from_the_counts(reason: &str) -> bool {
    reason.starts_with("counts ") || reason.contains("backfill-closed-at")
}

/// One chart or agent panel, ready to draw: its document for the stylesheet, and its table's rows.
pub(crate) struct Chart {
    pub(crate) document: String,
    pub(crate) rows: String,
    /// Whether a poll could change it. A ledger whose counts refuse until its operator has run
    /// `backfill-closed-at` keeps its sentence and is not asked again.
    pub(crate) live: bool,
}

/// What every poll's element says: asked every [`POLL_EVERY`] or on `gonk:refresh` (a resume or a
/// tab shown again), replaced whole so a refused chart's answer takes the timer with it, its
/// failures never a message, and a tick during an ask dropped rather than queued (ledger #947).
fn polled(kind: &str, path: &str) -> String {
    format!(
        "data-live=\"{kind}\" data-ui-quiet=\"\" hx-get=\"{}\" \
         hx-trigger=\"every {POLL_EVERY}, gonk:refresh\" hx-swap=\"outerHTML\" \
         hx-sync=\"this:drop\"",
        attr(path)
    )
}

/// `html` as a fragment that polls `path`. `.dashboard-live` is `display: contents`, so the element
/// changes nothing about the layout around it.
pub(crate) fn live(kind: &str, path: &str, html: &str) -> String {
    format!(
        "<div class=\"dashboard-live\" {}>{html}</div>",
        polled(kind, path)
    )
}

/// What a fragment of one ledger declares: a read of that ledger, under the viewer's capability.
pub(crate) fn of_one_ledger(description: Description) -> Description {
    description
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(ikigai_ledger::CAP_READ)
        .requires(ikigai_store::CAP_READ_GRAPH)
        .input(
            arg("ledger", "Which ledger.")
                .binding()
                .default_value("default")
                .optional(),
        )
        .input(as_html_arg())
        .output(HTML)
}

/// The path of the agent table [`AGENTS_ROUTE`] names.
pub(crate) fn agents_path(ledger: &Ledger) -> String {
    format!("{PAGE_PATH}/agents/{}", url_segment(ledger.name()))
}

/// Whether a read of the counts failed for good: `counts` refuses a ledger holding a closed item
/// with no `closedAt` until its operator has run `backfill-closed-at`, and no poll can change that.
/// Any other failure is worth asking again. `tests/dashboard_2542.rs` pins this sentence.
fn refused_for_good(error: &Error) -> bool {
    matches!(error, Error::Endpoint(reason) if reason.contains("backfill-closed-at"))
}

/// Draw `charts`, in order: each document's render is keyed on its bytes, so a chart whose numbers
/// did not move is a cache hit.
pub(crate) async fn drawn_charts(inv: &Invocation<'_>, charts: Vec<Chart>) -> Result<String> {
    let mut out = String::new();
    for chart in charts {
        out.push_str(&drawn_chart(inv, chart).await?);
    }
    Ok(out)
}

/// One of [`drawn_charts`].
pub(crate) async fn drawn_chart(inv: &Invocation<'_>, chart: Chart) -> Result<String> {
    let html = render::rendered_chunks(inv, &[chart.document]).await?;
    let slots = [(ROWS_SLOT.to_string(), chart.rows)];
    render::splice(html, &slots).map_err(crate::web::render_err)
}

/// A Dashboard chart as its fragment, or `None` when this viewer may not read the ledger's counts.
/// The page and the poll both come through here, so they cannot draw different things.
async fn chart_fragment(inv: &Invocation<'_>, ledger: &Ledger) -> Result<Option<String>> {
    let href = ledger_href(ledger);
    let Some(chart) = chart_chunk(inv, ledger, Lines::Both, Some(&href)).await else {
        return Ok(None);
    };
    let is_live = chart.live;
    let drawn = drawn_chart(inv, chart).await?;
    Ok(Some(if is_live {
        let path = format!("{PAGE_PATH}/chart/{}", url_segment(ledger.name()));
        live("chart", &path, &drawn)
    } else {
        format!("<div class=\"dashboard-live\">{drawn}</div>")
    }))
}

/// The sentence over the agent panels, as its fragment. It is asked for like a panel, so the minute
/// it names moves with them, and it is not news: `web/gonk.js` announces nothing for it.
fn agents_note(inv: &Invocation<'_>) -> String {
    format!(
        "<p class=\"note\" {}>{}</p>",
        polled("note", NOTE_PATH),
        html_text(&crate::agents::as_of(inv))
    )
}

/// One chart, or `None` when this viewer may not read the ledger's counts. `link` makes it a
/// Dashboard chart: titled with the ledger's name and linking to its page.
pub(crate) async fn chart_chunk(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    lines: Lines,
    link: Option<&str>,
) -> Option<Chart> {
    let id = format!("chart-{}", ledger.name());
    let table_label = match link {
        Some(_) => format!("Data table for {}", ledger.name()),
        None => "Data table".to_string(),
    };
    let mut common = vec![
        ("id", id.as_str()),
        ("title", link.map_or(lines.title(), |_| ledger.name())),
        ("table-label", table_label.as_str()),
    ];
    if let Some(href) = link {
        common.push(("href", href));
    }
    let mut rows = String::new();
    let mut live = true;
    let chart = match counts(inv, ledger).await {
        Err(Error::Denied(_)) => return None,
        Err(error) => {
            live = !refused_for_good(&error);
            let note = unread(&error);
            common.extend([("drawn", "false"), ("summary", note.as_str())]);
            element("chart", &common, "")
        }
        Ok(days) if days.is_empty() => {
            common.extend([
                ("drawn", "false"),
                (
                    "summary",
                    "No items filed yet, so there is nothing to chart.",
                ),
            ]);
            element("chart", &common, "")
        }
        Ok(days) => {
            rows = table_rows(&days, lines);
            drawn(&common, &days, lines)
        }
    };
    Some(Chart {
        document: render::chunk(&chart),
        rows,
        live,
    })
}

/// The ledger's daily series, under the invocation's capability.
async fn counts(inv: &Invocation<'_>, ledger: &Ledger) -> Result<Vec<Day>> {
    let read = with(
        request(Verb::Source, &format!("{}counts", ledger.prefix()))?,
        "days",
        CHART_DAYS,
    );
    let csv = fetch(inv, read).await?;
    parse_counts(&String::from_utf8_lossy(&csv))
        .map_err(|row| Error::Endpoint(format!("the ledger's counts carry {row}")))
}

/// A tab's panel, as the page and the fragment both draw it. Only its own tab's questions are asked.
///
/// ★ Written here, not by the stylesheet, so a switch's fetch is these bytes with no page around
/// them: the charts and tables in it have had their transforms, and the rest is a heading and a
/// sentence.
async fn panel_html(
    web: &Web,
    inv: &Invocation<'_>,
    ledgers: &[Ledger],
    tab: Tab,
) -> Result<String> {
    Ok(match tab {
        Tab::Cache | Tab::System => crate::operator::panel(web, inv, tab).await,
        Tab::Ledgers => {
            let mut charts = Vec::new();
            for ledger in menu_order(ledgers) {
                if let Some(chart) = chart_fragment(inv, ledger).await? {
                    charts.push(chart);
                }
            }
            let count = match charts.len() {
                1 => "1 ledger you may read".to_string(),
                n => format!("{n} ledgers you may read"),
            };
            format!(
                "<h2 class=\"sr-only\" id=\"dashboard-ledgers\">Ledgers</h2>\
                 <p class=\"note\">{count}</p><div class=\"chart-grid\">{}</div>",
                charts.concat()
            )
        }
        Tab::Agents => {
            let agents = fetch(inv, request(Verb::Source, crate::agents::VIEW_IRI)?).await?;
            format!(
                "<h2 class=\"sr-only\" id=\"dashboard-agents\">Agents</h2>{}{}",
                agents_note(inv),
                String::from_utf8_lossy(&agents)
            )
        }
    })
}

/// Whether this viewer is offered `tab`. An operator tab they may not read is not on their page at
/// all, the rule for a ledger they may not read; nor is the System tab on a server that does not
/// bind its resources, the rule `onboard::offers_page` keeps for a page with nothing behind it.
pub(crate) fn offered(web: &Web, capability: &Capability, tab: Tab) -> bool {
    match tab {
        Tab::Ledgers | Tab::Agents => true,
        Tab::Cache => capability.allows(crate::operator::CAP_INSPECT),
        Tab::System => {
            capability.allows(crate::system::CAP_READ)
                && ikigai_core::Iri::parse(crate::system::DISK)
                    .is_ok_and(|iri| web.hub.describe(&iri).is_some())
        }
    }
}

/// A tab's link and its panel's element: the panel it shows holds [`PANEL_SLOT`], the other nothing.
fn tabs_and_panels(shown: Tab, tabs: &[Tab]) -> String {
    let mut links = String::new();
    let mut panels = String::new();
    for &tab in tabs {
        let current = if tab == shown { "true" } else { "false" };
        let attributes = [
            ("name", tab.name()),
            ("label", tab.label()),
            ("current", current),
        ];
        let (path, panel_path) = (tab.path(), tab.panel_path());
        let mut link = attributes.to_vec();
        link.extend([("href", path.as_str()), ("panel-href", panel_path.as_str())]);
        links.push_str(&element("dashboard-tab", &link, ""));
        panels.push_str(&if tab == shown {
            wrap("dashboard-panel", &attributes, &render::slot(PANEL_SLOT))
        } else {
            element("dashboard-panel", &attributes, "")
        });
    }
    links + &panels
}

/// `/dashboard`, `/dashboard/ledgers` and `/dashboard/agents`: one tab's panel, whole, with no script
/// needed. Public like `/l`, and for the same reason: it lists only what `web::readable_ledgers`
/// allows, and a caller who may read none is told how to get one.
pub struct DashboardPage {
    pub web: Arc<Web>,
    pub tab: Tab,
}

#[async_trait]
impl Endpoint for DashboardPage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        self.tab.require(inv.capability)?;
        let ledgers = readable_ledgers(&self.web, inv.capability);
        let mut children = nav_on_dashboard(&self.web, inv.capability, &ledgers);
        // The tabs this viewer has, and the one asked for, whose capability is checked above.
        let tabs: Vec<Tab> = Tab::ALL
            .into_iter()
            .filter(|&tab| tab == self.tab || offered(&self.web, inv.capability, tab))
            .collect();
        let operator = tabs.iter().any(|tab| tab.capability().is_some());
        if ledgers.is_empty() && !operator {
            children.push_str(&Lacking::ledgers().element());
            let doc = envelope(
                "page",
                &[
                    ("view", "empty"),
                    ("full", "true"),
                    ("ui-theme", render::theme_of(inv)),
                    ("title", "No ledger to show"),
                ],
                &children,
            );
            return Ok(html(
                render::render(&doc, true).map_err(crate::web::render_err)?,
            ));
        }
        let panel = panel_html(&self.web, inv, &ledgers, self.tab).await?;
        children.push_str(&tabs_and_panels(self.tab, &tabs));
        let title = format!("Dashboard: {}", self.tab.label());
        let doc = envelope(
            "page",
            &[
                ("view", "dashboard"),
                ("full", "true"),
                ("ui-theme", render::theme_of(inv)),
                ("title", &title),
                ("heading", "Dashboard"),
                (
                    "live-running",
                    &format!(
                        "These numbers update every {POLL_EVERY_WORDS} while this browser tab is \
                         showing."
                    ),
                ),
            ],
            &children,
        );
        let shell = render::render(&doc, true).map_err(crate::web::render_err)?;
        Ok(html(
            render::splice(shell, &[(PANEL_SLOT.to_string(), panel)])
                .map_err(crate::web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        match self.tab {
            Tab::Cache => "gonk-page-dashboard-cache",
            Tab::System => "gonk-page-dashboard-system",
            Tab::Ledgers | Tab::Agents => "gonk-page-dashboard",
        }
    }

    fn describe(&self) -> Description {
        let description = Description::new(self.name())
            .title("The Dashboard, as a page")
            .summary(
                "One tab of the Dashboard as an HTML page: a chart of the daily open and closed \
                 counts of every ledger this caller may read, each linking to its ledger, or a \
                 table of each ledger's agents. A ledger the caller may not read is absent. A \
                 caller who may read none is told how to get one. An open page refreshes the \
                 shown tab's charts or tables from their own fragments, and the other tab asks \
                 nothing.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(as_html_arg())
            .output(HTML);
        self.tab.declared(description)
    }
}

/// `urn:iki:gonk:fragment:dashboard-panel:{ledgers,agents}`: what an open Dashboard fetches once, on
/// a switch to that tab. Public like the page: a caller who has lost every ledger gets a panel with
/// none in it, whose first line says so.
pub struct PanelFragment {
    pub web: Arc<Web>,
    pub tab: Tab,
}

#[async_trait]
impl Endpoint for PanelFragment {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        self.tab.require(inv.capability)?;
        let ledgers = readable_ledgers(&self.web, inv.capability);
        Ok(html(panel_html(&self.web, inv, &ledgers, self.tab).await?))
    }

    fn name(&self) -> &str {
        match self.tab {
            Tab::Cache => "gonk-fragment-dashboard-panel-cache",
            Tab::System => "gonk-fragment-dashboard-panel-system",
            Tab::Ledgers | Tab::Agents => "gonk-fragment-dashboard-panel",
        }
    }

    fn describe(&self) -> Description {
        let description = Description::new(self.name())
            .title("One Dashboard tab's panel, as a fragment")
            .summary(
                "What the Dashboard's page draws under this tab, with no page around it: the \
                 charts or the agent tables of every ledger this caller may read, each of which \
                 then polls its own fragment.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(as_html_arg())
            .output(HTML);
        self.tab.declared(description)
    }
}

/// [`Tab::OPERATOR`]'s pages, panels and parts, and the workers' resource: what `web::space` binds.
pub(crate) fn bind_operator(mut space: EndpointSpace, web: &Arc<Web>) -> EndpointSpace {
    for (tab, page, panel) in [
        (Tab::Cache, CACHE_PAGE_IRI, CACHE_PANEL_IRI),
        (Tab::System, SYSTEM_PAGE_IRI, SYSTEM_PANEL_IRI),
    ] {
        space = space
            .bind(
                Exact::new(page),
                DashboardPage {
                    web: Arc::clone(web),
                    tab,
                },
            )
            .bind(
                Exact::new(panel),
                PanelFragment {
                    web: Arc::clone(web),
                    tab,
                },
            );
    }
    for part in crate::operator::Part::ALL {
        space = space.bind(
            Exact::new(part.iri()),
            crate::operator::PartFragment {
                web: Arc::clone(web),
                part,
            },
        );
    }
    space.bind(
        Exact::new(crate::operator::WORKERS),
        crate::operator::Workers {
            web: Arc::clone(web),
        },
    )
}

/// `urn:iki:gonk:fragment:dashboard-chart:{ledger}`: what an open Dashboard asks for one chart.
pub struct ChartFragment;

#[async_trait]
impl Endpoint for ChartFragment {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        let ledger = Ledger::parse(&binding(inv, "ledger")?)?;
        require_read(inv.capability, &ledger)?;
        chart_fragment(inv, &ledger)
            .await?
            .map(html)
            .ok_or_else(|| Error::Denied("this grant does not read that ledger's counts".into()))
    }

    fn name(&self) -> &str {
        "gonk-fragment-dashboard-chart"
    }

    fn describe(&self) -> Description {
        of_one_ledger(
            Description::new(self.name())
                .title("One Dashboard chart, as a fragment")
                .summary(
                    "The chart the Dashboard draws for one ledger this caller may read. Its ETag \
                     is a hash of these bytes, so an unchanged chart answers If-None-Match with a \
                     304.",
                ),
        )
    }
}

/// `urn:iki:gonk:fragment:dashboard-agents-note`: the sentence over the agent tables.
pub struct NoteFragment;

#[async_trait]
impl Endpoint for NoteFragment {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        Ok(html(agents_note(inv)))
    }

    fn name(&self) -> &str {
        "gonk-fragment-dashboard-agents-note"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("The sentence over the Dashboard's agent tables, as a fragment")
            .summary("What a number in the agent tables does, and the minute they are as of.")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(as_html_arg())
            .output(HTML)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, open: u64, closed: u64) -> Day {
        Day {
            date: date.to_string(),
            open,
            closed,
        }
    }

    /// ★ Ledger #2970: every class says why, and what the error carries for a log stays out.
    #[test]
    fn a_failed_read_says_why_and_echoes_nothing_internal() {
        let secret = "SECRET internal text";
        let says = |error: Error, why: &str| {
            let note = unread(&error);
            assert_eq!(
                note,
                format!("No chart: the counts could not be read: {why}.")
            );
            assert!(!note.contains(secret), "{note}");
        };
        says(
            Error::Timeout(secret.into()),
            "they took longer than your time budget. An operator can raise it with a \
             urn:cap:store:budget:<ms> grant",
        );
        let unavailable = "the store is unavailable right now. Try again shortly";
        says(Error::Unavailable(secret.into()), unavailable);
        says(
            Error::RetryAfter {
                delay: None,
                detail: secret.into(),
            },
            unavailable,
        );
        let missing = "the store holding them is missing";
        says(Error::NotFound(secret.into()), missing);
        says(
            Error::Unresolved(ikigai_core::Iri::parse("urn:iki:store:SECRET").unwrap()),
            missing,
        );
        says(
            Error::OverBudget {
                detail: secret.into(),
            },
            "they are more than the store will answer with at once",
        );
        let failed = "this server failed while reading them";
        says(Error::Refused(secret.into()), failed);
        says(Error::Conflict(secret.into()), failed);
        // `Endpoint` is both the store's internal failures and the ledger's sentences.
        says(Error::Endpoint(format!("store: {secret}")), failed);
        says(
            Error::Endpoint(format!("internal: {secret} ended without answering")),
            failed,
        );
        says(Error::Endpoint(format!("counting quads: {secret}")), failed);
        says(
            Error::Endpoint(format!("the ledger's counts carry the row `{secret}`")),
            failed,
        );
    }

    #[test]
    fn the_ledgers_own_sentences_about_its_counts_are_kept() {
        for sentence in [
            "a closed item has no readable closedAt; run backfill-closed-at explicitly before \
             charting",
            "counts refuses multiple filed times for one item",
            "counts found a status event before the item was filed",
        ] {
            assert_eq!(
                unread(&Error::Endpoint(sentence.into())),
                format!("No chart: the counts could not be read: {sentence}.")
            );
        }
    }

    #[test]
    fn the_counts_csv_parses_and_a_bad_row_is_an_error() {
        let csv = "date,open,closed\n2026-10-27,12,5\n2026-10-28,10,8\n";
        assert_eq!(
            parse_counts(csv).unwrap(),
            vec![day("2026-10-27", 12, 5), day("2026-10-28", 10, 8)]
        );
        assert_eq!(parse_counts("date,open,closed\n").unwrap(), vec![]);
        assert!(parse_counts("date,open,closed\n2026-10-27,12\n").is_err());
        assert!(parse_counts("date,open,closed\n2026-10-27,a,5\n").is_err());
    }

    #[test]
    fn dates_read_as_words() {
        assert_eq!(in_words("2026-11-02"), "Nov 2, 2026");
        assert_eq!(tick("2026-11-02", "2026-12-01"), "Nov 2");
        assert_eq!(tick("2025-12-30", "2026-01-02"), "Dec 30, 2025");
        assert_eq!(in_words("not a date"), "not a date");
    }

    #[test]
    fn the_y_axis_steps_are_round_whole_numbers_that_reach_the_data() {
        assert_eq!(y_scale(0), (1, 1));
        assert_eq!(y_scale(1), (1, 1));
        assert_eq!(y_scale(4), (4, 1));
        assert_eq!(y_scale(5), (6, 2));
        assert_eq!(y_scale(9), (10, 5));
        assert_eq!(y_scale(21), (30, 10));
        assert_eq!(y_scale(2834), (3000, 1000));
        for max in 0..5000 {
            let (top, step) = y_scale(max);
            assert!(top >= max && top % step == 0 && top / step <= 5, "{max}");
        }
    }

    #[test]
    fn end_labels_keep_apart_inside_the_plot() {
        assert_eq!(label_ys(30.0, 100.0), (30.0, 100.0));
        // A tie at zero, the usual state of a young ledger's closed line.
        let (open, closed) = label_ys(PLOT_BOTTOM, PLOT_BOTTOM);
        assert_eq!(closed - open, LABEL_GAP);
        assert!(closed <= PLOT_BOTTOM);
        // Near but crossed: the order holds.
        let (open, closed) = label_ys(25.0, 21.0);
        assert_eq!(open - closed, LABEL_GAP);
        assert!(closed >= PLOT_TOP);
    }

    #[test]
    fn the_summary_says_the_latest_numbers_and_the_range() {
        let days = [day("2026-10-27", 12, 5), day("2026-11-02", 10, 8)];
        assert_eq!(
            summary(&days, Lines::Both),
            "As of Nov 2, 2026: 10 open, 8 closed. Daily counts from Oct 27, 2026 to Nov 2, 2026, in UTC."
        );
        assert!(summary(&days, Lines::Open).starts_with("As of Nov 2, 2026: 10 open. Daily"));
        assert!(summary(&days, Lines::Closed).starts_with("As of Nov 2, 2026: 8 closed. Daily"));
        assert_eq!(
            summary(&days[..1], Lines::Open),
            "As of Oct 27, 2026: 12 open. One day of counts so far, in UTC."
        );
    }

    #[test]
    fn a_line_spans_the_plot_and_a_lone_day_sits_at_its_end() {
        let days = [
            day("2026-10-27", 0, 0),
            day("2026-10-28", 4, 0),
            day("2026-10-29", 2, 0),
        ];
        assert_eq!(
            points(&days, |d| d.open, 4),
            "0.0,140.0 500.0,20.0 1000.0,80.0"
        );
        assert_eq!(points(&days[1..2], |d| d.open, 4), "1000.0,20.0");
    }

    #[test]
    fn the_table_keeps_the_ends_and_the_days_a_shown_count_changed() {
        let days = [
            day("2026-10-01", 1, 0),
            day("2026-10-02", 1, 0),
            day("2026-10-03", 1, 1),
            day("2026-10-04", 1, 1),
            day("2026-10-05", 1, 1),
        ];
        let rows = |lines| table_rows(&days, lines).matches("<tr>").count();
        assert_eq!(rows(Lines::Both), 3);
        // The close moved only the closed count, so an open-only table has only its two ends.
        assert_eq!(rows(Lines::Open), 2);
    }
}
