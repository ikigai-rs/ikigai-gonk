//! Ledger [#4140](http://localhost:1062/l/default/item/4140): `/l/{ledger}/facet` ran one SELECT
//! per ledger of a union, one after another, and each crossed every item's labels with its
//! priorities and a two-row `VALUES ?kind` before `COUNT(DISTINCT)`, binding `?number` and
//! `?title` even with no text filter.
//!
//! [`the_facet_counts_what_the_items_say`] holds the answers: the counts the JSON face gives,
//! over a matrix of filters and a union, against the same counts computed here from the
//! seeded items. It passes before and after the rewrite, so the rewrite changed no answer.
//!
//! `cargo test --release --test facet_4140 -- --ignored --nocapture` prints the time per read.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::web::{self, FacetDocument};
use ikigai_gonk::{compose_serving_with_lifecycles, doors};
use ikigai_store::DurableStore;

const LEDGERS: [&str; 4] = ["default", "crew", "ops", "web"];

/// One seeded item, as the counts here read it.
#[derive(Clone)]
struct Item {
    title: String,
    open: bool,
    labels: Vec<String>,
    priorities: Vec<u8>,
}

struct Gonk {
    runtime: tokio::runtime::Runtime,
    hub: Arc<Kernel>,
    http: Arc<Kernel>,
}

impl Gonk {
    fn start() -> Gonk {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .build()
            .unwrap();
        let hub = Arc::new(
            compose_serving_with_lifecycles(
                DurableStore::in_memory().unwrap(),
                None,
                Vec::new(),
                Vec::new(),
                None,
                None,
                None,
                None,
                Default::default(),
            )
            .0,
        );
        let config = tempfile::tempdir().unwrap();
        let layout = ikigai_gonk::quic::Layout::in_config_home(config.path());
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: LEDGERS.iter().map(|name| name.to_string()).collect(),
            browse_roots: Vec::new().into(),
            passkeys: Arc::new(ikigai_gonk::identity::Passkeys::new(layout, 0)),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            holder_aliases: Default::default(),
            display: Default::default(),
            page_assets: Default::default(),
            projects: Default::default(),
            toolbox: false,
            faces: Vec::new(),
            inquiry: None,
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        Gonk { runtime, hub, http }
    }

    fn issue(&self, kernel: &Arc<Kernel>, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Vec<u8> {
        let request = args.iter().fold(
            Request::new(verb, Iri::parse(iri).expect("a test IRI")),
            |request, (name, value)| {
                request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()))
            },
        );
        self.runtime
            .block_on(Kernel::issue(kernel, request, &Capability::root()))
            .unwrap_or_else(|e| panic!("{iri} {args:?}: {e}"))
            .bytes
    }

    fn load(&self, ledger: &str, items: &[Item], from: usize) {
        let mut ttl = String::from(
            "@prefix l: <https://ikigai-rs.dev/ns/ledger#> .\n\
             @prefix d: <http://purl.org/dc/terms/> .\n",
        );
        for (n, item) in items.iter().enumerate() {
            let n = from + n;
            ttl.push_str(&format!(
                "<urn:facet4140:{ledger}:{n}> a l:Item; l:number {}; d:title \"{}\"; l:status l:{}",
                n + 1,
                item.title,
                if item.open { "open" } else { "closed" },
            ));
            for label in &item.labels {
                ttl.push_str(&format!("; l:label \"{label}\""));
            }
            for p in &item.priorities {
                ttl.push_str(&format!("; l:priority {p}"));
            }
            ttl.push_str(" .\n");
        }
        self.issue(
            &self.hub,
            Verb::Sink,
            "urn:iki:store:load",
            &[
                ("content", &ttl),
                ("format", "text/turtle"),
                ("graph", &format!("urn:iki:ledger:graph:{ledger}")),
            ],
        );
    }

    fn facet(&self, args: &[(&str, &str)]) -> FacetDocument {
        let mut args = args.to_vec();
        args.push(("as", "application/json"));
        let bytes = self.issue(
            &self.http,
            Verb::Source,
            "urn:iki:gonk:fragment:facet:ledger:default",
            &args,
        );
        serde_json::from_slice(&bytes).expect("the facet's JSON face")
    }
}

/// A deterministic spread of items: labels from a pool, any number of them, no priority, one,
/// or (rarely) two, open and closed, titles that a text filter can pick out.
fn seed(ledger: usize, count: usize) -> Vec<Item> {
    let mut state = 0x9e37_79b9_u64.wrapping_mul(ledger as u64 + 1);
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..count)
        .map(|n| {
            let r = next();
            let labels = (0..r % 4)
                .map(|k| format!("label-{}", (next() + k) % 40))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            let priorities = match next() % 10 {
                0..=3 => Vec::new(),
                9 => vec![(next() % 5) as u8, (next() % 5) as u8],
                _ => vec![(next() % 5) as u8],
            };
            let word = ["Alpha", "beta", "GAMMA", "delta"][(next() % 4) as usize];
            Item {
                title: format!("{word} item {n} of {ledger}"),
                open: next() % 3 != 0,
                labels,
                priorities,
            }
        })
        .collect()
}

/// What the facet should say for `filter` over `items`, computed from the items themselves.
fn expected(items: &[&Item], filter: &[(&str, &str)]) -> FacetDocument {
    let arg = |name: &str| filter.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
    let terms = |name: &str| -> Vec<String> {
        arg(name)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect()
    };
    let (required, excluded) = (terms("labels"), terms("without"));
    let status = arg("status").unwrap_or("open");
    let mut labels: BTreeMap<String, u64> = BTreeMap::new();
    let mut priorities: BTreeMap<Option<u8>, u64> = BTreeMap::new();
    for item in items {
        let status_ok = match status {
            "open" => item.open,
            "closed" => !item.open,
            _ => true,
        };
        let priority_ok = arg("priority").is_none_or(|wanted| {
            wanted.split(',').map(str::trim).any(|w| match w {
                "none" => item.priorities.is_empty(),
                n => item.priorities.contains(&n.parse().unwrap()),
            })
        });
        let text_ok =
            arg("text").is_none_or(|t| item.title.to_lowercase().contains(&t.to_lowercase()));
        if !(status_ok
            && priority_ok
            && text_ok
            && required.iter().all(|l| item.labels.contains(l))
            && !excluded.iter().any(|l| item.labels.contains(l)))
        {
            continue;
        }
        for label in &item.labels {
            *labels.entry(label.clone()).or_default() += 1;
        }
        let mut distinct = item.priorities.clone();
        distinct.sort();
        distinct.dedup();
        if distinct.is_empty() {
            *priorities.entry(None).or_default() += 1;
        }
        for p in distinct {
            *priorities.entry(Some(p)).or_default() += 1;
        }
    }
    // The facet orders terms as strings, and `unset` after the digits.
    let mut priorities: Vec<_> = priorities.into_iter().collect();
    priorities.sort_by_key(|(p, _)| p.map_or("unset".to_string(), |p| p.to_string()));
    FacetDocument {
        schema: ikigai_gonk::web::FACET_SCHEMA,
        ledger: "default".into(),
        ledgers: None,
        labels: labels
            .into_iter()
            .map(|(label, count)| ikigai_gonk::web::LabelCount { label, count })
            .collect(),
        priorities: priorities
            .into_iter()
            .map(|(priority, count)| ikigai_gonk::web::PriorityCount { priority, count })
            .collect(),
    }
}

const FILTERS: &[&[(&str, &str)]] = &[
    &[],
    &[("status", "all")],
    &[("status", "closed")],
    &[("labels", "label-3")],
    &[("labels", "label-3,label-7"), ("status", "all")],
    &[("without", "label-1,label-2")],
    &[("priority", "none")],
    &[("priority", "0,4")],
    &[("priority", "2,none"), ("labels", "label-5")],
    &[("text", "gamma")],
    &[
        ("text", "item 1"),
        ("without", "label-0"),
        ("status", "all"),
    ],
    &[("labels", "label-9"), ("without", "label-9")],
    &[("text", "no such title")],
];

#[test]
fn the_facet_counts_what_the_items_say() {
    let gonk = Gonk::start();
    let seeded: Vec<Vec<Item>> = (0..LEDGERS.len()).map(|n| seed(n, 150)).collect();
    for (ledger, items) in LEDGERS.iter().zip(&seeded) {
        gonk.load(ledger, items, 0);
    }
    for filter in FILTERS {
        let one = gonk.facet(filter);
        assert_eq!(
            one,
            expected(&seeded[0].iter().collect::<Vec<_>>(), filter),
            "{filter:?}"
        );

        let mut union = filter.to_vec();
        union.push(("ledgers", "default,crew,ops,web"));
        let all = gonk.facet(&union);
        let mut want = expected(&seeded.iter().flatten().collect::<Vec<_>>(), filter);
        want.ledgers = Some({
            let mut names: Vec<String> = LEDGERS.iter().map(|s| s.to_string()).collect();
            names.sort();
            names
        });
        assert_eq!(all, want, "{union:?}");
    }
}

fn timed(gonk: &Gonk, rounds: usize, filter: &[(&str, &str)], items: usize) -> Vec<Duration> {
    let mut times = Vec::new();
    for round in 0..rounds {
        // A write to every ledger between reads, as on a live server: it cuts the facet's
        // cached answer, so each read below counts again.
        for (n, ledger) in LEDGERS.iter().enumerate() {
            gonk.load(ledger, &seed(100 + n, 1), items + round);
        }
        let start = Instant::now();
        let doc = gonk.facet(filter);
        times.push(start.elapsed());
        assert!(!doc.labels.is_empty());
    }
    times.sort();
    times
}

/// The time per read, after a write, over a four-ledger union and over one ledger.
#[test]
#[ignore = "a measurement: run with --release --ignored --nocapture"]
fn time_per_read() {
    let gonk = Gonk::start();
    let items = 5_000;
    for (n, ledger) in LEDGERS.iter().enumerate() {
        gonk.load(ledger, &seed(n, items), 0);
    }
    let union = [("ledgers", "default,crew,ops,web")];
    for (name, filter) in [
        ("one ledger, no filter", &[][..]),
        ("one ledger, text=gamma", &[("text", "gamma")][..]),
        ("4-ledger union, no filter", &union[..]),
        (
            "4-ledger union, labels=label-3",
            &[("ledgers", "default,crew,ops,web"), ("labels", "label-3")][..],
        ),
    ] {
        let times = timed(&gonk, 15, filter, items);
        eprintln!(
            "{name}: median {:?}, min {:?}, max {:?}",
            times[times.len() / 2],
            times[0],
            times[times.len() - 1]
        );
    }
}

/// The facet's per-ledger reads go to the hub from tasks of their own, so they are not
/// recorded as its dependencies. That is safe only while no kernel keeps the facet's
/// answer: a write would not cut a kept one. Neither face may be cached.
#[test]
fn the_facet_is_never_kept() {
    let gonk = Gonk::start();
    gonk.load("default", &seed(0, 20), 0);
    for face in ["application/json", "text/html"] {
        let read = || {
            Request::new(
                Verb::Source,
                Iri::parse("urn:iki:gonk:fragment:facet:ledger:default").unwrap(),
            )
            .with_arg("as", ArgRef::Inline(face.as_bytes().to_vec()))
            .with_arg("ledgers", ArgRef::Inline(b"default,crew".to_vec()))
        };
        gonk.runtime
            .block_on(Kernel::issue(&gonk.http, read(), &Capability::root()))
            .unwrap();
        assert!(!gonk.http.is_cached(&read(), &Capability::root()), "{face}");
    }
}
