//! Ledger #3811: gonk's pages ride out a restart or a short outage instead of showing an error
//! at once (`src/ui_retry.rs`).
//!
//! - one setting, `gonk.ui.retry.limit` and `gonk.ui.retry.base`, written on the `<html>` of
//!   gonk's own pages and of a framed face as `data-ui-retry-limit` and `data-ui-retry-base`
//!   (milliseconds), which ikigai-ui 0.5.1's shell reads; a malformed or unknown value stops the
//!   server at start;
//! - a `502`, `503` or `504` page a browser navigated to is marked to load itself again
//!   (`data-reload`), and a `500` page is not.
//!
//! The panel's read that is asked again is `src/dashboard.rs`'s
//! `a_chart_whose_counts_fail_once_draws_the_chart_not_the_note` and `src/ui_retry.rs`'s tests.
//! ⚠ There is no DOM in `cargo test`, so the script's half (the reload, its backoff and its
//! sentence in `#status`) is pinned as WIRING here.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{Description, EndpointSpace, Error, Exact, FnEndpoint, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, config, doors, quic, refusal, ui_retry, web};
use ikigai_store::DurableStore;

// ------------------------------------------------------------------ the setting, end to end

struct Gonk {
    child: Child,
    port: u16,
    host: String,
}

impl Drop for Gonk {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write `config` as the machine's config file under a fresh home, with a face named `ops`.
fn home_with(config: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let ops = home.path().join("ops");
    std::fs::create_dir(&ops).unwrap();
    std::fs::write(
        ops.join("index.html"),
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Ops</title>\
         </head><body><main><h1>Ops</h1></main></body></html>\n",
    )
    .unwrap();
    let dir = home.path().join("c").join("ikigai");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!("gonk.face = \"ops ~/ops\"\n{config}"),
    )
    .unwrap();
    home
}

fn command(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"));
    command
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("c"))
        .args(["--port", "0", "--socket", "g.sock"])
        .stdin(Stdio::null());
    command
}

/// Start gonk on `home`, and wait for its banner's `http` line.
fn start(home: &std::path::Path) -> Gonk {
    let log = home.join("gonk.log");
    let file = std::fs::File::create(&log).unwrap();
    let mut child = command(home)
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let host = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.lines().any(|l| l.trim_start().starts_with("mount")) {
            if let Some(host) = text
                .lines()
                .find_map(|l| l.trim_start().strip_prefix("http "))
                .and_then(|rest| rest.trim_start().strip_prefix("http://"))
                .and_then(|rest| rest.split(['/', ' ']).next())
            {
                break host.to_string();
            }
        }
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            panic!("the server printed no banner:\n{text}");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let port = host.rsplit(':').next().unwrap().parse().unwrap();
    Gonk { child, port, host }
}

/// A GET with the `Host` the banner names: `(status, body)`.
fn get(gonk: &Gonk, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", gonk.port)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        gonk.host
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let status = raw
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_string()
    };
    (status, body)
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&rest[..n]);
        body = &rest[n + 2..];
    }
    out
}

/// The page's `<html …>` start tag.
fn html_tag(page: &str) -> &str {
    let at = page
        .find("<html")
        .unwrap_or_else(|| panic!("no <html>:\n{page}"));
    let end = at + page[at..].find('>').unwrap();
    &page[at..=end]
}

/// ★ A gonk page and a framed face carry the configured values, in milliseconds, so they give
/// up at the same moment. Before, neither carried any, and ikigai-ui's shell used its own.
#[test]
fn a_gonk_page_and_a_framed_face_carry_the_configured_retry_settings() {
    let home = home_with("gonk.ui.retry.limit = \"45s\"\ngonk.ui.retry.base = \"250ms\"\n");
    let gonk = start(home.path());
    let ours = "data-ui-retry-limit='45000' data-ui-retry-base='250'";

    let (status, page) = get(&gonk, "/");
    assert_eq!(status, 200, "{page}");
    assert!(html_tag(&page).contains(ours), "{}", html_tag(&page));

    let (status, face) = get(&gonk, "/face/name/ops/index.html");
    assert_eq!(status, 200, "{face}");
    assert!(
        face.contains("id=\"gonk-frame\""),
        "the face is framed: {face}"
    );
    assert!(html_tag(&face).contains(ours), "{}", html_tag(&face));
}

/// With no line, every page carries ikigai-ui's own defaults, stated.
#[test]
fn with_no_line_a_page_carries_the_defaults() {
    let home = home_with("");
    let gonk = start(home.path());
    let (_, page) = get(&gonk, "/");
    assert!(
        html_tag(&page).contains("data-ui-retry-limit='90000' data-ui-retry-base='1000'"),
        "{}",
        html_tag(&page)
    );
}

/// ★ A value that is not a duration stops the server at start, naming the key, as every key's
/// does: a misspelled setting that silently did nothing would look like one in effect.
#[test]
fn a_malformed_retry_value_stops_the_server_at_start() {
    let home = home_with("gonk.ui.retry.limit = \"soon\"\n");
    let out = command(home.path()).output().unwrap();
    assert!(!out.status.success(), "it served");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains(ui_retry::LIMIT_KEY), "{said}");
}

#[test]
fn an_unknown_or_impossible_retry_line_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let homes = config::Homes {
        home: dir.path().to_path_buf(),
        config: dir.path().join("config"),
        data: dir.path().join("data"),
    };
    let settings = |text: &str| config::settings(&config::Flags::default(), text, &homes);

    let set = settings("gonk.ui.retry.limit = \"2m\"\ngonk.ui.retry.base = \"500ms\"\n").unwrap();
    assert_eq!(set.ui_retry.limit, Duration::from_secs(120));
    assert_eq!(set.ui_retry.base, Duration::from_millis(500));
    assert_eq!(
        settings("").unwrap().ui_retry,
        ui_retry::Settings::default()
    );
    let off = settings("gonk.ui.retry.limit = \"off\"\n").unwrap();
    assert_eq!(off.ui_retry.limit, Duration::ZERO);

    for (text, names) in [
        ("gonk.ui.retry.limt = \"90s\"\n", "gonk.ui.retry.limt"),
        ("gonk.ui.retry.limit = \"-90s\"\n", ui_retry::LIMIT_KEY),
        ("gonk.ui.retry.limit = \"2h\"\n", ui_retry::LIMIT_KEY),
        ("gonk.ui.retry.base = \"0s\"\n", ui_retry::BASE_KEY),
        ("gonk.ui.retry.base = \"quick\"\n", ui_retry::BASE_KEY),
        (
            "gonk.ui.retry.limit = \"5s\"\ngonk.ui.retry.base = \"10s\"\n",
            ui_retry::BASE_KEY,
        ),
    ] {
        let refused = settings(text).unwrap_err();
        assert!(refused.contains(names), "{text}: {refused}");
    }
}

// ------------------------------------------------------------------ the error pages

const NAVIGATE: &[(&str, &str)] = &[
    (
        "Accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    ),
    ("Sec-Fetch-Mode", "navigate"),
    ("Sec-Fetch-Site", "none"),
    ("Sec-Fetch-Dest", "document"),
];

/// A door whose front page answers `Unavailable` (503), whose ledgers page answers `Timeout`
/// (504), and whose dashboard answers an internal failure (500), with gonk's error page
/// installed as `main` installs it.
fn failing_door() -> SocketAddr {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let keys = Arc::new(Passkeys::new(layout, addr.port()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new().into(),
        passkeys: Arc::clone(&keys),
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
    let failing = |name: &'static str, error: fn() -> Error| {
        FnEndpoint::new(name, move |_| Err(error()))
            .with_description(Description::new(name).verb(Verb::Source))
    };
    let pages = EndpointSpace::new()
        .bind(
            Exact::new("urn:iki:gonk:page:home"),
            failing("down", || {
                Error::Unavailable("the store is restarting".into())
            }),
        )
        .bind(
            Exact::new(web::LEDGERS_IRI),
            failing("slow", || Error::Timeout("the store did not answer".into())),
        )
        .bind(
            Exact::new(ikigai_gonk::dashboard::PAGE_IRI),
            failing("broken", || Error::Endpoint("a bug".into())),
        );
    let http = Arc::new(doors::http_kernel(Arc::clone(&hub), pages));
    let door = doors::HttpDoor {
        anonymous: grants_for("default", Authority::Read).unwrap(),
        bind: addr,
        passkeys: Some(keys),
        public: Vec::new(),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    };
    std::thread::spawn(move || {
        let _config = config;
        let edge = refusal::install(doors::edge_config(door.clone()), door.clone(), face);
        runtime.block_on(ikigai_web::serve_with_listener(
            http,
            doors::http_cap(door),
            listener,
            edge,
        ))
    });
    addr
}

fn navigate(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let mut head = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n",
        addr.port()
    );
    for (k, v) in NAVIGATE {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = raw.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (status, dechunk_if(body))
}

fn dechunk_if(body: &str) -> String {
    // The door answers an error page whole, with a Content-Length; a chunked one is dechunked.
    if body.starts_with('<') {
        body.to_string()
    } else {
        dechunk(body)
    }
}

/// The refusal's `<section …>` start tag.
fn section(page: &str) -> &str {
    let at = page
        .find("data-refusal=")
        .unwrap_or_else(|| panic!("no refusal section:\n{page}"));
    let start = page[..at].rfind('<').unwrap();
    let end = at + page[at..].find('>').unwrap();
    &page[start..=end]
}

/// ★ A 503 and a 504 a browser navigated to are gonk's page, marked to load itself again, on a
/// page that carries the retry settings and the script that reads them. Before, the page stayed
/// until the person reloaded it.
#[test]
fn a_503_or_504_page_reloads_itself_and_a_500_page_does_not() {
    let addr = failing_door();
    for (path, code) in [("/", 503), (web::LEDGERS_PATH, 504)] {
        let (status, page) = navigate(addr, path);
        assert_eq!(status, code, "the status is the library's: {page}");
        let tag = section(&page);
        assert!(tag.contains("data-reload='true'"), "{path}: {tag}");
        assert!(tag.contains("data-retry-after-ms='0'"), "{path}: {tag}");
        assert!(page.contains("data-ui-retry-limit='90000'"), "{page}");
        assert!(page.contains("src='/static/gonk.js'"), "{page}");
        assert!(
            page.contains("id='status'"),
            "the status region it speaks in: {page}"
        );
    }

    let (status, page) = navigate(addr, ikigai_gonk::dashboard::PAGE_PATH);
    assert_eq!(status, 500, "{page}");
    let tag = section(&page);
    assert!(!tag.contains("data-reload"), "a 500 does not reload: {tag}");
}

/// The script's half, as wiring: it reads the marker and the two attributes, keeps its state
/// per address, says it is waiting in `#status`, offers a way to stop, and loads the page again
/// only by `location.reload()` of a page the server marked, which is only ever a GET's.
#[test]
fn the_script_reloads_a_marked_page_with_the_shells_backoff() {
    let script = include_str!("../web/gonk.js");
    let at = script
        .find("function reloadAfterOutage()")
        .expect("the reload is wired");
    let body = &script[at..at + script[at..].find("\n  }\n").unwrap()];
    for wiring in [
        "[data-refusal][data-reload]",
        "data-ui-retry-limit",
        "data-ui-retry-base",
        "data-retry-after-ms",
        "sessionStorage",
        "Math.random() * ceiling",
        "Math.min(OUTAGE_CAP_MS, base * Math.pow(2, state.attempt))",
        "flash(",
        "Stop trying",
        "location.reload()",
    ] {
        assert!(body.contains(wiring), "{wiring}");
    }
    assert!(!body.contains("fetch("), "it sends nothing of its own");
    assert!(
        script.contains("    focusRefusal();\n    reloadAfterOutage();"),
        "run at every page's start"
    );
}
