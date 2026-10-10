//! How many rows one server-rendered page draws — the ONE rule the ledger listing and the
//! review queue share (ledger #480).
//!
//! The two pages used to carry a `ROWS`/`MAX_ROWS` pair and a `rows_wanted` each, and the
//! copies drifted into opposite answers: the listing drew `?limit=1000` at the cap while the
//! queue refused it, and each comment described its own choice as the house rule.
//!
//! # The rule: draw at the cap, and SAY so
//!
//! A count past [`MAX_ROWS`] is drawn at [`MAX_ROWS`], and the page states what it drew out of
//! what — with [`cap_clause`] naming the cap whenever the cap is what stopped it. A value that
//! is not a positive count (or `all`) is refused, naming the cap.
//!
//! ⚠ That is not the truncation the recorded lesson forbids ("a bound must refuse, not
//! truncate"). The lesson's test is whether the consumer can tell the cut answer from a
//! complete one, and here it always can: neither page draws a row without the count sentence
//! saying "showing the first 500 of 700", so a cut is never read as a total. Refusing would
//! have been the INCONSISTENT choice, because `?limit=all` has always meant "as many as one
//! page draws" on both pages — itself a clamp — so a refused `1000` beside an accepted `all`
//! would refuse the number and grant the word for the same request. And the input is a URL a
//! person typed: an error page that names the cap costs a second request to get the answer
//! the clamp already gives, with the same disclosure.
//!
//! Where the lesson DOES apply is the machine faces (the Turtle and JSON reads behind these
//! pages), which carry no sentence; those bounds are theirs, not this module's.

use std::num::IntErrorKind;

use ikigai_core::{Error, Result};

/// How many rows a page draws unless the caller asks for more.
///
/// ★ **This is a latency bound and the number is measured, not chosen.** `xrust` builds the
/// result tree node by node and the cost is in what it WRITES, not what it reads: the ledger
/// list page costs ~150 ms of chrome plus ~6 ms per row at fifty rows and ~15 ms per row at
/// four hundred — 410 rows measured 6.5 s, which is the whole of the ~6 s page (parsing the
/// Turtle, adding the view triples and serializing RDF/XML together cost 70 ms of it).
/// Handing the stylesheet a SMALLER GRAPH does not help — a 3.4× smaller input moved the
/// same render by 3% — so the only lever is fewer rows. `examples/render-cost.rs` is how to
/// take those numbers again; ledger #443 carries the first set.
pub const ROWS: usize = 50;

/// The ceiling on one render, whatever `limit` asks for — about eight seconds' worth.
/// `?limit=all` means this, and so does any larger count; see the module's rule. A caller
/// who wants every row wants a machine face, not a page.
pub const MAX_ROWS: usize = 500;

/// How many rows the caller asked to see: [`ROWS`] when `limit` is absent, [`MAX_ROWS`] for
/// `all` or for any count past it, and the count itself otherwise. Anything that is not a
/// positive count is refused, and the refusal names the cap.
pub fn rows_wanted(limit: Option<&str>) -> Result<usize> {
    match limit {
        None => Ok(ROWS),
        Some("all") => Ok(MAX_ROWS),
        Some(other) => match other.parse::<usize>() {
            Ok(n) if n > 0 => Ok(n.min(MAX_ROWS)),
            // A count too large for a `usize` is still a count past the cap.
            Err(e) if *e.kind() == IntErrorKind::PosOverflow => Ok(MAX_ROWS),
            _ => Err(Error::InvalidArgument {
                name: "limit".to_string(),
                detail: format!(
                    "`{other}` is not a positive number of rows, or `all` (one page draws at \
                     most {MAX_ROWS}; a larger count is drawn at {MAX_ROWS} and the page says so)"
                ),
            }),
        },
    }
}

/// The row count a "show more" link should ask for, or `None` when this page cannot draw
/// more than it already has: every row is shown, or the cap is.
///
/// ⚠ A link labeled "show all 700" that draws 500 would be the silent clamp this module
/// exists to rule out — and once 500 are shown it would link the page to itself.
pub fn more(shown: usize, total: usize) -> Option<usize> {
    let next = total.min(MAX_ROWS);
    (next > shown).then_some(next)
}

/// The clause a count sentence carries when the CAP is what stopped the page — it drew
/// [`MAX_ROWS`] and there were more — so a reader who asked for 1000 is told why they see
/// 500. `None` otherwise: a smaller `limit` the reader chose needs no explaining.
pub fn cap_clause(shown: usize, total: usize) -> Option<String> {
    (shown >= MAX_ROWS && total > shown).then(|| format!("one page draws at most {MAX_ROWS} rows"))
}

/// The `limit` argument's summary, for the contract of every page that takes one — so the
/// two contracts cannot describe the rule two ways either.
pub fn limit_summary() -> String {
    format!(
        "How many rows to draw: a number, or `all` for as many as one page draws ({MAX_ROWS}); \
         a larger number is drawn at {MAX_ROWS} and the page says so. Default {ROWS}. The page \
         counts the whole filtered set either way and says what it drew."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_past_the_cap_is_drawn_at_the_cap() {
        assert_eq!(rows_wanted(None).unwrap(), ROWS);
        assert_eq!(rows_wanted(Some("all")).unwrap(), MAX_ROWS);
        assert_eq!(rows_wanted(Some("3")).unwrap(), 3);
        assert_eq!(rows_wanted(Some(&MAX_ROWS.to_string())).unwrap(), MAX_ROWS);
        assert_eq!(rows_wanted(Some("1000")).unwrap(), MAX_ROWS);
        assert_eq!(
            rows_wanted(Some(&usize::MAX.to_string())).unwrap(),
            MAX_ROWS
        );
        assert_eq!(
            rows_wanted(Some("99999999999999999999999")).unwrap(),
            MAX_ROWS
        );
    }

    #[test]
    fn a_value_that_is_not_a_positive_count_is_refused_naming_the_cap() {
        for bad in ["0", "-1", "none", "", "1e3", "3.5"] {
            match rows_wanted(Some(bad)) {
                Err(Error::InvalidArgument { name, detail }) => {
                    assert_eq!(name, "limit");
                    assert!(detail.contains(&MAX_ROWS.to_string()), "{detail}");
                }
                other => panic!("`{bad}` must be refused: {other:?}"),
            }
        }
    }

    #[test]
    fn more_is_offered_only_when_the_page_can_draw_more() {
        assert_eq!(more(50, 60), Some(60));
        assert_eq!(more(60, 60), None);
        // Past the cap the link asks for the cap, never for a count it cannot draw…
        assert_eq!(more(50, 700), Some(MAX_ROWS));
        // …and once the cap is shown there is no link at all.
        assert_eq!(more(MAX_ROWS, 700), None);
    }

    #[test]
    fn the_cap_is_named_only_when_it_is_what_stopped_the_page() {
        assert!(cap_clause(MAX_ROWS, 700)
            .unwrap()
            .contains(&MAX_ROWS.to_string()));
        assert_eq!(cap_clause(MAX_ROWS, MAX_ROWS), None);
        assert_eq!(cap_clause(50, 700), None);
    }
}
