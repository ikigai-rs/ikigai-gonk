//! `cargo run --release --example poll-cost [-- <renders>]` — what one answer to an open page's
//! poll costs gonk, by the render that dominates it (ledger #1705).
//!
//! The Layers page's poll became a SHORT poll (every 3 seconds per open Layers page) in place of
//! a long poll that held a connection for up to 45 seconds. Every answer, even one that says
//! nothing new, is an envelope rendered through `web/gonk.xsl` by xrust, which parses the whole
//! stylesheet each time; the copy it reads is in memory. So the question this answers is what
//! that render costs per answer: the quiet answer (the next watcher only), an answer with the
//! banner, and the header's count for comparison (it has polled every 10 s since ledger #1700).
//!
//! Prints min / median / max milliseconds per render. Divide by the cadence for the CPU share
//! one open page costs.

use std::time::Instant;

use ikigai_gonk::render::{self, element, envelope};

fn measure(label: &str, document: &str, renders: usize) {
    let mut took: Vec<f64> = (0..renders)
        .map(|_| {
            let started = Instant::now();
            let html = render::render(document, false).expect("the fragment renders");
            assert!(!html.is_empty());
            started.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    took.sort_by(f64::total_cmp);
    println!(
        "{label:<28} min {:>7.2} ms   median {:>7.2} ms   max {:>7.2} ms",
        took[0],
        took[took.len() / 2],
        took[took.len() - 1]
    );
}

fn main() {
    let renders: usize = std::env::args()
        .nth(1)
        .map(|n| n.parse().expect("a number of renders"))
        .unwrap_or(50);
    let watch = element(
        "watch",
        &[
            ("url", "/layers/~/wait?layer=talk&after=12&door=ok"),
            ("trigger", "load delay:3s"),
        ],
        "",
    );
    measure(
        "quiet wait (watcher only)",
        &envelope("page", &[("view", "layers-wait")], &watch),
        renders,
    );
    let banner = element(
        "notice",
        &[("kind", "error")],
        "The layer door at quic://example:4434 cannot be reached: connection refused.",
    );
    measure(
        "wait with the banner",
        &envelope(
            "page",
            &[("view", "layers-wait"), ("banner", "true")],
            &format!("{watch}{banner}"),
        ),
        renders,
    );
    measure(
        "header count",
        &envelope(
            "page",
            &[
                ("view", "layers-count"),
                ("count", "3"),
                ("url", "/layers/~/count?seen=3"),
                ("every", "10s"),
            ],
            "",
        ),
        renders,
    );
}
