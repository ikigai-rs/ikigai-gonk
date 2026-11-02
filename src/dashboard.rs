//! A ledger's open and closed counts as a chart, and the Dashboard page: one chart per ledger the
//! viewer may read (ledger [#2542](http://localhost:1062/l/default/item/2542)).
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
//! `closedAt` until its operator has run `backfill-closed-at`, and its sentence says so.
//!
//! ★ **What one page costs.** The counts are cached and a chart's render is keyed on its document
//! (`render::rendered_chunks`), so a load between writes asks the store nothing for any ledger. ⚠
//! A write to ANY ledger costs the next load one store query per ledger, because a store read
//! rests on the store's write thread rather than on one ledger's, and one transform, for the chart
//! whose numbers moved (`tests/dashboard_2542.rs`).
//!
//! ⚠ **Not live.** gonk's door has no push stream to declare `data-ui-push` against: no
//! `EdgeConfig::push`, no `urn:cap:kernel:listen` in any grant, and door kernels that cache
//! nothing, so a door's stream would hear no cut. A chart is as of the page's load.
//!
//! ★ **Why the plot has two coordinate systems.** Lengths along y are CSS pixels, so label spacing
//! holds at any width. Lengths along x are percentages of the plot (the lines, in a nested `svg`
//! stretched over a 0–1000 `viewBox`), so one chart fits 320 CSS px and a wide page with its text
//! at the same size: a single `viewBox` would shrink the labels along with the lines.

use async_trait::async_trait;
use ikigai_core::{Description, Endpoint, Error, Invocation, Representation, Result, Verb};
use ikigai_fn::html::html_text;
use ikigai_ledger::Ledger;
use std::sync::Arc;

use crate::render::{self, element, envelope, wrap};
use crate::web::{
    fetch, html, html_only, ledger_href, menu_order, nav_on_dashboard, readable_ledgers, request,
    with, Lacking, Web,
};

/// The Dashboard page.
pub const PAGE_IRI: &str = "urn:iki:gonk:page:dashboard";
/// [`PAGE_IRI`]'s path, the header's "Dashboard".
pub const PAGE_PATH: &str = "/dashboard";

/// The Dashboard's slot for its charts.
const CHARTS_SLOT: &str = "charts";

/// A chart's slot for its table's rows.
const ROWS_SLOT: &str = "rows";

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

/// What a read of the counts that failed says, where the chart would have been: `counts`' own
/// sentence when it has one for a person (the ledger's reads give the same caller the same
/// sentence at `/iki/ledger/…`), else that there is no chart.
fn unread(error: &Error) -> String {
    let detail = match error {
        Error::Endpoint(reason) => format!(": {reason}"),
        _ => String::new(),
    };
    format!("No chart: the counts could not be read{detail}.")
}

/// One chart, ready to draw: its document for the stylesheet, and its table's rows.
pub(crate) struct Chart {
    document: String,
    rows: String,
}

/// Draw `charts`, in order: each document's render is keyed on its bytes, so a chart whose numbers
/// did not move is a cache hit.
pub(crate) async fn drawn_charts(inv: &Invocation<'_>, charts: Vec<Chart>) -> Result<String> {
    let mut out = String::new();
    for chart in charts {
        let html = render::rendered_chunks(inv, &[chart.document]).await?;
        let slots = [(ROWS_SLOT.to_string(), chart.rows)];
        out.push_str(&render::splice(html, &slots).map_err(crate::web::render_err)?);
    }
    Ok(out)
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
    let chart = match counts(inv, ledger).await {
        Err(Error::Denied(_)) => return None,
        Err(error) => {
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

/// `/dashboard`: a chart per ledger this caller may read. Public like `/l`, and for the same
/// reason: it lists only what `web::readable_ledgers` allows, and a caller who may read none is told
/// how to get one.
pub struct DashboardPage {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for DashboardPage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        let ledgers = readable_ledgers(&self.web, inv.capability);
        let mut children = nav_on_dashboard(&self.web, inv.capability, &ledgers);
        if ledgers.is_empty() {
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
        let mut charts = Vec::new();
        for ledger in menu_order(&ledgers) {
            let href = ledger_href(ledger);
            if let Some(chart) = chart_chunk(inv, ledger, Lines::Both, Some(&href)).await {
                charts.push(chart);
            }
        }
        children.push_str(&render::slot(CHARTS_SLOT));
        let count = match charts.len() {
            1 => "1 ledger you may read".to_string(),
            n => format!("{n} ledgers you may read"),
        };
        let doc = envelope(
            "page",
            &[
                ("view", "dashboard"),
                ("full", "true"),
                ("ui-theme", render::theme_of(inv)),
                ("title", "Dashboard"),
                ("count", &count),
            ],
            &children,
        );
        let shell = render::render(&doc, true).map_err(crate::web::render_err)?;
        let charts = drawn_charts(inv, charts).await?;
        Ok(html(
            render::splice(shell, &[(CHARTS_SLOT.to_string(), charts)])
                .map_err(crate::web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-page-dashboard"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("The Dashboard, as a page")
            .summary(
                "A chart of the daily open and closed counts of every ledger this caller may \
                 read, each linking to its ledger, as an HTML page. A ledger the caller may not \
                 read is absent. A caller who may read none is told how to get one.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(crate::web::as_html_arg())
            .output(crate::web::HTML)
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
