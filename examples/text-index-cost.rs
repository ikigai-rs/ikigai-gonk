//! `cargo run --release --example text-index-cost -- [--items N]` — what a ledger search costs
//! with and without the store's word index (ledger #4130), on a scratch store.
//!
//! Everything happens in a fresh temporary directory: a RocksDB store opened the way `main`
//! opens one (`open_shared_declaring` with browse's promise, so the ledger graphs are covered
//! and the index's answers are trusted), composed with `compose_serving_with_lifecycles`.
//! Nothing opens `~/.ikigai/store`, and no live gonk is involved.
//!
//! 1. **Seed**: one item is filed through the ledger, and its quads are copied N times (default
//!    10,000) with a fresh id, number, title and body each, written straight into the ledger's
//!    graph. Words come from a fixed Zipf-like vocabulary, so `staff` is common and
//!    `healthiness` rare.
//! 2. **Before**: the store reopened WITHOUT the index; each search timed.
//! 3. **After**: the store reopened WITH the index (gonk's enrollment). Its first build is timed
//!    until `state: ready`, its size on disk is printed, then each search is timed again, and
//!    `sort=relevance` too.
//!
//! Each search runs 7 times with a different `limit`, so the listing itself is never a cache hit;
//! the median is printed, and the first run beside it, since the ledger's inner reads (the index
//! query, the candidate check) can be cache hits on the runs after the first.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_store::DurableStore;
use oxigraph::model::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};

const GRAPH: &str = "urn:iki:ledger:graph:default";

fn issue(kernel: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Result<String, String> {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("an IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    block_on(Kernel::issue(kernel, request, &Capability::root()))
        .map(|answer| String::from_utf8_lossy(&answer.bytes).into_owned())
        .map_err(|e| e.to_string())
}

fn compose(text_index: bool, dir: &std::path::Path) -> (Kernel, Arc<oxigraph::store::Store>) {
    let graph = ikigai_gonk::browse::Graph::chosen();
    let (store, handle) =
        DurableStore::open_shared_declaring(dir, graph.sharer_writes()).expect("a scratch store");
    let store = if text_index {
        store
            .with_text_index(ikigai_gonk::text_index::enrollment())
            .expect("the index")
    } else {
        store
    };
    let kernel = ikigai_gonk::compose_serving_with_lifecycles(
        store,
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        None,
        Default::default(),
    )
    .0;
    (kernel, handle)
}

/// A tiny deterministic generator, so every run seeds the same ledger.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A Zipf-like draw over `vocab`: rank r is picked with weight about 1/r.
fn word<'a>(rng: &mut Rng, vocab: &'a [String]) -> &'a str {
    let n = vocab.len() as f64;
    let u = (rng.below(1_000_000) as f64 + 1.0) / 1_000_001.0;
    let rank = (n.powf(u) as usize).saturating_sub(1).min(vocab.len() - 1);
    &vocab[rank]
}

fn words(rng: &mut Rng, vocab: &[String], low: u64, high: u64) -> String {
    let count = low + rng.below(high - low + 1);
    (0..count)
        .map(|_| word(rng, vocab))
        .collect::<Vec<_>>()
        .join(" ")
}

fn vocabulary() -> Vec<String> {
    let mut vocab: Vec<String> = [
        "the", "of", "and", "to", "a", "in", "is", "it", "for", "on", "lease", "parser", "staff",
        "queue", "review", "panic", "store", "index", "page",
    ]
    .iter()
    .map(|w| w.to_string())
    .collect();
    // Then 5,000 made-up words, and one rare real one deep in the tail.
    let syllables = ["ka", "lo", "mi", "ren", "tu", "sa", "vo", "de", "ni", "pra"];
    for i in 0..5_000usize {
        let mut w = String::new();
        let mut x = i + 11;
        while x > 0 {
            w.push_str(syllables[x % syllables.len()]);
            x /= syllables.len();
        }
        vocab.push(w);
        if i == 1_500 {
            vocab.push("healthiness".to_string());
        }
    }
    vocab
}

fn seed(dir: &std::path::Path, items: usize) {
    let (kernel, handle) = compose(false, dir);
    issue(
        &kernel,
        Verb::Sink,
        "urn:iki:ledger:default:append",
        &[("content", "TEMPLATETITLE\n\nTEMPLATEBODY")],
    )
    .expect("one item");
    let graph = GraphName::NamedNode(NamedNode::new(GRAPH).unwrap());
    let template: Vec<Quad> = handle
        .quads_for_pattern(None, None, None, Some(graph.as_ref()))
        .collect::<Result<_, _>>()
        .unwrap();
    let id = template
        .iter()
        .find_map(|q| {
            let s = match &q.subject {
                NamedOrBlankNode::NamedNode(n) => n.as_str(),
                _ => return None,
            };
            s.strip_prefix("urn:iki:ledger:default:item:")
                .map(|rest| rest.split(':').next().unwrap().to_string())
        })
        .expect("the template item's id");
    let vocab = vocabulary();
    let mut rng = Rng(0x4130_4130_4130_4130);
    let mut text_bytes = 0usize;
    let mut quads = Vec::new();
    for n in 0..items {
        let new_id = format!("{id}x{n:05}");
        let title = words(&mut rng, &vocab, 4, 11);
        let body = words(&mut rng, &vocab, 40, 399);
        text_bytes += title.len() + body.len();
        let iri = |s: &str| s.replace(&id, &new_id);
        for q in &template {
            let subject = match &q.subject {
                NamedOrBlankNode::NamedNode(s) => {
                    NamedOrBlankNode::NamedNode(NamedNode::new(iri(s.as_str())).unwrap())
                }
                other => other.clone(),
            };
            let object = match &q.object {
                Term::NamedNode(o) => Term::NamedNode(NamedNode::new(iri(o.as_str())).unwrap()),
                Term::Literal(l) if l.value() == "TEMPLATETITLE" => {
                    Term::Literal(Literal::new_simple_literal(&title))
                }
                Term::Literal(l) if l.value() == "TEMPLATEBODY" => {
                    Term::Literal(Literal::new_simple_literal(&body))
                }
                Term::Literal(l) if q.predicate.as_str().ends_with("#number") => Term::Literal(
                    Literal::new_typed_literal((n + 2).to_string(), l.datatype().into_owned()),
                ),
                other => other.clone(),
            };
            quads.push(Quad::new(
                subject,
                q.predicate.clone(),
                object,
                graph.clone(),
            ));
        }
    }
    handle.extend(quads).unwrap();
    println!(
        "seeded {items} items ({:.1} MiB of title and body text) into {}",
        text_bytes as f64 / 1048576.0,
        dir.display()
    );
}

fn median(kernel: &Kernel, args: &[(&str, &str)]) -> (Duration, String) {
    let mut cold = None;
    let mut times = Vec::new();
    let mut first_line = String::new();
    for run in 0..7 {
        let limit = (50 + run).to_string();
        let mut all: Vec<(&str, &str)> = args.to_vec();
        all.push(("limit", &limit));
        let start = Instant::now();
        let answer = issue(kernel, Verb::Source, "urn:iki:ledger:default:items", &all);
        times.push(start.elapsed());
        cold.get_or_insert(start.elapsed());
        first_line = match answer {
            Ok(text) => text
                .lines()
                .rfind(|l| !l.trim().is_empty())
                .unwrap_or("")
                .to_string(),
            Err(e) => format!("refused: {e}"),
        };
    }
    times.sort();
    let cold = cold.expect("one run");
    (
        times[times.len() / 2],
        format!(
            "(first run {:.1} ms)  {first_line}",
            cold.as_secs_f64() * 1000.0
        ),
    )
}

const SEARCHES: [(&str, &[(&str, &str)]); 5] = [
    ("text=staff (titles)", &[("text", "staff")]),
    ("text=healthiness (titles)", &[("text", "healthiness")]),
    (
        "search=healthiness whole_word match=title,body",
        &[
            ("search", "healthiness"),
            ("whole_word", "true"),
            ("match", "title,body"),
        ],
    ),
    (
        "phrase=\"lease parser\" (titles)",
        &[("phrase", "lease parser")],
    ),
    (
        "phrase=\"lease parser\" match=title,body",
        &[("phrase", "lease parser"), ("match", "title,body")],
    ),
];

fn main() {
    let mut args = std::env::args().skip(1);
    let mut items = 10_000usize;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--items" => items = args.next().expect("--items N").parse().expect("a count"),
            other => panic!("unknown flag {other}"),
        }
    }
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let dir = scratch.path().join("store");
    seed(&dir, items);

    println!("\nBEFORE: no text index (gonk.store.text_index = false)");
    {
        let (kernel, _handle) = compose(false, &dir);
        for (name, search) in SEARCHES {
            let (t, last) = median(&kernel, search);
            println!("  {name:52} {:>9.1} ms   {last}", t.as_secs_f64() * 1000.0);
        }
        let mut ranked = SEARCHES[0].1.to_vec();
        ranked.push(("sort", "relevance"));
        let (t, last) = median(&kernel, &ranked);
        println!(
            "  {:52} {:>9.1} ms   {last}",
            "text=staff sort=relevance",
            t.as_secs_f64() * 1000.0
        );
    }

    println!("\nAFTER: the store's text index, gonk's enrollment");
    let start = Instant::now();
    let (kernel, _handle) = compose(true, &dir);
    let status_iri = format!(
        "urn:iki:store:text-index:graph:{}",
        ikigai_store::graph_thread(GRAPH)
            .strip_prefix("urn:iki:store:graph-written:")
            .unwrap()
    );
    let status = loop {
        let status = issue(&kernel, Verb::Source, &status_iri, &[]).unwrap_or_default();
        if status.lines().any(|l| l == "state: ready") || start.elapsed() > Duration::from_secs(600)
        {
            break status;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    println!(
        "  first build, open to `state: ready`: {:.1} s",
        start.elapsed().as_secs_f64()
    );
    for line in status
        .lines()
        .filter(|l| l.starts_with("documents") || l.starts_with("state"))
    {
        println!("  {line}");
    }
    let index_dir = ikigai_gonk::text_index::dir(&dir);
    println!(
        "  on disk: {} ({})",
        ikigai_gonk::text_index::size_words(ikigai_gonk::text_index::bytes_on_disk(&index_dir)),
        index_dir.display()
    );
    println!(
        "  banner: {}",
        ikigai_gonk::text_index::banner_line(true, &dir).trim()
    );
    for (name, search) in SEARCHES {
        let (t, last) = median(&kernel, search);
        println!("  {name:52} {:>9.1} ms   {last}", t.as_secs_f64() * 1000.0);
    }
    for (name, search) in SEARCHES {
        let mut ranked = search.to_vec();
        ranked.push(("sort", "relevance"));
        let (t, last) = median(&kernel, &ranked);
        println!(
            "  {:52} {:>9.1} ms   {last}",
            format!("{name} sort=relevance"),
            t.as_secs_f64() * 1000.0
        );
    }
}
