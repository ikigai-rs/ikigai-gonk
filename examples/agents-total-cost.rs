//! `cargo run --release --example agents-total-cost [-- <rounds>]`: what each of the Agents tab's
//! totals costs the store (ledger #3178), at the profile the tab was measured at (ledger #2820): one
//! ledger of 6,000 items, ten agents with 300 hand-backs each and one with 3,000, every item claimed,
//! handed back, released and closed done, the hand-backs spread over the last two days.
//!
//! Each question is asked cold, after a write, so no cached answer is read: once across every holder
//! (`urn:iki:gonk:agent:question:name:{name}`), and once for the busiest holder, for comparison. Then
//! the whole Agents tab, cold and warm. Prints the median and the slowest of `rounds` asks.

use std::sync::Arc;
use std::time::Instant;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::agents::QUESTIONS;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::DurableStore;

const LEDGER: &str = "default";

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// One ask on a thread with the door's stack, as the door gives one.
fn ask(kernel: &Arc<Kernel>, iri: &str, args: &[(&str, &str)]) -> (f64, usize) {
    let (kernel, read) = (Arc::clone(kernel), request(Verb::Source, iri, args));
    std::thread::Builder::new()
        .stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
        .spawn(move || {
            let started = Instant::now();
            let answer = futures::executor::block_on(kernel.issue(read, &Capability::root()))
                .unwrap_or_else(|e| panic!("{e}"));
            (started.elapsed().as_secs_f64() * 1000.0, answer.bytes.len())
        })
        .unwrap()
        .join()
        .unwrap()
}

fn root_issue(hub: &Arc<Kernel>, verb: Verb, iri: &str, args: &[(&str, &str)]) {
    futures::executor::block_on(hub.issue(request(verb, iri, args), &Capability::root()))
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
}

fn median_and_max(mut times: Vec<f64>) -> (f64, f64) {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (times[(times.len() - 1) / 2], times[times.len() - 1])
}

fn main() {
    let rounds: usize = std::env::args()
        .nth(1)
        .map_or(5, |value| value.parse().expect("a number"));
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let passkeys = Arc::new(ikigai_gonk::identity::Passkeys::new(layout, 0));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec![LEDGER.to_string()],
        browse_roots: Vec::new().into(),
        passkeys,
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        page_assets: Default::default(),
        toolbox: false,
        faces: Vec::new(),
    });
    let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));

    let ledger = Ledger::parse(LEDGER).unwrap();
    let graph = ledger.graph();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let at = |ms_back: u64| {
        format!(
            "\"{}\"^^xsd:dateTime",
            ikigai_ledger::sparql::iso8601(now - ms_back)
        )
    };
    let holders: Vec<(String, usize)> = (0..10)
        .map(|n| (format!("agent-{n}"), 300))
        .chain(std::iter::once(("busy".to_string(), 3000)))
        .collect();
    let started = Instant::now();
    let mut number = 0;
    for (holder, count) in &holders {
        for batch in 0..count / 150 {
            let mut triples = String::new();
            for n in 0..150 {
                number += 1;
                let item = format!("urn:sample:cost:{number}");
                // Spread over the last two days, so a 24-hour window holds about half.
                let back = (batch * 150 + n) as u64 * (2 * 86_400_000 / *count as u64);
                triples.push_str(&format!(
                    "<{item}> a ledger:Item ; ledger:number {number} ; dcterms:title \"t\" ; \
                     ledger:status ledger:closed ; ledger:closedReason ledger:done ; \
                     ledger:closedAt {closed} .\n\
                     <{item}:claim> a ledger:ClaimEvent ; ledger:onItem <{item}> ; \
                     ledger:claimedBy \"{holder}\" ; dcterms:created {claimed} .\n\
                     <{item}:back> ledger:onItem <{item}> ; ledger:handedBackBy \"{holder}\" ; \
                     dcterms:created {closed} .\n\
                     <{item}:release> a ledger:ReleaseEvent ; ledger:onItem <{item}> ; \
                     ledger:claimedBy \"{holder}\" ; ledger:releaseReason \"released\" ; \
                     dcterms:created {closed} .\n",
                    closed = at(back),
                    claimed = at(back + 3_600_000),
                ));
            }
            let update = format!(
                "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>\n\
                 PREFIX dcterms: <http://purl.org/dc/terms/>\n\
                 PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n\
                 WITH <{graph}> INSERT {{ {triples} }} WHERE {{}}"
            );
            root_issue(
                &hub,
                Verb::Sink,
                "urn:iki:store:graph-update",
                &[("graph", graph.as_str()), ("content", &update)],
            );
        }
    }
    println!(
        "{number} items, {} agents: seeded in {:.1} s",
        holders.len(),
        started.elapsed().as_secs_f64()
    );

    let append = format!("{}append", ledger.prefix());
    let write = || root_issue(&hub, Verb::Sink, &append, &[("content", "a write")]);
    println!(
        "question                    all agents: median / max ms   busiest agent: median / max ms"
    );
    for question in &QUESTIONS {
        let mut all = Vec::new();
        let mut busy = Vec::new();
        for _ in 0..rounds {
            write();
            let iri = format!("urn:iki:gonk:agent:question:name:{}", question.name);
            all.push(ask(&http, &iri, &[("ledger", LEDGER)]).0);
            write();
            let iri = format!(
                "urn:iki:gonk:agent:holder:busy:question:name:{}",
                question.name
            );
            busy.push(ask(&http, &iri, &[("ledger", LEDGER)]).0);
        }
        let (all_median, all_max) = median_and_max(all);
        let (busy_median, busy_max) = median_and_max(busy);
        println!(
            "{:<27} {all_median:>8.1} / {all_max:>8.1}              {busy_median:>8.1} / {busy_max:>8.1}",
            question.name
        );
    }
    let page = "urn:iki:gonk:page:dashboard:agents";
    let (mut cold, mut warm) = (Vec::new(), Vec::new());
    for _ in 0..rounds {
        write();
        cold.push(ask(&http, page, &[]).0);
        warm.push(ask(&http, page, &[]).0);
    }
    let (cold_median, cold_max) = median_and_max(cold);
    let (warm_median, warm_max) = median_and_max(warm);
    println!("the Agents tab after a write: {cold_median:.0} ms median, {cold_max:.0} ms max");
    println!("the Agents tab between writes: {warm_median:.0} ms median, {warm_max:.0} ms max");
}
