//! Ledger [#2972](http://localhost:1062/l/default/item/2972): the Dashboard's Cache and System tabs,
//! which only an operator's grant sees.
//!
//! These read the door over real HTTP. What a browser does with them (the keys, axe in both themes
//! at 320 and 1280 px, only the shown panel polling) is `tests/browser/dashboard_operator_2972.mjs`.
//! The ledger names and the workers' lines are neutral samples.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::dashboard::Tab;
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::operator::{self, Part};
use ikigai_gonk::system::{self, System};
use ikigai_gonk::{compose_with, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::{DurableStore, SharerWrites};

const JS: &str = include_str!("../web/gonk.js");

const INSPECT: &str = "urn:cap:kernel:inspect";

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    http: Arc<Kernel>,
    _dirs: Vec<tempfile::TempDir>,
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

impl Server {
    /// A server configured with `configured` ledgers, whose anonymous caller reads `readable` and
    /// holds `extra`, with the System family bound.
    fn start(configured: &[&str], readable: &[&str], extra: &[&str]) -> Server {
        let names: Vec<String> = readable.iter().map(|s| s.to_string()).collect();
        let mut anonymous = grants_for_all(&names, Authority::Read).unwrap();
        anonymous.extend(extra.iter().map(|s| s.to_string()));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        std::fs::write(store_dir.path().join("sample"), [0; 2048]).unwrap();
        // Shared with a declaration, and read through a handle, as `main` opens the live one.
        let (store, handle) =
            DurableStore::in_memory_shared_declaring(SharerWrites::only_the_default_graph())
                .unwrap();
        let dataset = backup::Dataset::new(&store, handle);
        let every = std::time::Duration::from_secs(86_400);
        let system = System::new(
            store_dir.path().to_path_buf(),
            backups.path().to_path_buf(),
            Some(every),
        );
        system.listening("HTTP", addr.to_string());
        let hub = Arc::new(compose_with(
            store,
            None,
            Vec::new(),
            vec![Arc::new(system.space())],
            Some(Backups {
                settings: Arc::new(backup::Settings {
                    dir: backups.path().to_path_buf(),
                    keep: 5,
                    every: Some(every),
                    store_path: "/nonexistent/live/store".into(),
                    scripts: None,
                }),
                jobs: None,
                next: Default::default(),
                dataset,
            }),
            None,
            None,
        ));
        system.attach(&hub);
        let layout = quic::Layout::in_config_home(config.path());
        let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: configured.iter().map(|s| s.to_string()).collect(),
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            holder_aliases: Default::default(),
            toolbox: false,
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        let serving = Arc::clone(&http);
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                serving,
                doors::http_cap(door.clone()),
                listener,
                doors::edge_config(door),
            ))
        });
        Server {
            addr,
            hub,
            http,
            _dirs: vec![config, store_dir, backups],
        }
    }

    fn operator() -> Server {
        Server::start(
            &["default", "alpha"],
            &["default", "alpha"],
            &[INSPECT, system::CAP_READ],
        )
    }

    fn get(&self, path: &str, site: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html\r\n\
             Sec-Fetch-Site: {site}\r\nConnection: close\r\n\r\n",
            self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        (
            head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            body.replace('\'', "\""),
        )
    }

    fn page(&self, path: &str) -> String {
        let (status, body) = self.get(path, "none");
        assert_eq!(status, 200, "{path}: {body}");
        body
    }

    fn fragment(&self, path: &str) -> String {
        let (status, body) = self.get(path, "same-origin");
        assert_eq!(status, 200, "{path}: {body}");
        body
    }

    fn act(&self, ledger: &str, action: &str, args: &[(&str, &str)]) {
        let iri = format!("{}{action}", Ledger::parse(ledger).unwrap().prefix());
        futures::executor::block_on(Kernel::issue(
            &self.hub,
            request(Verb::Sink, &iri, args),
            &Capability::root(),
        ))
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
    }

    fn hub_read(&self, iri: &str, args: &[(&str, &str)]) {
        futures::executor::block_on(Kernel::issue(
            &self.hub,
            request(Verb::Source, iri, args),
            &Capability::root(),
        ))
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
    }
}

fn tab_link(name: &str) -> String {
    format!("id=\"dashboard-tab-{name}\"")
}

#[test]
fn each_panel_renders_alone() {
    let server = Server::operator();
    let cache = server.fragment(&Tab::Cache.panel_path());
    for part in ["entries", "threads", "uncached"] {
        assert!(
            cache.contains(&format!("hx-get=\"/dashboard/part/{part}\"")),
            "{cache}"
        );
    }
    for part in ["disk", "store", "gonk", "workers", "claimed"] {
        assert!(
            !cache.contains(&format!("/dashboard/part/{part}")),
            "{cache}"
        );
    }
    let system = server.fragment(&Tab::System.panel_path());
    for part in ["disk", "store", "gonk", "workers", "claimed"] {
        assert!(
            system.contains(&format!("hx-get=\"/dashboard/part/{part}\"")),
            "{system}"
        );
    }
    assert!(!system.contains("/dashboard/part/entries"), "{system}");
    for (panel, heading) in [(&cache, "Cache</h2>"), (&system, "System</h2>")] {
        assert!(panel.contains(heading), "{panel}");
        assert!(!panel.contains("<html"), "{panel}");
        assert!(!panel.contains("aria-live"), "{panel}");
        assert!(!panel.contains("<figure"), "{panel}");
    }

    // With no script a URL is a tab, and the other three are empty and hidden.
    for (path, shown, title) in [
        ("/dashboard/cache", "cache", "Dashboard: Cache · gonk"),
        ("/dashboard/system", "system", "Dashboard: System · gonk"),
    ] {
        let page = server.page(path);
        assert!(page.contains(&format!("<title>{title}</title>")), "{page}");
        assert!(page.contains(&format!("aria-labelledby=\"dashboard-tab-{shown}\"")));
        for name in ["ledgers", "agents", "cache", "system"] {
            assert!(page.contains(&tab_link(name)), "{name}: {page}");
        }
        assert!(!page.contains("hx-get=\"/dashboard/chart"), "{page}");
        assert_eq!(page.matches("<h1").count(), 1, "{path}");
    }
}

#[test]
fn a_viewer_without_the_grant_never_sees_them_and_is_refused_them() {
    let server = Server::start(&["default"], &["default"], &[]);
    for path in ["/dashboard", "/dashboard/ledgers", "/dashboard/agents"] {
        let page = server.page(path);
        assert!(page.contains(&tab_link("ledgers")), "{page}");
        assert!(!page.contains(&tab_link("cache")), "{page}");
        assert!(!page.contains(&tab_link("system")), "{page}");
        assert!(!page.contains("/dashboard/cache"), "{page}");
        assert!(!page.contains("/dashboard/system"), "{page}");
    }
    let mut paths: Vec<String> = vec![
        Tab::Cache.path(),
        Tab::System.path(),
        Tab::Cache.panel_path(),
        Tab::System.panel_path(),
    ];
    paths.extend(Part::ALL.iter().map(|part| part.path()));
    for path in paths {
        assert_eq!(server.get(&path, "same-origin").0, 403, "{path}");
    }
}

/// One tab's grant is that tab's alone.
#[test]
fn each_grant_opens_its_own_tab() {
    let cache_only = Server::start(&["default"], &["default"], &[INSPECT]);
    let page = cache_only.page("/dashboard/ledgers");
    assert!(page.contains(&tab_link("cache")), "{page}");
    assert!(!page.contains(&tab_link("system")), "{page}");
    assert_eq!(cache_only.get("/dashboard/system", "none").0, 403);
    assert_eq!(cache_only.get(&Part::Disk.path(), "same-origin").0, 403);

    let system_only = Server::start(&["default"], &["default"], &[system::CAP_READ]);
    let page = system_only.page("/dashboard/ledgers");
    assert!(page.contains(&tab_link("system")), "{page}");
    assert!(!page.contains(&tab_link("cache")), "{page}");
    assert_eq!(system_only.get("/dashboard/cache", "none").0, 403);
    assert_eq!(system_only.get(&Part::Entries.path(), "same-origin").0, 403);

    // An operator with no ledger still has their tabs.
    let no_ledger = Server::start(&["default"], &[], &[INSPECT]);
    let page = no_ledger.page("/dashboard/cache");
    assert!(page.contains(&tab_link("cache")), "{page}");
}

/// ★ A page another site navigated to keeps a ledger's read and none of this.
#[test]
fn another_sites_navigation_loses_both() {
    let server = Server::operator();
    let (status, page) = server.get("/dashboard/ledgers", "cross-site");
    assert_eq!(status, 200, "{page}");
    assert!(!page.contains(&tab_link("cache")), "{page}");
    assert!(!page.contains(&tab_link("system")), "{page}");
    assert_eq!(server.get("/dashboard/cache", "cross-site").0, 403);
    assert_eq!(server.get("/dashboard/system", "cross-site").0, 403);
}

#[test]
fn each_new_resource_declares_and_enforces_its_capability() {
    let server = Server::start(&["default"], &["default"], &[]);
    let reader =
        Capability::scoped(grants_for_all(&["default".to_string()], Authority::Read).unwrap());
    let mut resources: Vec<(String, &str, &Arc<Kernel>)> = vec![
        (system::DISK.to_string(), system::CAP_READ, &server.hub),
        (system::STORE.to_string(), system::CAP_READ, &server.hub),
        (system::GONK.to_string(), system::CAP_READ, &server.hub),
        (
            operator::WORKERS.to_string(),
            system::CAP_READ,
            &server.http,
        ),
    ];
    for (iri, tab) in [
        (ikigai_gonk::dashboard::CACHE_PAGE_IRI, Tab::Cache),
        (ikigai_gonk::dashboard::CACHE_PANEL_IRI, Tab::Cache),
        (ikigai_gonk::dashboard::SYSTEM_PAGE_IRI, Tab::System),
        (ikigai_gonk::dashboard::SYSTEM_PANEL_IRI, Tab::System),
    ] {
        resources.push((iri.to_string(), tab.capability().unwrap(), &server.http));
    }
    for part in Part::ALL {
        resources.push((part.iri(), part.tab().capability().unwrap(), &server.http));
    }
    for (iri, needed, kernel) in resources {
        let parsed = Iri::parse(&iri).unwrap();
        let described = kernel.describe(&parsed).unwrap_or_else(|| panic!("{iri}"));
        assert!(
            described.requires.iter().any(|c| c == needed),
            "{iri} declares {:?}",
            described.requires
        );
        let refused = futures::executor::block_on(Kernel::issue(
            kernel,
            request(Verb::Source, &iri, &[]),
            &reader,
        ));
        assert!(
            matches!(refused, Err(Error::Denied(_))),
            "{iri}: {refused:?}"
        );
    }
    // The hub's own answers to a holder, as JSON.
    let holder = Capability::scoped([system::CAP_READ]);
    for iri in [system::DISK, system::STORE, system::GONK] {
        let answer = futures::executor::block_on(Kernel::issue(
            &server.hub,
            request(Verb::Source, iri, &[]),
            &holder,
        ))
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
        let _: serde_json::Value = serde_json::from_slice(&answer.bytes).unwrap();
    }
    // ★ The two tokens the Store row reads under stay ones no grant may hold.
    let scopes: Vec<String> = system::store_scopes()
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(!ikigai_gonk::grants::gonk_admin_scopes(&scopes).is_empty());
}

/// ★ The HUB's cache: a door kernel's is empty. These readouts' grammar is read from a real kernel,
/// so a change of it goes red here rather than blanking the tab.
#[test]
fn the_cache_tab_reads_the_hubs_cache_and_names_entries() {
    let server = Server::operator();
    server.act("default", "append", &[("content", "sample item")]);
    // The Ledgers tab fills the hub's cache: the counts' name is what to find.
    server.page("/dashboard/ledgers");
    let part = server.fragment(&Part::Entries.path());
    assert!(part.contains("urn:iki:ledger:default:counts"), "{part}");
    assert!(part.contains(">live<"), "{part}");
    assert!(part.contains("Hits and misses"), "{part}");
    assert!(part.contains("not counted by this kernel"), "{part}");
    assert!(!part.contains("could not be read"), "{part}");
    assert!(!part.contains("Entries</dt><dd>0 of"), "{part}");
    let (entries, table) = (
        part.find("<dt>Entries").unwrap(),
        part.find("<table").unwrap(),
    );
    assert!(entries < table, "totals first: {part}");

    // A write cuts threads, and a stale entry is marked.
    server.act("default", "append", &[("content", "another")]);
    let threads = server.fragment(&Part::Threads.path());
    assert!(threads.contains("urn:"), "{threads}");
    assert!(!threads.contains("could not be read"), "{threads}");
    let part = server.fragment(&Part::Entries.path());
    assert!(part.contains(">cut<"), "{part}");

    // Nothing above went uncached, so this asks for something that always does.
    let quiet = server.fragment(&Part::Uncached.path());
    assert!(
        quiet.contains("Every read computed lately was cached."),
        "{quiet}"
    );
    server.hub_read(system::GONK, &[]);
    let uncached = server.fragment(&Part::Uncached.path());
    assert!(
        uncached.contains(&format!(
            "<th scope=\"row\">{}</th><td>1</td>",
            system::GONK
        )),
        "{uncached}"
    );
    assert!(!uncached.contains("could not be read"), "{uncached}");
}

/// ★ A query's text is an argument. It must not reach a page, whoever asked it.
#[test]
fn the_cache_tab_shows_names_and_never_an_arguments_value() {
    let server = Server::operator();
    server.act("default", "append", &[("content", "sample item")]);
    let query = "SELECT ?s WHERE { ?s ?p \"NEVER-SHOW-THIS\" }";
    let graph = Ledger::parse("default").unwrap().graph();
    for _ in 0..2 {
        server.hub_read("urn:sparql:select", &[("query", query), ("graph", &graph)]);
    }
    for part in [Part::Entries, Part::Threads, Part::Uncached] {
        let body = server.fragment(&part.path());
        assert!(!body.contains("NEVER-SHOW-THIS"), "{part:?}: {body}");
    }
}

#[test]
fn the_system_tab_says_what_each_row_is_and_what_to_do() {
    let server = Server::operator();
    server.act("default", "append", &[("content", "sample item")]);

    let disk = server.fragment(&Part::Disk.path());
    for said in ["Store", "Backups", "Free", "Total", "GiB"] {
        assert!(disk.contains(said), "{said}: {disk}");
    }
    assert!(disk.contains("floor"), "{disk}");

    let gonk = server.fragment(&Part::Gonk.path());
    for said in [
        env!("CARGO_PKG_VERSION"),
        "Commit",
        "Built",
        "Up for",
        "HTTP at 127.0.0.1",
        "ikigai-ledger",
        "ikigai-browse",
    ] {
        assert!(gonk.contains(said), "{said}: {gonk}");
    }

    // No backup: the row says so in words, and what to do.
    let store = server.fragment(&Part::Store.path());
    assert!(store.contains("needs-attention"), "{store}");
    assert!(store.contains("Needs attention:"), "{store}");
    assert!(store.contains("Source urn:iki:gonk:backup"), "{store}");
    assert!(store.contains("2.0 KiB"), "the size on disk: {store}");
    assert!(store.contains("<dt>Quads</dt><dd>"), "{store}");
    assert!(!store.contains("<dd>could not be read"), "{store}");

    // One backup: fine, its time and age, and the quads per graph it recorded.
    futures::executor::block_on(Kernel::issue(
        &server.hub,
        request(Verb::Source, backup::BACKUP, &[]),
        &Capability::root(),
    ))
    .expect("a backup");
    let store = server.fragment(&Part::Store.path());
    assert!(!store.contains("needs-attention"), "{store}");
    assert!(store.contains("<strong>OK:</strong>"), "{store}");
    assert!(store.contains(" UTC, "), "{store}");
    assert!(store.contains("urn:iki:ledger:graph:default"), "{store}");
    assert!(store.contains("Quads per graph"), "{store}");
}

#[test]
fn the_workers_rows_are_each_status_items_latest_lines_as_data() {
    let server = Server::start(&["default", "secret"], &["default"], &[system::CAP_READ]);
    // A row with no line yet says what to do.
    server.act("default", "append", &[("content", "status of sample-a")]);
    server.act(
        "default",
        "label",
        &[("item", "1"), ("content", "worker-status:sample-a")],
    );
    let workers = server.fragment(&Part::Workers.path());
    assert!(workers.contains("sample-a"), "{workers}");
    assert!(workers.contains("needs-attention"), "{workers}");

    for line in [
        "Worker: holder=old",
        "Worker: holder=sample-a labels=worker:pool <script>x</script>",
        "Disk: 2 GB free, under the 30 GB floor",
        "Usage: agent=sample",
    ] {
        server.act("default", "comment", &[("item", "1"), ("content", line)]);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let workers = server.fragment(&Part::Workers.path());
    assert!(
        workers.contains("labels=worker:pool"),
        "the latest: {workers}"
    );
    assert!(!workers.contains("holder=old"), "{workers}");
    assert!(!workers.contains("<script>"), "as text: {workers}");
    assert!(workers.contains("&lt;script&gt;"), "{workers}");
    assert!(workers.contains("Disk: 2 GB free"), "{workers}");
    assert!(!workers.contains("Usage:"), "{workers}");
    assert!(!workers.contains("needs-attention"), "{workers}");
    assert!(workers.contains("href=\"/l/default/item/1\""), "{workers}");
    let disk = server.fragment(&Part::Disk.path());
    assert!(
        disk.contains("<strong>sample-a</strong>: Disk: 2 GB free"),
        "{disk}"
    );

    // What is claimed now, and nothing from a ledger this viewer may not read.
    server.act(
        "default",
        "claim",
        &[("item", "1"), ("content", "sample-holder")],
    );
    server.act("secret", "append", &[("content", "hidden title")]);
    server.act(
        "secret",
        "label",
        &[("item", "1"), ("content", "worker-status:hidden")],
    );
    server.act(
        "secret",
        "claim",
        &[("item", "1"), ("content", "hidden-holder")],
    );
    let claimed = server.fragment(&Part::Claimed.path());
    assert!(claimed.contains("sample-holder"), "{claimed}");
    assert!(!claimed.contains("hidden"), "{claimed}");
    let workers = server.fragment(&Part::Workers.path());
    assert!(!workers.contains("hidden"), "{workers}");
}

/// The script's half is run by `tests/browser/dashboard_operator_2972.mjs`; this pins it as wiring.
#[test]
fn a_readout_says_only_a_change_in_what_needs_attention() {
    assert!(JS.contains("if (kind === \"readout\") liveSayAttention(elt, next);"));
    assert!(JS.contains("\" needs attention.\""));
    assert!(JS.contains("\" no longer needs attention.\""));
}
