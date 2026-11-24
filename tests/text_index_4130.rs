//! Ledger [#4130](http://localhost:1062/l/default/item/4130): the store indexes each ledger's own
//! graph (`dcterms:title`, `ledger:body`) for search, and the ledger page offers "most relevant
//! first" when that index answers, says plainly when it cannot, and takes `phrase=`.
//!
//! `cargo test --release --test text_index_4130 -- --ignored --nocapture` measures a search
//! over a generated 10,000-item ledger, with and without the index, on a scratch store.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::{compose_serving_with_lifecycles, doors, web};
use ikigai_store::DurableStore;

struct Gonk {
    runtime: tokio::runtime::Runtime,
    hub: Arc<Kernel>,
    http: Arc<Kernel>,
}

impl Gonk {
    fn start(store: DurableStore) -> Gonk {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .build()
            .unwrap();
        let hub = Arc::new(
            compose_serving_with_lifecycles(
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
            .0,
        );
        let config = tempfile::tempdir().unwrap();
        let layout = ikigai_gonk::quic::Layout::in_config_home(config.path());
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
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

    fn indexed() -> Gonk {
        Gonk::start(
            DurableStore::in_memory()
                .unwrap()
                .with_text_index(ikigai_gonk::ledger_text_enrollment())
                .unwrap(),
        )
    }

    fn try_issue(
        &self,
        kernel: &Arc<Kernel>,
        verb: Verb,
        iri: &str,
        args: &[(&str, &str)],
    ) -> Result<Vec<u8>, String> {
        let request = args.iter().fold(
            Request::new(verb, Iri::parse(iri).expect("a test IRI")),
            |request, (name, value)| {
                request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()))
            },
        );
        self.runtime
            .block_on(Kernel::issue(kernel, request, &Capability::root()))
            .map(|r| r.bytes)
            .map_err(|e| e.to_string())
    }

    fn issue(&self, kernel: &Arc<Kernel>, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
        String::from_utf8(
            self.try_issue(kernel, verb, iri, args)
                .unwrap_or_else(|e| panic!("{iri} {args:?}: {e}")),
        )
        .unwrap()
    }

    fn append(&self, ledger: &str, content: &str) {
        self.issue(
            &self.hub,
            Verb::Sink,
            &format!("urn:iki:ledger:{ledger}:append"),
            &[("content", content)],
        );
    }

    fn page(&self, args: &[(&str, &str)]) -> String {
        self.issue(
            &self.http,
            Verb::Source,
            "urn:iki:gonk:page:ledger:default",
            args,
        )
    }

    fn status(&self, graph: &str) -> String {
        let thread = ikigai_store::graph_thread(graph);
        let encoded = thread.strip_prefix("urn:iki:store:graph-written:").unwrap();
        self.issue(
            &self.hub,
            Verb::Source,
            &format!("urn:iki:store:text-index:graph:{encoded}"),
            &[],
        )
    }

    fn wait_ready(&self) {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let status = self.status("urn:iki:ledger:graph:default");
            if status.lines().any(|l| l == "state: ready") {
                return;
            }
            assert!(Instant::now() < until, "{status}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The item numbers, in page order, from the rendered rows.
    fn numbers(&self, args: &[(&str, &str)]) -> Vec<u32> {
        let json = self.issue(
            &self.hub,
            Verb::Source,
            "urn:iki:ledger:default:items",
            &[args, &[("as", "application/json")]].concat(),
        );
        let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rows = doc
            .as_array()
            .cloned()
            .or_else(|| doc["items"].as_array().cloned())
            .expect("rows");
        rows.iter()
            .map(|row| row["number"].as_u64().expect("a number") as u32)
            .collect()
    }
}

#[test]
fn the_enrollment_is_every_ledger_graph_and_never_a_graveyard_or_session() {
    let gonk = Gonk::indexed();
    for graph in ["urn:iki:ledger:graph:default", "urn:iki:ledger:graph:crew"] {
        assert!(!gonk.status(graph).contains("not enrolled"), "{graph}");
    }
    for graph in [
        "urn:iki:ledger:graph:default:deleted",
        "urn:iki:ledger:graph:default:session",
    ] {
        assert!(
            gonk.status(graph).contains("state: not enrolled"),
            "{graph}"
        );
    }
}

#[test]
fn the_page_offers_most_relevant_first_when_the_index_answers() {
    let gonk = Gonk::indexed();
    gonk.wait_ready();
    gonk.append("default", "A note on parsers\n\nnothing else here");
    gonk.append(
        "default",
        "Parser parser parser\n\nthe parser rewrite for the parser",
    );
    gonk.append("default", "Unrelated\n\nmentions a parser once");
    let page = gonk.page(&[("text", "parser")]);
    assert!(page.contains("List the most relevant first"), "{page}");
    assert!(page.contains("sort=relevance"), "{page}");
    assert!(page.contains("enter relevance by itself") || page.contains("Or enter relevance"));
    assert!(!page.contains("id='relevance-note'"), "{page}");
    // No search, no offer: relevance needs words to rank by.
    assert!(!gonk.page(&[]).contains("List the most relevant first"));

    let ranked = gonk.page(&[("text", "parser"), ("sort", "relevance")]);
    assert!(ranked.contains("most relevant first."), "{ranked}");
    assert!(!ranked.contains("List the most relevant first"), "{ranked}");
    // The order is the ledger's: the item that says "parser" most, in its title, first.
    assert_eq!(
        gonk.numbers(&[("search", "parser"), ("sort", "relevance")])
            .first(),
        Some(&2)
    );
}

#[test]
fn the_page_says_plainly_when_there_is_no_index() {
    let gonk = Gonk::start(DurableStore::in_memory().unwrap());
    gonk.append("default", "A note on parsers");
    let page = gonk.page(&[("text", "parser")]);
    assert!(!page.contains("List the most relevant first"), "{page}");
    assert!(
        page.contains("Most relevant first is unavailable: this server keeps no text index."),
        "{page}"
    );
    assert!(!page.contains("Or enter relevance"), "{page}");
    // And the ledger refuses the order itself, in words.
    let refused = gonk
        .try_issue(
            &gonk.http,
            Verb::Source,
            "urn:iki:gonk:page:ledger:default",
            &[("text", "parser"), ("sort", "relevance")],
        )
        .map(String::from_utf8);
    match refused {
        Err(e) => assert!(e.contains("text index"), "{e}"),
        Ok(Ok(page)) => assert!(page.contains("text index"), "{page}"),
        Ok(Err(e)) => panic!("{e}"),
    }
}

#[test]
fn phrase_is_reachable_from_the_search_form_and_kept_by_its_links() {
    for gonk in [
        Gonk::indexed(),
        Gonk::start(DurableStore::in_memory().unwrap()),
    ] {
        gonk.append("default", "Lease ran out\n\nthe lease ran out");
        gonk.append("default", "Out ran the lease");
        let form = gonk.page(&[]);
        assert!(form.contains("<label for='search-phrase'>"), "{form}");
        assert!(form.contains("name='phrase'"), "{form}");
        let page = gonk.page(&[("phrase", "lease ran out"), ("status", "all")]);
        assert!(page.contains("value='lease ran out'"), "{page}");
        assert!(page.contains("Lease ran out"), "{page}");
        assert!(!page.contains("Out ran the lease"), "{page}");
    }
}

/// Before and after, on a scratch store: a generated 10,000-item ledger searched by scan
/// (no index) and through the index, then ranked by relevance.
#[test]
#[ignore = "a measurement: cargo test --release --test text_index_4130 -- --ignored --nocapture"]
fn measure_a_search_over_ten_thousand_items() {
    const ITEMS: usize = 10_000;
    let words = [
        "lease", "parser", "socket", "ledger", "review", "index", "graph", "render", "quic",
        "backup",
    ];
    let mut ttl = String::from(
        "@prefix l: <https://ikigai-rs.dev/ns/ledger#> .\n@prefix d: <http://purl.org/dc/terms/> .\n",
    );
    for n in 0..ITEMS {
        let (a, b, c) = (words[n % 10], words[(n / 10) % 10], words[(n / 100) % 10]);
        ttl.push_str(&format!(
            "<urn:m4130:{n}> a l:Item; l:number {}; d:title \"{a} {b} item {n}\"; \
             l:body \"Body of {n}: the {c} and the {b} meet the {a}, item {n} of the scratch ledger.\"; \
             l:status l:open .\n",
            n + 1
        ));
    }
    let scratch = tempfile::tempdir().unwrap();
    for (label, indexed) in [("before (scan)", false), ("after (index)", true)] {
        let path = scratch.path().join(label.replace([' ', '(', ')'], ""));
        let store = DurableStore::open(&path).unwrap();
        let store = if indexed {
            store
                .with_text_index(ikigai_gonk::ledger_text_enrollment())
                .unwrap()
        } else {
            store
        };
        let gonk = Gonk::start(store);
        gonk.issue(
            &gonk.hub,
            Verb::Sink,
            "urn:iki:store:load",
            &[
                ("content", &ttl),
                ("format", "text/turtle"),
                ("graph", "urn:iki:ledger:graph:default"),
            ],
        );
        if indexed {
            gonk.wait_ready();
            // One ranked read commits what the load left pending.
            let _ = gonk.numbers(&[("search", "warmup"), ("sort", "relevance")]);
        }
        let mut cases: Vec<Vec<(&str, &str)>> = vec![
            vec![("text", "parser"), ("limit", "50")],
            vec![
                ("search", "socket review"),
                ("whole_word", "true"),
                ("limit", "50"),
            ],
        ];
        if indexed {
            cases.push(vec![
                ("search", "socket review"),
                ("sort", "relevance"),
                ("limit", "50"),
            ]);
        }
        for args in cases {
            let _ = gonk.numbers(&args);
            let runs = 5;
            let start = Instant::now();
            for _ in 0..runs {
                // A fresh limit per run so the kernel's cache does not answer.
                let _ = gonk.try_issue(
                    &gonk.hub,
                    Verb::Source,
                    "urn:iki:ledger:default:count",
                    &args,
                );
                let _ = gonk.numbers(&args);
            }
            println!(
                "{label}: {args:?}: {:?} per search (items + count, mean of {runs})",
                start.elapsed() / runs
            );
        }
        if indexed {
            let status = gonk.status("urn:iki:ledger:graph:default");
            println!(
                "{label}: index {} on disk: {} bytes",
                status
                    .lines()
                    .find(|l| l.starts_with("documents:"))
                    .unwrap_or(""),
                ikigai_gonk::text_index_bytes(&path)
            );
        }
    }
}
