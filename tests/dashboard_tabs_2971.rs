//! Ledger [#2971](http://localhost:1062/l/default/item/2971): the Dashboard's two tabs, each with its
//! own URL and its own fragment, and only the one shown asks anything.
//!
//! These read the door over real HTTP. What a browser does with it (the ARIA pattern, the keys,
//! Back and Forward, only the shown panel polling) is `tests/browser/dashboard_tabs_2971.mjs`. The
//! ledger names are neutral samples.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, TraceEvent, Tracer, Verb};
use ikigai_gonk::dashboard::{self, Tab};
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::DurableStore;

const JS: &str = include_str!("../web/gonk.js");
const CSS: &str = include_str!("../web/gonk.css");

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    http: Arc<Kernel>,
    _config: tempfile::TempDir,
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

impl Server {
    /// A server configured with `configured` ledgers, whose anonymous caller reads `readable`.
    fn readers(configured: &[&str], readable: &[&str]) -> Server {
        let names: Vec<String> = readable.iter().map(|s| s.to_string()).collect();
        let anonymous = grants_for_all(&names, Authority::Read).unwrap();
        // The door's stacks, as `main` builds them: the agent questions go deep.
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
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
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
            page_assets: Default::default(),
            toolbox: false,
            faces: Vec::new(),
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
            public: Vec::new(),
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
            _config: config,
        }
    }

    /// `(status, body)` of a GET with these `Sec-Fetch-Site` and `Accept` lines.
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

    /// A browser's navigation to a page, as a link or a reload makes it with no script.
    fn page(&self, path: &str) -> String {
        let (status, body) = self.get(path, "none");
        assert_eq!(status, 200, "{path}: {body}");
        body
    }

    /// What the page's script fetches on a switch.
    fn panel(&self, tab: Tab) -> String {
        let path = tab.panel_path();
        let (status, body) = self.get(&path, "same-origin");
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

    fn file(&self, ledger: &str, title: &str) {
        self.act(ledger, "append", &[("content", title)]);
    }
}

/// What `panel`'s element says: `(whether it is hidden, what it holds)`.
fn panel_of<'a>(page: &'a str, name: &str) -> (bool, &'a str) {
    let id = format!("id=\"dashboard-panel-{name}\"");
    let at = page.find(&id).unwrap_or_else(|| panic!("{id}: {page}"));
    let open = page[..at].rfind("<section").unwrap();
    let close = at + page[at..].find('>').unwrap();
    let end = close
        + page[close..]
            .find("</section>")
            .unwrap_or(page.len() - close);
    (page[open..close].contains("hidden="), &page[close + 1..end])
}

/// A tab's link, whole.
fn link_of<'a>(page: &'a str, name: &str) -> &'a str {
    let id = format!("id=\"dashboard-tab-{name}\"");
    let at = page.find(&id).unwrap_or_else(|| panic!("{id}: {page}"));
    let open = page[..at].rfind("<a ").unwrap();
    &page[open..at + page[at..].find("</a>").unwrap()]
}

/// Agents to show on the Agents tab, in two sample ledgers.
fn sampled() -> Server {
    let server = Server::readers(&["default", "alpha"], &["default", "alpha"]);
    for ledger in ["default", "alpha"] {
        server.file(ledger, "sample item");
        server.act(
            ledger,
            "claim",
            &[("item", "1"), ("content", "sample-agent")],
        );
    }
    server
}

#[test]
fn each_panel_renders_alone() {
    let server = sampled();
    let ledgers = server.panel(Tab::Ledgers);
    assert!(
        ledgers.contains("hx-get=\"/dashboard/chart/default\""),
        "{ledgers}"
    );
    assert!(
        ledgers.contains("hx-get=\"/dashboard/chart/alpha\""),
        "{ledgers}"
    );
    assert!(ledgers.contains("2 ledgers you may read"), "{ledgers}");
    assert!(!ledgers.contains("agent-panel"), "{ledgers}");
    assert!(!ledgers.contains("/dashboard/agents"), "{ledgers}");

    let agents = server.panel(Tab::Agents);
    assert!(
        agents.contains("hx-get=\"/dashboard/agents/default\""),
        "{agents}"
    );
    assert!(
        agents.contains("hx-get=\"/dashboard/agents/alpha\""),
        "{agents}"
    );
    assert!(
        agents.contains("hx-get=\"/dashboard/agents-note\""),
        "{agents}"
    );
    assert!(agents.contains("sample-agent"), "{agents}");
    assert!(!agents.contains("<figure"), "{agents}");
    assert!(!agents.contains("/dashboard/chart"), "{agents}");

    // A fragment, with no page around it, and each with the heading its h3s sit under.
    for (panel, heading) in [(&ledgers, "Ledgers</h2>"), (&agents, "Agents</h2>")] {
        assert!(panel.contains(heading), "{panel}");
        assert!(!panel.contains("<html"), "{panel}");
        assert!(!panel.contains("<header"), "{panel}");
        assert!(!panel.contains("aria-live"), "{panel}");
    }
}

/// With no script a URL is a tab: the server alone picks the panel, and the other is not drawn.
#[test]
fn the_tab_url_selects_the_panel_with_no_script() {
    let server = sampled();
    for (path, shown, other, title) in [
        (
            "/dashboard",
            "ledgers",
            "agents",
            "Dashboard: Ledgers · gonk",
        ),
        (
            "/dashboard/ledgers",
            "ledgers",
            "agents",
            "Dashboard: Ledgers · gonk",
        ),
        (
            "/dashboard/agents",
            "agents",
            "ledgers",
            "Dashboard: Agents · gonk",
        ),
    ] {
        let page = server.page(path);
        assert!(
            page.contains(&format!("<title>{title}</title>")),
            "{path}: {page}"
        );
        assert_eq!(page.matches("<h1").count(), 1, "{path}");

        let (hidden, held) = panel_of(&page, shown);
        assert!(!hidden, "{path}: {page}");
        assert!(held.contains("data-live"), "{path}: {held}");
        let (hidden, held) = panel_of(&page, other);
        assert!(hidden, "{path}: {page}");
        assert_eq!(held, "", "{path}: the hidden panel holds nothing");

        // Plain links: the current one says so, each goes to its own page, and nothing promises
        // keys that only the script gives.
        assert!(
            link_of(&page, shown).contains("aria-current=\"page\""),
            "{page}"
        );
        assert!(!link_of(&page, other).contains("aria-current"), "{page}");
        assert!(link_of(&page, shown).contains(&format!("href=\"/dashboard/{shown}\"")));
        assert!(link_of(&page, other).contains(&format!("href=\"/dashboard/{other}\"")));
        for role in [
            "role=\"tablist\"",
            "role=\"tab\"",
            "role=\"tabpanel\"",
            "aria-selected",
        ] {
            assert!(!page.contains(role), "{path}, {role}: {page}");
        }
        // Each panel is named by its tab, the tab being a link that is always there.
        assert!(
            page.contains(&format!("aria-labelledby=\"dashboard-tab-{shown}\"")),
            "{page}"
        );
    }
    // A link to one tab shows no fragment of the other, so it polls nothing of it.
    let ledgers = server.page("/dashboard/ledgers");
    assert!(!ledgers.contains("hx-get=\"/dashboard/agents"), "{ledgers}");
    assert!(!ledgers.contains("agent-panel"), "{ledgers}");
    let agents = server.page("/dashboard/agents");
    assert!(!agents.contains("hx-get=\"/dashboard/chart"), "{agents}");
    assert!(!agents.contains("<figure"), "{agents}");
}

/// The switch and the load cannot draw different things.
#[test]
fn a_page_is_made_of_the_panel_a_switch_fetches() {
    let server = sampled();
    for (path, tab) in [
        ("/dashboard/ledgers", Tab::Ledgers),
        ("/dashboard/agents", Tab::Agents),
    ] {
        // A table's links and its sentence name the minute, which may turn between the two asks,
        // and cannot turn twice in as many tries.
        let same = (0..2).any(|_| server.page(path).contains(&server.panel(tab)));
        assert!(same, "{path}: {}", server.panel(tab));
    }
}

/// The pause control and the one status region are the page's, so they serve either tab.
#[test]
fn the_pause_control_comes_before_the_tabs_on_either_page() {
    let server = sampled();
    for path in ["/dashboard/ledgers", "/dashboard/agents"] {
        let page = server.page(path);
        let pause = page.find("id=\"dashboard-pause\"").unwrap();
        let tabs = page.find("id=\"dashboard-tabs\"").unwrap();
        let panel = page.find("id=\"dashboard-panel-ledgers\"").unwrap();
        assert!(pause < tabs && tabs < panel, "{path}");
        assert_eq!(page.matches("id=\"dashboard-pause\"").count(), 1, "{path}");
        assert_eq!(page.matches("aria-live").count(), 1, "{path}");
    }
}

#[derive(Default)]
struct Collect(Mutex<Vec<TraceEvent>>);

impl Tracer for Collect {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// What the hub was asked for one read of `iri`.
fn asked(server: &Server, iri: &str, reader: &Capability) -> Vec<String> {
    let trace = Arc::new(Collect::default());
    server.hub.set_tracer(trace.clone());
    futures::executor::block_on(server.http.issue_traced(
        request(Verb::Source, iri, &[]),
        reader,
        Arc::new(Collect::default()),
    ))
    .expect("it answers");
    server.hub.clear_tracer();
    let events = std::mem::take(&mut *trace.0.lock().unwrap());
    events
        .iter()
        .filter(|e| !e.cache_hit)
        .map(|e| e.target.to_string())
        .collect()
}

/// ★ The hidden panel asks the store nothing, on a load or on a switch: the two tabs' questions do
/// not overlap, so a tab's read after a write asks only its own.
#[test]
fn the_hidden_panel_asks_the_store_nothing() {
    let names = ["default".to_string(), "alpha".to_string()];
    let server = sampled();
    let reader = Capability::scoped(grants_for_all(&names, Authority::Read).unwrap());
    let is_counts = |target: &String| target.ends_with(":counts");
    // Each agent question goes through the query face, and nothing on the Ledgers tab does.
    let is_agents = |target: &String| target == "urn:sparql:select";
    for (iri, shows_counts) in [
        (dashboard::PAGE_IRI, true),
        (dashboard::LEDGERS_PAGE_IRI, true),
        (dashboard::LEDGERS_PANEL_IRI, true),
        (dashboard::AGENTS_PAGE_IRI, false),
        (dashboard::AGENTS_PANEL_IRI, false),
    ] {
        // A write drops what is cached, so what is asked below is really asked.
        server.file("default", "another");
        let asked = asked(&server, iri, &reader);
        assert_eq!(
            asked.iter().any(is_counts),
            shows_counts,
            "{iri}, the counts: {asked:?}"
        );
        assert_eq!(
            asked.iter().any(is_agents),
            !shows_counts,
            "{iri}, the agents: {asked:?}"
        );
        if shows_counts {
            let queries = asked.iter().filter(|t| t.starts_with("urn:iki:store:"));
            assert_eq!(
                queries.count(),
                names.len(),
                "{iri}: one per ledger: {asked:?}"
            );
        }
    }
}

#[test]
fn a_panel_lists_only_what_the_viewer_may_read() {
    let server = Server::readers(&["default", "secret"], &["default"]);
    server.file("secret", "sample item");
    for tab in Tab::EVERYONE {
        let panel = server.panel(tab);
        assert!(panel.contains("/l/default"), "{panel}");
        assert!(!panel.contains("secret"), "{panel}");
    }
    // Nothing readable: a page says how to get in with no tabs on it, and a page held open since
    // the grant was lost gets a panel with nothing in it to poll.
    let nobody = Server::readers(&["default"], &[]);
    for tab in Tab::EVERYONE {
        let page = nobody.page(&tab.path());
        assert!(page.contains("Sign in to read the ledgers."), "{page}");
        assert!(!page.contains("dashboard-tabs"), "{page}");
        let panel = nobody.panel(tab);
        assert!(!panel.contains("<figure"), "{panel}");
        assert!(!panel.contains("agent-panel"), "{panel}");
    }
    assert!(nobody
        .panel(Tab::Ledgers)
        .contains("0 ledgers you may read"));
}

/// The script's half is run by `tests/browser/dashboard_tabs_2971.mjs`; this pins it as WIRING, so
/// CI notices the parts going missing.
#[test]
fn the_script_wires_the_tabs_pattern_and_empties_the_hidden_panel() {
    for wired in [
        // The pattern, given only where a script can honor it…
        "list.setAttribute(\"role\", \"tablist\")",
        "tab.setAttribute(\"role\", \"tab\")",
        "tab.setAttribute(\"aria-controls\", tab.getAttribute(\"data-panel\"))",
        "tabPanel(tab).setAttribute(\"role\", \"tabpanel\")",
        "tab.setAttribute(\"aria-selected\", selected ? \"true\" : \"false\")",
        // …one tab stop, and the keys…
        "tab.tabIndex = selected ? 0 : -1",
        "e.key === \"ArrowRight\"",
        "e.key === \"ArrowLeft\"",
        // …the URL and history…
        "history.pushState({ dashboardTab:",
        "window.addEventListener(\"popstate\"",
        // …and the hidden panel left with nothing that could ask.
        "window.htmx.trigger(elt, \"htmx:abort\")",
        "left.replaceChildren()",
        "window.htmx.process(panel)",
    ] {
        assert!(JS.contains(wired), "web/gonk.js: {wired}");
    }
    // A switch never sends focus into a panel: the only focus calls go to a tab.
    let section = &JS[JS.find("the Dashboard's tabs (ledger #2971)").unwrap()
        ..JS.find("sign-in notices").unwrap()];
    let focuses: Vec<_> = section.match_indices(".focus(").collect();
    assert_eq!(focuses.len(), 2, "{section}");
    for (at, _) in focuses {
        let before = &section[at.saturating_sub(4)..at];
        assert!(
            before.ends_with("tab") || before.ends_with("next"),
            "{before}"
        );
    }
    // The bar and the weight say which is shown, and it is the same with a script or without.
    assert!(CSS
        .contains(".dashboard-tab[aria-current=\"page\"], .dashboard-tab[aria-selected=\"true\"]"));
}
