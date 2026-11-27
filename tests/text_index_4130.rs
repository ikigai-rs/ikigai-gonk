//! Ledger #4130: the ledgers' word index, as the ledger page offers it.
//!
//! - With the index enrolled (what `main` does unless `gonk.store.text_index = false`), a
//!   search offers "most relevant first", and following it lists the best match first and says
//!   the order in words.
//! - Without it, the same search says plainly that the order is not available, and asking for
//!   it anyway lists nothing and says why, never "No items match".
//! - `phrase=` is reachable from the form and filters the list and the facet counts alike.
//! - The graveyard and session graphs are never enrolled.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{doors, quic, web};
use ikigai_store::DurableStore;

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    store: DurableStore,
    _config: tempfile::TempDir,
}

/// The ledger's own graph, the one enrolled.
const GRAPH: &str = "urn:iki:ledger:graph:default";

/// How long the index may take to settle after the seed before the test fails. The store
/// commits at most every second; this is a ceiling for a loaded CI runner, not a wait.
const SETTLE_CEILING: Duration = Duration::from_secs(60);

impl Server {
    fn start(text_index: bool) -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let store = DurableStore::in_memory().unwrap();
        let store = if text_index {
            store
                .with_text_index(ikigai_gonk::text_index::enrollment())
                .unwrap()
        } else {
            store
        };
        let kept = store.clone();
        let hub = Arc::new(
            ikigai_gonk::compose_serving_with_lifecycles(
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
        let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
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
        let hub_kept = Arc::clone(&hub);
        let door = doors::HttpDoor {
            anonymous: grants_for("default", Authority::Write).unwrap(),
            bind: addr,
            passkeys: Some(passkeys),
            public_origin: None,
            public: Vec::new(),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door.clone()),
                listener,
                doors::edge_config(door),
            ))
        });
        Server {
            addr,
            hub: hub_kept,
            store: kept,
            _config: config,
        }
    }

    /// The store's own status of the ledger graph's index,
    /// `urn:iki:store:text-index:graph:{graph}`, read under the ledger's grant.
    fn index_status(&self) -> String {
        let thread = ikigai_store::graph_thread(GRAPH);
        let encoded = thread
            .strip_prefix("urn:iki:store:graph-written:")
            .expect("the store's graph thread prefix");
        let iri = Iri::parse(format!("urn:iki:store:text-index:graph:{encoded}")).unwrap();
        let cap = Capability::scoped(grants_for("default", Authority::Read).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        match runtime.block_on(self.hub.issue(Request::new(Verb::Source, iri), &cap)) {
            Ok(status) => String::from_utf8_lossy(&status.bytes).into_owned(),
            Err(e) => format!("(the status read failed: {e})"),
        }
    }

    /// The literals the index should hold: every title and body in the ledger's graph, the
    /// count ikigai-ledger's `complete` compares `documents:` with.
    fn enrolled_literals(&self) -> usize {
        let body = ikigai_gonk::text_index::body();
        self.store
            .snapshot_iter()
            .map(|quad| quad.expect("a quad"))
            .filter(|quad| {
                quad.graph_name.to_string() == format!("<{GRAPH}>")
                    && (quad.predicate.as_str() == ikigai_gonk::text_index::TITLE
                        || quad.predicate.as_str() == body)
            })
            .count()
    }

    /// Wait until the index is complete in the ledger's sense (`state: ready`, `pending: 0`
    /// and `documents:` equal to the enrolled literals), or fail with the last status read.
    fn wait_for_complete_index(&self) {
        let deadline = Instant::now() + SETTLE_CEILING;
        loop {
            let status = self.index_status();
            let literals = self.enrolled_literals();
            let lines: Vec<&str> = status.lines().map(str::trim).collect();
            if lines.contains(&"state: ready")
                && lines.contains(&"pending: 0")
                && lines.contains(&format!("documents: {literals}").as_str())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the index of {GRAPH} did not settle within {SETTLE_CEILING:?} at {literals} \
                 enrolled literals; its last status:\n{status}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn raw(&self, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> String {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n",
            self.addr.port()
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ));
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "{method} {path}: {response}"
        );
        response
    }

    fn page(&self, path: &str) -> String {
        self.raw(
            "GET",
            path,
            &[
                ("Accept", CHROME_ACCEPT.to_string()),
                ("Sec-Fetch-Site", "none".to_string()),
            ],
            "",
        )
    }

    fn file(&self, title: &str, body: &str) {
        let fields = [
            ("_ledger", "default"),
            ("_action", "append"),
            ("_then", "items"),
            ("content", &format!("{title}\n\n{body}")),
        ];
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        self.raw(
            "POST",
            "/act",
            &[
                ("Accept", "*/*".to_string()),
                ("Origin", format!("http://localhost:{}", self.addr.port())),
                ("Sec-Fetch-Site", "same-origin".to_string()),
                ("HX-Request", "true".to_string()),
                (
                    "Content-Type",
                    "application/x-www-form-urlencoded".to_string(),
                ),
            ],
            &body,
        );
    }
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Three items whose titles hold "lease" once, twice and three times, in filing order, so the
/// default order (by number here) and the relevance order disagree.
fn seed(server: &Server) {
    server.file("One lease mention among many other words here", "z");
    server.file("Parser panic on lease and lease", "y");
    server.file("Lease renewal three lease lease", "x");
    server.file("Nothing relevant", "the lease ran out");
}

fn position(page: &str, needle: &str) -> usize {
    page.find(needle)
        .unwrap_or_else(|| panic!("`{needle}` not on the page: {page}"))
}

#[test]
fn a_search_offers_most_relevant_first_and_lists_the_best_match_first() {
    let server = Server::start(true);
    seed(&server);
    // The index is derived and commits after the writes; wait on the store's own status
    // until it is complete, as the ledger checks before it ranks (ikigai-ledger 0.26's
    // `complete`). Through ikigai-store 0.6.3 a first build overlapping the seed's writes could
    // settle one document high and never complete (ledger #4448, #4438).
    server.wait_for_complete_index();
    let page = server.page("/l/default?text=lease");
    assert!(
        page.contains("Show the most relevant items first"),
        "a ready index is offered under a search: {page}"
    );
    assert!(page.contains("sort=relevance"), "{page}");
    // The default order: filing order here.
    assert!(
        position(&page, "One lease mention") < position(&page, "Lease renewal three"),
        "{page}"
    );
    // No search, no offer: the ledger refuses relevance without one.
    let plain = server.page("/l/default");
    assert!(
        !plain.contains("Show the most relevant items first"),
        "{plain}"
    );
    assert!(
        !plain.contains("Most relevant ordering is not available"),
        "{plain}"
    );

    // The index is complete, so the ledger ranks at once.
    let ranked = server.page("/l/default?text=lease&sort=relevance");
    assert!(
        ranked.contains("Most relevant items first."),
        "the order is said in words: {ranked}"
    );
    assert!(
        position(&ranked, "Lease renewal three") < position(&ranked, "One lease mention"),
        "the title with the most hits ranks first: {ranked}"
    );
    assert!(
        !ranked.contains("Show the most relevant items first"),
        "not offered again once in effect: {ranked}"
    );
}

#[test]
fn without_the_index_the_page_says_so_and_a_relevance_order_lists_nothing() {
    let server = Server::start(false);
    seed(&server);
    let page = server.page("/l/default?text=lease");
    assert!(
        page.contains(
            "Most relevant ordering is not available: the search index for ledger default is \
             not ready."
        ),
        "{page}"
    );
    assert!(
        !page.contains("Show the most relevant items first"),
        "{page}"
    );
    assert!(
        page.contains("Lease renewal three"),
        "the scan still answers: {page}"
    );

    let refused = server.page("/l/default?text=lease&sort=relevance");
    assert!(
        refused.contains("Most relevant ordering is not available:"),
        "{refused}"
    );
    assert!(
        refused.contains("nothing was listed"),
        "the ledger's own words: {refused}"
    );
    assert!(
        refused.contains("3 open items match; none are listed."),
        "{refused}"
    );
    assert!(!refused.contains("No items match."), "{refused}");
    assert!(!refused.contains("Lease renewal three"), "{refused}");
}

#[test]
fn a_phrase_is_reachable_from_the_form_and_filters_the_list_and_its_counts() {
    let server = Server::start(false);
    seed(&server);
    let page = server.page("/l/default");
    assert!(page.contains("name='phrase'"), "{page}");
    assert!(page.contains("Exact phrase in the title"), "{page}");
    let phrased = server.page("/l/default?phrase=lease+and+lease");
    assert!(
        phrased.contains("Parser panic on lease and lease"),
        "{phrased}"
    );
    assert!(!phrased.contains("Lease renewal three"), "{phrased}");
    assert!(
        phrased.contains("value='lease and lease'"),
        "the form keeps it: {phrased}"
    );
    let facet = server.page("/l/default/facet?phrase=lease+and+lease&page=true");
    assert!(
        facet.contains("(1)") && !facet.contains("(4)"),
        "facet counts follow the phrase: {facet}"
    );
}
