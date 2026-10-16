//! A query only the CLOCK can stop, for the tests that assert a time-budget refusal through
//! gonk's doors (ledger [#1730](http://localhost:1062/l/default/item/1730), gonk's half of
//! ledger [#1685](http://localhost:1062/l/default/item/1685)'s sweep).
//!
//! ★ **A read is stopped by whichever bound fires FIRST.** These tests used a cross product of
//! `VALUES` tables in which every row paid for a `SHA512`, a million rows or more. Measured on
//! plasma (M-series, 2026-10-16) under `cargo test --release`, that was refused by the store's
//! 100,000-row bound after 155 ms, before the door's 300 to 600 ms budgets, and a signed-in
//! caller's read ANSWERED inside the store's 2.5 s base: seven tests failed in release and
//! passed in a debug build, which is a machine's speed, not a proof.
//!
//! [`rows`] and [`no_rows`] are built so that no other bound CAN fire: at most [`ROWS`] rows,
//! each one small, so the whole answer is under both size bounds by construction (the const
//! assertion below), and each row made expensive instead: a string of a million characters
//! grown from the row, of which only the length is kept. The store's own tests use the same
//! row (ikigai-rs/ikigai-store PR 65). Measured through gonk's `urn:sparql:select` on plasma
//! (2026-10-16): all 22,500 rows in 22.3 s in a release build (3.8 MB of answer; [`no_rows`]
//! the same 22.4 s), and 20 ms a row in a debug one, so a machine would have to be about nine
//! times faster than plasma's release build to finish inside the largest budget or base these
//! tests set (2.5 s). The serializer checks the cancellation token between rows, so a refused
//! read of [`rows`] stops within one row.
//!
//! Both are written with no whitespace, so the same text survives `/k`'s whitespace-separated
//! command grammar, and both begin `SELECT*` (a caller that wants a dataset inserts its `FROM`
//! after it).

use ikigai_store::budget::{DEFAULT_MAX_BYTES, DEFAULT_MAX_ROWS};

/// One side of the product: two `VALUES` tables of this many rows each.
const SIDE: u64 = 150;

/// The most rows [`rows`] can answer.
pub const ROWS: u64 = SIDE * SIDE;

/// An upper bound on one of [`rows`]'s rows serialized as a SPARQL JSON binding: two short
/// literals and an integer (`{"a":{"type":"literal","value":"149"},"b":…,"n":{"type":"literal",
/// "datatype":"http://www.w3.org/2001/XMLSchema#integer","value":"1000014"}},` is about 175
/// bytes).
const ROW_BYTES: u64 = 192;

// The query cannot reach a size bound, so a refusal of it is the clock's. The head and tail of
// an answer are a few dozen bytes; one row's worth is ample room for them.
const _: () = {
    assert!(ROWS < DEFAULT_MAX_ROWS);
    assert!((ROWS + 1) * ROW_BYTES < DEFAULT_MAX_BYTES);
};

/// One `VALUES` table of [`SIDE`] quoted numbers, with no whitespace.
fn table(var: &str) -> String {
    let values: String = (0..SIDE).map(|n| format!("\"{n}\"")).collect();
    format!("VALUES?{var}{{{values}}}")
}

/// The length of a million characters grown from `?a` and `?b`: ten `x`s and the two values,
/// every `x` replaced by ten, five times over. It depends on the row, so the planner cannot fold
/// it into one constant.
fn grown_length() -> String {
    let mut grown = r#"CONCAT(?a,?b,"xxxxxxxxxx")"#.to_string();
    for _ in 0..5 {
        grown = format!(r#"REPLACE({grown},"x","xxxxxxxxxx")"#);
    }
    format!("STRLEN({grown})")
}

/// Every row of the product, each with its expensive length `?n`: a streamed answer, which the
/// serializer stops at the budget.
pub fn rows() -> String {
    format!(
        "SELECT*WHERE{{{}{}BIND({}AS?n)}}",
        table("a"),
        table("b"),
        grown_length()
    )
}

/// The same work keeping NO row, for a reader whose own `LIMIT` would stop [`rows`] early (a
/// `LIMIT` bounds rows, not work). ⚠ With no row to serialize nothing checks the cancellation
/// token, so a refused read of this goes on evaluating after its caller is answered, up to
/// [`ROWS`] rows' worth (the servers here lift the store's overdue cap for that reason); it ends
/// with the test binary.
// Each test binary compiles this module whole; only `tests/anonymous_runs_1210.rs` sends a
// query that must keep no row.
#[allow(dead_code)]
pub fn no_rows() -> String {
    format!(
        "SELECT*WHERE{{{}{}FILTER({}<0)}}",
        table("a"),
        table("b"),
        grown_length()
    )
}
