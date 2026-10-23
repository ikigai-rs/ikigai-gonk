//! The page a browser gets when it NAVIGATES to an error (ledger
//! [#2021](http://localhost:1062/l/default/item/2021), `src/refusal.rs`), driven over real HTTP
//! the way a browser drives it: `Sec-Fetch-Mode: navigate` and Chrome's `Accept`.
//!
//! - signed out, refused a ledger the passkey grant would cover: gonk's page shell with the
//!   sign-in on it, `403`;
//! - signed in under a grant that does not reach it: the same shell, what the grant lacks, no
//!   sign-in, `403`;
//! - any other error a browser navigates to: the same shell with the status and the message,
//!   the status unchanged;
//! - curl, a script's `fetch` and htmx: the answer they got before (`ikigai-web`'s minimal
//!   page, or the sentence as text).
//!
//! ⚠ There is no DOM in `cargo test`, so the script's half (focus moving to the sign-in button
//! once it shows, and signing in reloading the page) is pinned as WIRING here.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

mod common;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::{compose, doors, quic, refusal, web};
use ikigai_store::DurableStore;

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

/// What a browser sends when a person types an address or follows a link.
const NAVIGATE: &[(&str, &str)] = &[
    ("Accept", CHROME_ACCEPT),
    ("Sec-Fetch-Mode", "navigate"),
    ("Sec-Fetch-Site", "none"),
    ("Sec-Fetch-Dest", "document"),
];

struct Server {
    addr: SocketAddr,
    layout: quic::Layout,
    _config: tempfile::TempDir,
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

impl Server {
    /// A server whose anonymous loopback caller reads the `default` ledger, with gonk's error
    /// page installed as `main` installs it. `passkeys` off is a server nobody can sign in to.
    fn start(passkeys: bool) -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
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
            toolbox: false,
        });
        let http = Arc::new(doors::http_kernel(
            Arc::clone(&hub),
            web::space(Arc::clone(&face)),
        ));
        let door = doors::HttpDoor {
            anonymous: grants_for("default", Authority::Read).unwrap(),
            bind: addr,
            passkeys: passkeys.then_some(keys),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        std::thread::spawn(move || {
            let edge = refusal::install(doors::edge_config(door.clone()), door.clone(), face);
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door),
                listener,
                edge,
            ))
        });
        Server {
            addr,
            layout,
            _config: config,
        }
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    fn raw(&self, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> Response {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let mut head = format!("{method} {path} HTTP/1.1\r\n");
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            head.push_str(&format!("Host: localhost:{}\r\n", self.addr.port()));
        }
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
        // The stylesheet's HTML writer single-quotes attributes; the assertions here are
        // written with double quotes.
        Response {
            status,
            head: head.to_string(),
            body: body.replace('\'', "\""),
        }
    }

    /// `path` as a browser navigating to it, signed in as `cookie` or nobody.
    fn navigate(&self, path: &str, cookie: Option<&str>) -> Response {
        self.get(path, NAVIGATE, cookie)
    }

    fn get(&self, path: &str, headers: &[(&str, &str)], cookie: Option<&str>) -> Response {
        let mut headers: Vec<(&str, String)> =
            headers.iter().map(|(k, v)| (*k, v.to_string())).collect();
        if let Some(cookie) = cookie {
            headers.push(("Cookie", format!("{}={cookie}", identity::SESSION_COOKIE)));
        }
        self.raw("GET", path, &headers, "")
    }

    fn json(&self, path: &str, body: &str) -> Response {
        self.raw(
            "POST",
            path,
            &[
                ("Accept", "application/json".to_string()),
                ("Content-Type", "application/json".to_string()),
                ("Origin", self.origin()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
            ],
            body,
        )
    }

    /// Enroll a passkey under `grant` holding `scopes`, sign it in, and return the session.
    fn sign_in(&self, grant: &str, scopes: &[String]) -> String {
        let authenticator = Authenticator::new();
        let invite = identity::invite(
            &self.layout,
            grant,
            scopes,
            false,
            30,
            identity::now_seconds(),
        )
        .unwrap();
        let c = self.challenge("register-options");
        let enrolled = self.json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(enrolled.status, 200, "{enrolled:?}");
        let c = self.challenge("login-options");
        let login = self.json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        );
        assert_eq!(login.status, 200, "{login:?}");
        let v: serde_json::Value = serde_json::from_str(&login.body).unwrap();
        v["session"].as_str().unwrap().to_string()
    }

    fn challenge(&self, op: &str) -> String {
        let got = self.json(&format!("/auth/{op}"), "{}");
        assert_eq!(got.status, 200, "{got:?}");
        let v: serde_json::Value = serde_json::from_str(&got.body).unwrap();
        v["challenge"].as_str().unwrap().to_string()
    }
}

/// The refusal's own section of a page: from its opening tag to the end of the section. (The
/// HTML writer sorts attributes by name, so the tag is found by its marker.)
fn section(page: &str) -> &str {
    let marker = page
        .find("data-refusal=\"true\"")
        .unwrap_or_else(|| panic!("no refusal section: {page}"));
    let at = page[..marker].rfind("<section").expect("the section's tag");
    let rest = &page[at..];
    &rest[..rest.find("</section>").expect("a closed section")]
}

/// The header, from `<header` to `</header>`.
fn header(page: &str) -> &str {
    let at = page
        .find("<header")
        .unwrap_or_else(|| panic!("no header: {page}"));
    let rest = &page[at..];
    &rest[..rest.find("</header>").expect("a closed header")]
}

/// Whether `response` is gonk's page shell: its stylesheet, its script and its sign-in header.
fn is_gonk_shell(response: &Response) -> bool {
    response.body.starts_with("<!DOCTYPE html>")
        && response.body.contains("<html lang=\"en\">")
        && response.body.contains("href=\"/static/gonk.css\"")
        && response.body.contains("src=\"/static/gonk.js\"")
        && response.body.contains("id=\"auth-login\"")
        && response.body.contains("<main id=\"main\">")
}

/// A response header's value, by name.
fn header_value<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.head.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then_some(v.trim())
    })
}

/// ★ Signed out, refused a ledger a passkey grant would cover: gonk's own page, `403`, with the
/// "not signed in" sentence and the passkey sign-in ON the page, and the header the anonymous
/// grant draws everywhere else. No capability IRI is the first thing anybody reads.
#[test]
fn a_signed_out_navigation_to_a_refused_ledger_is_the_shell_with_the_sign_in() {
    let server = Server::start(true);
    let refused = server.navigate("/l/acme", None);
    assert_eq!(refused.status, 403, "the status is unchanged: {refused:?}");
    assert!(is_gonk_shell(&refused), "{refused:?}");
    assert_eq!(
        header_value(&refused, "content-type"),
        Some("text/html; charset=utf-8"),
        "{refused:?}"
    );
    // The door's CSP governs it, and nothing on it is inline.
    assert!(
        header_value(&refused, "content-security-policy").is_some(),
        "{refused:?}"
    );
    assert!(!refused.body.contains("<style"), "{refused:?}");
    assert!(!refused.body.contains("<script>"), "{refused:?}");
    assert!(!refused.body.contains(" style="), "{refused:?}");
    assert!(
        refused.body.contains(&format!(
            "<title>{} · gonk</title>",
            refusal::SIGN_IN_HEADING
        )),
        "{refused:?}"
    );

    let section = section(&refused.body);
    assert!(section.contains("data-kind=\"signin\""), "{section}");
    assert!(
        section.contains("Sign in to read the ledger &quot;acme&quot;."),
        "{section}"
    );
    // The plain refusal's "where is the sign-in" is the page's button, not a sentence.
    assert!(!section.contains("then come back"), "{section}");
    assert!(
        section.contains(
            "<button aria-describedby=\"refusal-sentence\" class=\"signin-here\" hidden=\"hidden\" type=\"button\">Sign in with passkey</button>"
        ),
        "{section}"
    );
    assert!(!section.contains("urn:cap:"), "{section}");
    assert!(!section.contains("doesn&apos;t include"), "{section}");

    // The header is the one this caller's grant draws: the ledger it may read.
    let nav = header(&refused.body);
    assert!(nav.contains("/l/default"), "{nav}");
}

/// ★ Signed in, under a grant that does not reach the ledger: the same shell, `403`, what the
/// grant lacks with the grant LAST, and nothing offering a sign-in, which would cure nothing.
#[test]
fn a_signed_in_navigation_the_grant_lacks_says_so_and_offers_no_sign_in() {
    let server = Server::start(true);
    let token = server.sign_in("reader", &grants_for("default", Authority::Delete).unwrap());
    let lacking = server.navigate("/l/acme", Some(&token));
    assert_eq!(lacking.status, 403, "the status is unchanged: {lacking:?}");
    assert!(is_gonk_shell(&lacking), "{lacking:?}");
    assert!(
        lacking.body.contains(
            &format!("<title>{} · gonk</title>", refusal::GRANT_HEADING).replace('\'', "&apos;")
        ),
        "{lacking:?}"
    );
    let section = section(&lacking.body);
    assert!(section.contains("data-kind=\"grant\""), "{section}");
    assert!(
        section.contains(
            "Your passkey&apos;s grant doesn&apos;t include the ledger &quot;acme&quot;."
        ),
        "{section}"
    );
    assert!(!section.contains("signin-here"), "{section}");
    assert!(!section.contains("Sign in"), "{section}");
    let grant = section
        .find("The grant it needs: <code>urn:cap:ledger:read:acme")
        .unwrap_or_else(|| panic!("the grant it needs: {section}"));
    assert!(
        grant > section.find("refusal-sentence\">").unwrap(),
        "the grant comes LAST: {section}"
    );
}

/// Every other error a browser navigates to is the same shell, its status unchanged: a `404`.
#[test]
fn a_navigation_to_a_missing_page_is_the_shell_with_its_status() {
    let server = Server::start(true);
    let missing = server.navigate("/l/default/item/987654", None);
    assert_eq!(missing.status, 404, "{missing:?}");
    assert!(is_gonk_shell(&missing), "{missing:?}");
    assert!(
        missing.body.contains("<title>404 Not Found · gonk</title>"),
        "{missing:?}"
    );
    let section = section(&missing.body);
    assert!(section.contains("data-kind=\"error\""), "{section}");
    assert!(section.contains(">404 Not Found</h1>"), "{section}");
    assert!(section.contains("id=\"refusal-sentence\""), "{section}");
    assert!(!section.contains("signin-here"), "{section}");
}

/// A refusal the EDGE wrote before any authority existed (a form on another site posting here)
/// is no sign-in's to cure: the shell with the status, and no navigation drawn from anybody's
/// grant. A foreign `Host` keeps the library's page: the shell's stylesheet and script would be
/// fetched from that host, which is refused too.
#[test]
fn an_edge_refusal_is_the_shell_without_a_header_navigation() {
    let server = Server::start(true);
    let cross_site = server.raw(
        "POST",
        "/act",
        &[
            ("Accept", CHROME_ACCEPT.to_string()),
            ("Sec-Fetch-Mode", "navigate".to_string()),
            ("Sec-Fetch-Site", "cross-site".to_string()),
            ("Sec-Fetch-Dest", "document".to_string()),
            ("Origin", "https://elsewhere.example".to_string()),
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
        ],
        "target=urn%3Aiki%3Aledger%3Aappend",
    );
    assert_eq!(cross_site.status, 403, "{cross_site:?}");
    assert!(is_gonk_shell(&cross_site), "{cross_site:?}");
    let section = section(&cross_site.body);
    assert!(section.contains("data-kind=\"error\""), "{section}");
    assert!(!section.contains("signin-here"), "{section}");
    assert!(
        !header(&cross_site.body).contains("/l/default"),
        "{cross_site:?}"
    );

    let mut headers: Vec<(&str, &str)> = NAVIGATE.to_vec();
    headers.push(("Host", "attacker.example"));
    let foreign = server.get("/l/default", &headers, None);
    assert_eq!(foreign.status, 421, "{foreign:?}");
    assert!(!is_gonk_shell(&foreign), "{foreign:?}");
    assert!(
        foreign
            .body
            .contains("<main><h1>421 Misdirected Request</h1>"),
        "{foreign:?}"
    );
}

/// A server with no passkeys has no sign-in to offer: the refusal is the plain kind.
#[test]
fn without_passkeys_a_refusal_offers_no_sign_in() {
    let server = Server::start(false);
    let refused = server.navigate("/l/acme", None);
    assert_eq!(refused.status, 403, "{refused:?}");
    assert!(is_gonk_shell(&refused), "{refused:?}");
    let section = section(&refused.body);
    assert!(section.contains("data-kind=\"error\""), "{section}");
    assert!(!section.contains("signin-here"), "{section}");
}

/// curl, a script's `fetch` and htmx get exactly what they got before: the library's minimal
/// page for an `Accept` that asks for HTML, the sentence as text for one that does not, and
/// never gonk's shell.
#[test]
fn curl_a_fetch_and_htmx_keep_the_answer_they_had() {
    let server = Server::start(true);
    let minimal = |r: &Response| {
        r.body.contains("<main><h1>403 Forbidden</h1><p>")
            && !r.body.contains("gonk.css")
            && !r.body.contains("signin-here")
    };

    // curl asking for HTML: no fetch metadata, the minimal page.
    let curl = server.get("/l/acme", &[("Accept", "text/html")], None);
    assert_eq!(curl.status, 403, "{curl:?}");
    assert!(minimal(&curl), "{curl:?}");

    // curl's own `*/*`: the sentence as text, unchanged.
    let plain = server.get("/l/acme", &[("Accept", "*/*")], None);
    assert_eq!(plain.status, 403, "{plain:?}");
    assert!(
        header_value(&plain, "content-type").is_some_and(|t| t.starts_with("text/plain")),
        "{plain:?}"
    );
    assert!(
        plain.body.contains("Sign in to read the ledger"),
        "{plain:?}"
    );

    // A script's fetch from this page (not a navigation): the library's page.
    let fetch = server.get(
        "/l/acme",
        &[
            ("Accept", CHROME_ACCEPT),
            ("Sec-Fetch-Mode", "cors"),
            ("Sec-Fetch-Site", "same-origin"),
        ],
        None,
    );
    assert_eq!(fetch.status, 403, "{fetch:?}");
    assert!(minimal(&fetch), "{fetch:?}");

    // htmx asking for a fragment: never a whole page swapped into part of one.
    let htmx = server.get(
        "/l/acme",
        &[
            ("Accept", CHROME_ACCEPT),
            ("Sec-Fetch-Mode", "cors"),
            ("Sec-Fetch-Site", "same-origin"),
            ("HX-Request", "true"),
        ],
        None,
    );
    assert_eq!(htmx.status, 403, "{htmx:?}");
    assert!(!is_gonk_shell(&htmx), "{htmx:?}");
    assert!(!htmx.body.contains("signin-here"), "{htmx:?}");
}

/// ★ The keyboard path and focus (D13), as wiring: the heading is focusable and autofocused, so
/// a page with no script puts focus on it; the sign-in is a native button (Tab reaches it,
/// Enter and Space press it); the script moves focus to it once it shows it, and signing in
/// reloads the page, so the browser lands on the address it asked for.
#[test]
fn focus_lands_on_the_sign_in_and_signing_in_returns_to_the_page() {
    let server = Server::start(true);
    let refused = server.navigate("/l/acme", None);
    let section = section(&refused.body);
    assert!(
        section.contains(
            "<h1 aria-describedby=\"refusal-sentence\" autofocus=\"autofocus\" id=\"refusal-title\" tabindex=\"-1\">"
        ),
        "{section}"
    );
    assert!(
        section.contains("aria-labelledby=\"refusal-title\""),
        "{section}"
    );
    // A native button, in the tab order (no tabindex), and the one the script wires.
    let button = &section[section.find("<button").unwrap()..];
    let button = &button[..button.find('>').unwrap()];
    assert!(button.contains("type=\"button\""), "{button}");
    assert!(!button.contains("tabindex"), "{button}");
    assert!(section.contains("data-signin-notice=\"true\""), "{section}");

    let script = include_str!("../web/gonk.js");
    let focus = script
        .split("function focusRefusal()")
        .nth(1)
        .expect("the refusal focus function");
    let focus = &focus[..focus.find("\n  }").unwrap()];
    assert!(focus.contains("[data-refusal]"), "{focus}");
    assert!(focus.contains(".signin-here"), "{focus}");
    // The button when it shows, else the heading (a browser may skip `autofocus`).
    assert!(
        focus.contains("button && !button.hidden ? button : heading"),
        "{focus}"
    );
    assert!(focus.contains("target.focus()"), "{focus}");
    // It runs after the notices are drawn, which is when the button is shown.
    assert!(
        script.contains("drawNotices(document);\n    focusRefusal();"),
        "focusRefusal runs after drawNotices"
    );
    // A click on the page's button signs in, and a sign-in reloads the page it was on.
    assert!(script.contains("e.target.closest(\".signin-here\")) signIn()"));
    let sign_in = script
        .split("async function signIn()")
        .nth(1)
        .expect("signIn");
    let sign_in = &sign_in[..sign_in.find("\n  }\n").unwrap()];
    assert!(sign_in.contains("location.reload()"), "{sign_in}");
}
