//! A deployment's own stylesheet and script on gonk's pages (ledger
//! [#3379](http://localhost:1062/l/default/item/3379)), driven over real HTTP the way a browser
//! drives it: `gonk.page.style` and `gonk.page.script` lines are linked last in the shared head of
//! every covered page (the home page, a ledger list, an item, the ledgers index, a sign-in
//! refusal, a not-found page, an edge refusal and the bar around a face), are absent in safe mode
//! (`?safe=1`), are named as the deployment's in the head and the footer, and gonk's own comment
//! order control works without them.
//!
//! ⚠ There is no DOM in `cargo test`, so that a THROWING deployment script leaves gonk's own
//! controls working is pinned here as wiring (gonk's script deferred ahead of the deployment's)
//! and checked in a real browser by the `#[ignore]`d test at the bottom, which needs
//! `GONK_CHROMIUM` (a headless Chromium) to run.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::face::{self, Face};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::page_assets::{PageAssets, STABLE_HOOKS};
use ikigai_gonk::{compose, config, doors, quic, refusal, web};
use ikigai_store::DurableStore;

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

const STYLE: &str = "/face/name/site/site.css";
const SCRIPT: &str = "/face/name/site/site.js";

/// A deployment script that throws as soon as it runs, after first marking that it ran.
const THROWING: &str = "document.documentElement.setAttribute('data-site-ran', 'yes');\n\
                        throw new Error('the deployment script is broken');\n";

/// A second deployment script, run after the throwing one: it presses gonk's own "newest first".
const AFTER: &str = "document.querySelector('#comment-order a[data-order=newest]').click();\n\
                     document.documentElement.setAttribute('data-after-ran', 'yes');\n";

/// A face page with a `<body>`, which gonk frames.
const FRAMED: &str =
    "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Site</title>\
    <link rel=\"stylesheet\" href=\"own.css\"></head><body><p>The face's own</p></body></html>\n";

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    _dirs: Vec<tempfile::TempDir>,
}

struct Response {
    status: u16,
    head: String,
    body: String,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.head, self.body)
    }
}

fn build(dir: &Path) {
    std::fs::write(dir.join("site.css"), "body { outline: 1px solid; }\n").unwrap();
    std::fs::write(dir.join("site.js"), THROWING).unwrap();
    std::fs::write(dir.join("after.js"), AFTER).unwrap();
    // A page that asks this browser for safe mode, as the footer's toggle does.
    std::fs::write(
        dir.join("ask-safe.js"),
        "localStorage.setItem('gonk:safe', '1');\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ask-safe.html"),
        "<!doctype html><title>x</title><script src=\"ask-safe.js\"></script>\n",
    )
    .unwrap();
    std::fs::write(dir.join("framed.html"), FRAMED).unwrap();
    std::fs::write(dir.join("own.css"), "p { margin: 0; }\n").unwrap();
}

impl Server {
    /// A server whose anonymous loopback caller reads the `default` ledger, with the face `site`
    /// and gonk's error page installed as `main` installs them, and `assets` on its pages.
    fn start(assets: PageAssets) -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let dist = tempfile::tempdir().unwrap();
        build(dist.path());
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let keys = Arc::new(Passkeys::new(layout.clone(), addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&keys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            holder_aliases: Default::default(),
            page_assets: assets.clone(),
            toolbox: false,
            faces: vec!["site".to_string()],
        });
        let faces = vec![Face::at("site", dist.path().to_path_buf()).unwrap()];
        let public: Vec<String> = faces.iter().flat_map(Face::grants).collect();
        let http = Arc::new(doors::http_kernel_serving(
            Arc::clone(&hub),
            web::space(Arc::clone(&face)),
            face::spaces_with(&faces, &assets),
            None,
        ));
        let door = doors::HttpDoor {
            anonymous: grants_for("default", Authority::Read).unwrap(),
            bind: addr,
            passkeys: Some(keys),
            public,
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        std::thread::spawn(move || {
            let edge = face::route(doors::edge_config(door.clone()));
            let edge = refusal::install(edge, door.clone(), face);
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door),
                listener,
                edge,
            ))
        });
        Server {
            addr,
            hub,
            _dirs: vec![config, dist],
        }
    }

    fn raw(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
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
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        // The stylesheet's HTML writer single-quotes attributes; these assertions use double.
        Response {
            status,
            head: head.to_string(),
            body: body.replace('\'', "\""),
        }
    }

    /// `path` as a browser navigating to it.
    fn navigate(&self, path: &str) -> Response {
        self.raw(
            "GET",
            path,
            &[
                ("Accept", CHROME_ACCEPT),
                ("Sec-Fetch-Mode", "navigate"),
                ("Sec-Fetch-Site", "none"),
                ("Sec-Fetch-Dest", "document"),
            ],
            "",
        )
    }

    /// A form on another site posting here: an EDGE refusal, drawn with no capability.
    fn cross_site_post(&self, query: &str) -> Response {
        self.raw(
            "POST",
            &format!("/act{query}"),
            &[
                ("Accept", CHROME_ACCEPT),
                ("Sec-Fetch-Mode", "navigate"),
                ("Sec-Fetch-Site", "cross-site"),
                ("Sec-Fetch-Dest", "document"),
                ("Origin", "https://elsewhere.example"),
                ("Content-Type", "application/x-www-form-urlencoded"),
            ],
            "target=urn%3Aiki%3Aledger%3Aappend",
        )
    }

    fn root(&self, iri: &str, args: &[(&str, &str)]) {
        let mut request = Request::new(Verb::Sink, Iri::parse(iri).unwrap());
        for (name, value) in args {
            request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        futures::executor::block_on(Kernel::issue(&self.hub, request, &Capability::root()))
            .expect(iri);
    }

    /// Item 1 in `default`, with two comments written in order.
    fn item_with_comments(&self) {
        self.root("urn:iki:ledger:append", &[("content", "An item to read")]);
        self.root(
            "urn:iki:ledger:comment",
            &[("item", "1"), ("content", "the first comment")],
        );
        std::thread::sleep(std::time::Duration::from_millis(1100));
        self.root(
            "urn:iki:ledger:comment",
            &[("item", "1"), ("content", "the second comment")],
        );
    }
}

fn lines(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn both() -> PageAssets {
    PageAssets::parse(&lines(&[STYLE]), &lines(&[SCRIPT])).unwrap()
}

/// Every covered page, as `(what, response)`, with `query` (`""` or `"?safe=1"`) on each.
fn covered(server: &Server, query: &str) -> Vec<(&'static str, Response)> {
    let joined = |path: &str| format!("{path}{query}");
    let pages = vec![
        ("the home page", server.navigate(&joined("/"))),
        ("the ledgers index", server.navigate(&joined("/l"))),
        ("a ledger list", server.navigate(&joined("/l/default"))),
        (
            "an item page",
            server.navigate(&joined("/l/default/item/1")),
        ),
        ("the SPARQL page", server.navigate(&joined("/sparql"))),
        ("a sign-in refusal", server.navigate(&joined("/l/acme"))),
        (
            "a not-found page",
            server.navigate(&joined("/l/default/item/987654")),
        ),
        ("an edge refusal", server.cross_site_post(query)),
        (
            "a face's page in gonk's bar",
            server.navigate(&joined("/face/name/site/framed.html")),
        ),
    ];
    for (what, page) in &pages {
        assert!(
            page.head.contains("text/html"),
            "{what} is a page: {page:?}"
        );
    }
    pages
}

fn head(page: &str) -> &str {
    &page[..page.find("</head>").expect("a head")]
}

/// ★ The reproduction: with `gonk.page.style` and `gonk.page.script`, every page drawn through
/// gonk's shared head links the stylesheet LAST in its head and the script DEFERRED after
/// gonk.js, under a comment naming the config line.
#[test]
fn the_deployments_assets_are_on_every_covered_page() {
    let server = Server::start(both());
    server.item_with_comments();
    for (what, page) in covered(&server, "") {
        let body = &page.body;
        let head = head(body);
        let style = head
            .find(&format!("href=\"{STYLE}\""))
            .unwrap_or_else(|| panic!("{what} links the deployment's stylesheet: {page:?}"));
        let script = head
            .find(&format!("src=\"{SCRIPT}\""))
            .unwrap_or_else(|| panic!("{what} links the deployment's script: {page:?}"));
        assert!(
            head[..style].contains("gonk.page.style"),
            "{what}: a comment names the config line: {head}"
        );
        let tag =
            |at: usize| &head[head[..at].rfind('<').unwrap()..at + head[at..].find('>').unwrap()];
        assert!(
            tag(style).contains("rel=\"stylesheet\""),
            "{what}: {}",
            tag(style)
        );
        assert!(
            tag(style).contains("data-gonk-deployment=\"style\""),
            "{what}: {}",
            tag(style)
        );
        assert!(
            tag(script).contains("defer"),
            "{what}: the script is deferred: {}",
            tag(script)
        );
        assert!(
            tag(script).contains("data-gonk-deployment=\"script\""),
            "{what}: {}",
            tag(script)
        );
        // The gate that honors the per-browser toggle comes first, and blocks.
        let gate = head
            .find("/static/gonk-safe.js")
            .expect("the safe-mode gate");
        assert!(gate < style && gate < script, "{what}: {head}");
        assert!(!tag(gate).contains("defer"), "{what}: {}", tag(gate));
        if what.starts_with("a face") {
            // On a face, after gonk's frame and before the face's own sheet: the face keeps
            // the last word on its own page.
            assert!(head.find("gonk-frame.css").unwrap() < style, "{head}");
            assert!(style < head.find("own.css").unwrap(), "{head}");
            assert!(
                body.contains("href=\"?safe=1\">Safe mode</a>"),
                "{what}: {body}"
            );
            continue;
        }
        // Last in the head: after every sheet and script of gonk's own.
        for own in [
            "/static/tokens.css",
            "/static/gonk.css",
            "/static/shell.js",
            "/static/htmx.min.js",
            "/static/gonk.js",
        ] {
            let at = head
                .find(own)
                .unwrap_or_else(|| panic!("{what} links {own}"));
            assert!(
                at < style && at < script,
                "{what}: {own} comes first: {head}"
            );
        }
        let after = &head[style..];
        assert!(
            !after[1..].contains("<link"),
            "{what}: the stylesheet is last: {head}"
        );
        // And the footer says whose they are, with safe mode one link away.
        assert!(body.contains("id=\"deployment-note\""), "{what}: {body}");
        assert!(
            body.contains("This deployment adds its own style and script to gonk"),
            "{what}: {body}"
        );
        assert!(
            body.contains("href=\"?safe=1\">load this page in safe mode</a>"),
            "{what}: {body}"
        );
        assert!(body.contains("id=\"safe-toggle\""), "{what}: {body}");
    }
}

/// ★ Safe mode: the same pages with `?safe=1` link neither the deployment's assets nor the gate,
/// and say they are in safe mode. Refusals and error pages say it too.
#[test]
fn safe_mode_leaves_the_deployments_assets_out() {
    let server = Server::start(both());
    server.item_with_comments();
    for (what, page) in covered(&server, "?safe=1") {
        let body = &page.body;
        assert!(!body.contains(STYLE), "{what} in safe mode: {page:?}");
        assert!(!body.contains(SCRIPT), "{what} in safe mode: {page:?}");
        assert!(!body.contains("data-gonk-deployment"), "{what}: {page:?}");
        assert!(!body.contains("gonk-safe.js"), "{what}: {page:?}");
        if what.starts_with("a face") {
            assert!(
                body.contains("<span class=\"gonk-frame-safe\">Safe mode: without this deployment"),
                "{what}: {body}"
            );
            assert!(
                body.contains("own style and script</span>"),
                "{what}: {body}"
            );
            assert!(
                body.contains("own.css"),
                "the face's own sheet stays: {body}"
            );
            continue;
        }
        assert!(
            body.contains("/static/gonk.js"),
            "{what} is still gonk's page: {body}"
        );
        assert!(
            body.contains("<strong>Safe mode</strong>"),
            "{what}: {body}"
        );
        assert!(body.contains("class=\"safe-leave\""), "{what}: {body}");
    }
    // Any other value of the flag is not safe mode.
    let page = server.navigate("/l/default?safe=yes");
    assert!(page.body.contains(STYLE), "{page:?}");
}

/// With no `gonk.page.*` line nothing changes: no gate, no deployment link, no footer.
#[test]
fn with_no_assets_the_pages_are_as_they_were() {
    let server = Server::start(PageAssets::default());
    server.item_with_comments();
    for (what, page) in covered(&server, "") {
        assert!(!page.body.contains("gonk-safe.js"), "{what}: {page:?}");
        assert!(
            !page.body.contains("data-gonk-deployment"),
            "{what}: {page:?}"
        );
        assert!(!page.body.contains("deployment-note"), "{what}: {page:?}");
        assert!(!page.body.contains("Safe mode"), "{what}: {page:?}");
    }
}

/// A stylesheet alone, or a script alone, is named for what it is.
#[test]
fn the_footer_names_only_what_the_deployment_adds() {
    let server = Server::start(PageAssets::parse(&lines(&[STYLE]), &[]).unwrap());
    let page = server.navigate("/l/default");
    assert!(
        page.body
            .contains("This deployment adds its own style to gonk"),
        "{page:?}"
    );
    assert!(!page.body.contains("gonk.page.script"), "{page:?}");
    assert!(!page.body.contains(SCRIPT), "{page:?}");
}

/// ★ Isolation, as wiring: gonk's own script is deferred ahead of every deployment script, so its
/// listeners are in place before one runs, and the gate holds no listener a page relies on.
#[test]
fn gonks_script_runs_before_the_deployments() {
    let server = Server::start(both());
    let page = server.navigate("/l/default");
    let head = head(&page.body);
    assert!(head.find("/static/gonk.js").unwrap() < head.find(SCRIPT).unwrap());
    let gate = server.raw("GET", "/static/gonk-safe.js", &[], "");
    assert_eq!(gate.status, 200, "{gate:?}");
    assert!(gate.head.contains("text/javascript"), "{gate:?}");
    assert!(!gate.body.contains("addEventListener"), "{gate:?}");
}

/// ★ gonk's own comment order control: oldest first by default, newest first when asked, with
/// the current order marked in `aria-current` and plain links that work with no script.
#[test]
fn an_items_comments_show_oldest_or_newest_first() {
    let server = Server::start(PageAssets::default());
    server.item_with_comments();
    let order = |page: &Response| {
        let first = page
            .body
            .find("the first comment")
            .expect("the first comment");
        let second = page
            .body
            .find("the second comment")
            .expect("the second comment");
        first < second
    };
    let oldest = server.navigate("/l/default/item/1");
    assert_eq!(oldest.status, 200, "{oldest:?}");
    assert!(order(&oldest), "written order by default: {oldest:?}");
    assert!(oldest.body.contains("id=\"comment-order\""), "{oldest:?}");
    assert!(
        oldest
            .body
            .contains("<a aria-current=\"true\" data-order=\"oldest\" href=\"?comments=oldest\">")
            || oldest
                .body
                .contains("data-order=\"oldest\" href=\"?comments=oldest\" aria-current=\"true\""),
        "{oldest:?}"
    );
    let newest = server.navigate("/l/default/item/1?comments=newest");
    assert!(!order(&newest), "newest first when asked: {newest:?}");
    assert!(newest.body.contains("data-order=\"newest\""), "{newest:?}");
    let control = &newest.body[newest.body.find("id=\"comment-order\"").unwrap()..];
    let control = &control[..control.find("</nav>").unwrap()];
    assert_eq!(control.matches("aria-current").count(), 1, "{control}");
    let current = control.find("aria-current").unwrap();
    assert!(
        control[current..].starts_with("aria-current=\"true\" data-order=\"newest\"")
            || control[..current].rfind("data-order=\"newest\"")
                > control[..current].rfind("data-order=\"oldest\""),
        "{control}"
    );

    // One comment has no order to choose.
    server.root("urn:iki:ledger:append", &[("content", "A quiet item")]);
    server.root(
        "urn:iki:ledger:comment",
        &[("item", "2"), ("content", "alone")],
    );
    let single = server.navigate("/l/default/item/2");
    assert!(!single.body.contains("comment-order"), "{single:?}");
}

/// The small, named set of hooks a deployment may rely on is on the item page, as documented.
#[test]
fn the_stable_hooks_are_on_the_item_page() {
    let server = Server::start(both());
    server.item_with_comments();
    let page = server.navigate("/l/default/item/1").body;
    for hook in STABLE_HOOKS {
        let found = if let Some(id) = hook.strip_prefix('#') {
            page.contains(&format!("id=\"{id}\""))
        } else if let Some((element, class)) = hook.split_once('.') {
            let classed = |at: usize| {
                let tag = &page[at..at + page[at..].find('>').unwrap()];
                tag.split("class=\"").nth(1).is_some_and(|rest| {
                    rest.split('"')
                        .next()
                        .unwrap()
                        .split(' ')
                        .any(|c| c == class)
                })
            };
            page.match_indices(&format!(
                "<{}",
                if element.is_empty() { "" } else { element }
            ))
            .any(|(at, _)| classed(at))
        } else {
            false
        };
        assert!(found, "{hook} is on the item page: {page}");
    }
    let readme = include_str!("../README.md");
    for hook in STABLE_HOOKS {
        assert!(
            readme.contains(&format!("`{hook}`")),
            "the README documents {hook}"
        );
    }
}

/// ★ The config: each key repeats, and a value that is not a path on this server fails at start,
/// naming the key.
#[test]
fn a_page_asset_line_is_checked_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let homes = config::Homes {
        home: dir.path().to_path_buf(),
        config: dir.path().join("config"),
        data: dir.path().join("data"),
    };
    let settings = |text: &str| config::settings(&config::Flags::default(), text, &homes);
    assert!(settings("").unwrap().page_assets.is_empty());
    let configured = settings(
        "gonk.page.style = \"/face/name/ops/a.css\"\ngonk.page.style = \"/face/name/ops/b.css\"\n\
         gonk.page.script = \"/face/name/ops/site.js\"\n",
    )
    .unwrap();
    assert_eq!(
        configured.page_assets.styles(),
        ["/face/name/ops/a.css", "/face/name/ops/b.css"]
    );
    assert_eq!(configured.page_assets.scripts(), ["/face/name/ops/site.js"]);
    for bad in [
        "https://cdn.example/site.css",
        "//cdn.example/site.css",
        "site.css",
    ] {
        let refused = settings(&format!("gonk.page.style = \"{bad}\"\n")).unwrap_err();
        assert!(refused.contains("gonk.page.style"), "{refused}");
    }
    let refused = settings("gonk.page.script = \"/a b.js\"\n").unwrap_err();
    assert!(refused.contains("gonk.page.script"), "{refused}");
}

/// In a real browser: a deployment script that THROWS leaves gonk's own controls working (the
/// header's ledger menu becomes gonk.js's disclosure button, the comment order control
/// reorders), and safe mode does not run it at all. Needs a headless Chromium:
/// `GONK_CHROMIUM=/path/to/chrome cargo test --test page_assets_3379 -- --ignored`.
#[test]
#[ignore = "needs a headless Chromium in GONK_CHROMIUM"]
fn a_throwing_deployment_script_leaves_gonks_controls_working() {
    let chrome = std::env::var("GONK_CHROMIUM").expect("GONK_CHROMIUM names a Chromium");
    let after = "/face/name/site/after.js";
    let server =
        Server::start(PageAssets::parse(&lines(&[STYLE]), &lines(&[SCRIPT, after])).unwrap());
    server.item_with_comments();
    let browser = |profile: &Path, path: &str| {
        let out = std::process::Command::new(&chrome)
            .args([
                "--headless",
                "--no-sandbox",
                "--disable-gpu",
                &format!("--user-data-dir={}", profile.display()),
                "--virtual-time-budget=5000",
                "--dump-dom",
                &format!("http://localhost:{}{path}", server.addr.port()),
            ])
            .output()
            .expect("run Chromium");
        String::from_utf8_lossy(&out.stdout).replace('\'', "\"")
    };
    let dom = |path: &str| browser(tempfile::tempdir().unwrap().path(), path);
    let page = dom("/l/default/item/1");
    assert!(
        page.contains("data-site-ran=\"yes\""),
        "the deployment script ran and threw: {page}"
    );
    assert!(
        page.contains("data-after-ran=\"yes\""),
        "the next one ran: {page}"
    );
    // gonk.js's own work: the safe-mode link carries the page's address, the toggle shows...
    let port = server.addr.port();
    assert!(
        page.contains(&format!(
            "href=\"http://localhost:{port}/l/default/item/1?safe=1\""
        )),
        "gonk.js wired the footer: {page}"
    );
    let toggle = &page[page.find("id=\"safe-toggle\"").expect("the toggle")..];
    let toggle = &toggle[..toggle.find('>').unwrap()];
    assert!(
        !toggle.contains("hidden"),
        "gonk.js showed the toggle: {toggle}"
    );
    // ...and its comment order control, pressed after the throw, reordered the list in place.
    let list = &page[page.find("comment-list").expect("the comments")..];
    assert!(list.contains("data-order=\"newest\""), "{list}");
    assert!(
        list.find("the second comment").unwrap() < list.find("the first comment").unwrap(),
        "newest first: {list}"
    );
    let control = &page[page.find("id=\"comment-order\"").unwrap()..];
    let control = &control[..control.find("</nav>").unwrap()];
    let current = control.find("aria-current").expect("one link is current");
    assert!(
        control[current..].contains("newest first") && !control[current..].contains("oldest first"),
        "{control}"
    );

    let safe = dom("/l/default/item/1?safe=1");
    assert!(
        !safe.contains("data-site-ran"),
        "safe mode ran no deployment script: {safe}"
    );
    assert!(!safe.contains("data-after-ran"), "{safe}");
    assert!(safe.contains("<strong>Safe mode</strong>"), "{safe}");

    // The per-browser toggle: once this browser has asked, a page asked for WITHOUT the flag
    // is stopped by the gate and drawn again in safe mode, the deployment's script never run.
    let profile = tempfile::tempdir().unwrap();
    browser(profile.path(), "/face/name/site/ask-safe.html");
    let toggled = browser(profile.path(), "/l/default/item/1");
    assert!(toggled.contains("<strong>Safe mode</strong>"), "{toggled}");
    assert!(!toggled.contains("data-site-ran"), "{toggled}");
    assert!(
        toggled.contains("Stop using safe mode in this browser"),
        "{toggled}"
    );
}
