//! Ledger [#3841](http://localhost:1062/l/default/item/3841): gonk's pages opt in to ikigai-ui's
//! write-lapse handling (ikigai-ui PR 81, ledger #3822). Every page names its session answer
//! (`<meta name="ikigai-ui-session" content="/auth/session">`), `/auth/session` answers the
//! shell's GET for the request's own session, and every write form carries `data-ui-write`, so
//! the shell disables it with a note once the session's `writeSeconds` is 0. Driven over real
//! HTTP with a software authenticator against a scratch gonk (port 0, a tempdir config home).
//!
//! - [`a_lapsed_session_reads_on_and_its_write_forms_are_marked`]
//! - [`no_session_and_a_live_writer_see_no_change`]
//! - [`every_write_on_the_pages_carries_the_mark`]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

mod common;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;
use serde_json::Value;

/// `gonk.http.session_max_hours = 240`, so a session reads on past its 12 h write bound.
const MAX_SECONDS: u64 = 240 * 3_600;

struct Server {
    addr: SocketAddr,
}

impl Server {
    /// Over `hub` and the passkeys in `layout`, read afresh (so a `sessions.json` written
    /// before is what it restores); the anonymous caller holds nothing.
    fn start(hub: Arc<ikigai_core::Kernel>, layout: &quic::Layout) -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let passkeys = Arc::new(
            Passkeys::new(layout.clone(), addr.port())
                .with_max_seconds(MAX_SECONDS)
                .with_idle_seconds(MAX_SECONDS),
        );
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
        let door = doors::HttpDoor {
            anonymous: Vec::new(),
            bind: addr,
            passkeys: Some(passkeys),
            public: Vec::new(),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
            public_origin: None,
        };
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door.clone()),
                listener,
                doors::edge_config(door),
            ))
        });
        Server { addr }
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    fn raw(&self, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> Response {
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
        Response::parse(&response)
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

    fn cookie(token: Option<&str>) -> Vec<(&'static str, String)> {
        token
            .map(|t| vec![("Cookie", format!("{}={t}", identity::SESSION_COOKIE))])
            .unwrap_or_default()
    }

    /// A page, as a browser asks for one.
    fn page(&self, path: &str, token: Option<&str>) -> Response {
        let mut headers = vec![("Accept", "text/html".to_string())];
        headers.extend(Self::cookie(token));
        self.raw("GET", path, &headers, "")
    }

    /// The shell's read of the session answer: a same-origin GET for JSON.
    fn session(&self, token: Option<&str>) -> Response {
        let mut headers = vec![
            ("Accept", "application/json".to_string()),
            ("Sec-Fetch-Site", "same-origin".to_string()),
        ];
        headers.extend(Self::cookie(token));
        self.raw("GET", "/auth/session", &headers, "")
    }

    /// File an item in `default` as `token`, as the ledger page's form does.
    fn file(&self, content: &str, token: &str) -> Response {
        let body = format!(
            "_ledger=default&_action=append&_then=items&content={}",
            content.replace(' ', "+")
        );
        let mut headers = vec![
            ("Accept", "*/*".to_string()),
            ("Origin", self.origin()),
            ("Sec-Fetch-Site", "same-origin".to_string()),
            ("HX-Request", "true".to_string()),
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
        ];
        headers.extend(Self::cookie(Some(token)));
        self.raw("POST", "/act", &headers, &body)
    }

    fn challenge(&self, op: &str) -> String {
        let got = self.json(&format!("/auth/{op}"), "{}");
        assert_eq!(got.status, 200, "{got:?}");
        let v: Value = serde_json::from_str(&got.body).unwrap();
        v["challenge"].as_str().unwrap().to_string()
    }

    fn enroll(&self, layout: &quic::Layout, authenticator: &Authenticator, scopes: &[String]) {
        let invite =
            identity::invite(layout, "writer", scopes, false, 30, identity::now_seconds()).unwrap();
        let c = self.challenge("register-options");
        let enrolled = self.json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(enrolled.status, 200, "{enrolled:?}");
    }

    fn login(&self, authenticator: &Authenticator, count: u32) -> String {
        let c = self.challenge("login-options");
        let login = self.json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), count),
        );
        assert_eq!(login.status, 200, "{login:?}");
        let v: Value = serde_json::from_str(&login.body).unwrap();
        v["session"].as_str().unwrap().to_string()
    }
}

struct Response {
    status: u16,
    head: String,
    body: String,
}

impl Response {
    fn parse(response: &str) -> Response {
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        Response {
            status,
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    /// The body as JSON. A chunked body is taken from its first `{` to its last `}`.
    fn json(&self) -> Value {
        let start = self.body.find('{').expect("a JSON body");
        let end = self.body.rfind('}').expect("a JSON body");
        serde_json::from_str(&self.body[start..=end]).unwrap()
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.head, self.body)
    }
}

/// A scratch gonk with a writer enrolled on `default`, signed in, and one item filed.
fn signed_in_writer() -> (
    tempfile::TempDir,
    quic::Layout,
    Arc<ikigai_core::Kernel>,
    Server,
    String,
) {
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let server = Server::start(Arc::clone(&hub), &layout);
    let scopes = grants_for("default", Authority::Write).unwrap();
    let authenticator = Authenticator::new();
    server.enroll(&layout, &authenticator, &scopes);
    let token = server.login(&authenticator, 1);
    let filed = server.file("A first item", &token);
    assert_eq!(filed.status, 200, "{filed:?}");
    (config, layout, hub, server, token)
}

/// Move every session in `sessions.json` back by `seconds`, as if it had signed in that much
/// earlier, keeping how long it lasts.
fn backdate(layout: &quic::Layout, seconds: u64) {
    let path = layout.sessions_json();
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for session in doc["sessions"].as_object_mut().unwrap().values_mut() {
        for field in ["issued", "expires"] {
            let at = session[field].as_u64().unwrap();
            session[field] = Value::from(at - seconds);
        }
    }
    std::fs::write(&path, doc.to_string()).unwrap();
}

/// The pages a signed-in writer reaches whose forms write.
const WRITE_PAGES: [&str; 6] = [
    "/",
    "/l/default",
    "/l/default/item/1",
    "/queue",
    "/l/default/lifecycle",
    "/dashboard",
];

/// Whether `html` names the session answer for the shell, in either attribute order.
fn names_session(html: &str) -> bool {
    html.split("<meta").skip(1).any(|tag| {
        let tag = &tag[..tag.find('>').unwrap_or(tag.len())].replace('"', "'");
        tag.contains("name='ikigai-ui-session'") && tag.contains("content='/auth/session'")
    })
}

/// Every tag in `html` that writes (`hx-post`, `hx-put`, `hx-patch`, `hx-delete`, or a form
/// whose method is POST) and is neither marked `data-ui-write` itself nor inside a marked
/// form: the tag, quoted, so a failure names it.
fn unmarked_writes(html: &str) -> Vec<String> {
    let mut unmarked = Vec::new();
    let mut in_marked_form = 0usize;
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        rest = &rest[start..];
        let end = rest.find('>').map(|e| e + 1).unwrap_or(rest.len());
        let tag = &rest[..end];
        let lower = tag.to_ascii_lowercase();
        rest = &rest[end..];
        if lower.starts_with("</form") {
            in_marked_form = in_marked_form.saturating_sub(1);
            continue;
        }
        let marked = lower.contains("data-ui-write");
        if lower.starts_with("<form") {
            in_marked_form = if marked { in_marked_form + 1 } else { 0 };
        }
        let writes = ["hx-post=", "hx-put=", "hx-patch=", "hx-delete="]
            .iter()
            .any(|a| lower.contains(a))
            || (lower.starts_with("<form")
                && (lower.contains("method=\"post\"") || lower.contains("method='post'")));
        if writes && !marked && in_marked_form == 0 {
            unmarked.push(tag.to_string());
        }
    }
    unmarked
}

/// ★ A writer 13 h after the sign-in: write has lapsed, reads run on. The session answer the
/// shell reads says `writeSeconds` 0 with something to `restore` (so the shell shows "Writing
/// has lapsed"), the pages still answer, name that answer, and carry their write forms marked,
/// which is what the shell disables with its note; and a write is refused by the server too.
#[test]
fn a_lapsed_session_reads_on_and_its_write_forms_are_marked() {
    let (_config, layout, hub, server, token) = signed_in_writer();
    let live = server.session(Some(&token));
    assert_eq!(live.status, 200, "{live:?}");
    assert!(
        live.json()["writeSeconds"].as_u64().unwrap() > 0,
        "{live:?}"
    );
    // The page as loaded while write was live: its write forms marked, for the shell to
    // disable when the countdown reaches 0 with the page still open.
    let before = server.page("/l/default/item/1", Some(&token));
    assert!(names_session(&before.body), "{before:?}");
    assert!(
        before.body.matches("data-ui-write").count() > 1,
        "the item's write forms are marked: {before:?}"
    );
    assert_eq!(unmarked_writes(&before.body), Vec::<String>::new());

    backdate(&layout, 13 * 3_600);
    let server = Server::start(hub, &layout);
    let lapsed = server.session(Some(&token));
    assert_eq!(lapsed.status, 200, "{lapsed:?}");
    let answer = lapsed.json();
    assert_eq!(answer["writeSeconds"], 0, "{answer}");
    assert!(
        !answer["restore"].as_array().unwrap().is_empty(),
        "a fresh sign-in restores write: {answer}"
    );

    let item = server.page("/l/default/item/1", Some(&token));
    assert_eq!(item.status, 200, "reads run on: {item:?}");
    assert!(item.body.contains("A first item"), "{item:?}");
    assert!(
        names_session(&item.body),
        "the page names its session answer: {item:?}"
    );
    assert_eq!(unmarked_writes(&item.body), Vec::<String>::new());

    let refused = server.file("After the lapse", &token);
    assert_eq!(refused.status, 403, "{refused:?}");
}

/// A caller with no passkey session (the anonymous loopback reader, or the socket owner, who
/// never comes through the HTTP door's sign-in) gets `{}` from the session answer: a 200, so
/// the browser logs no error, with no numeric `writeSeconds`, which the shell reads as
/// "nothing changes"; a live writer's answer still has write left. A GET
/// cannot ask about another session by naming its token.
#[test]
fn no_session_and_a_live_writer_see_no_change() {
    let (_config, _layout, _hub, server, token) = signed_in_writer();
    let anonymous = server.session(None);
    assert_eq!(anonymous.status, 200, "{anonymous:?}");
    assert_eq!(anonymous.json(), serde_json::json!({}), "{anonymous:?}");

    let live = server.session(Some(&token));
    assert_eq!(live.status, 200, "{live:?}");
    let answer = live.json();
    let left = answer["writeSeconds"].as_u64().unwrap();
    assert!(left > 11 * 3_600, "{answer}");
    assert_eq!(answer["restore"], serde_json::json!([]), "{answer}");
    assert!(
        !live.head.to_ascii_lowercase().contains("max-age="),
        "the answer is not cached: {live:?}"
    );

    let mut headers = vec![("Accept", "application/json".to_string())];
    headers.push(("Sec-Fetch-Site", "same-origin".to_string()));
    let by_token = server.raw(
        "GET",
        &format!("/auth/session?content={token}"),
        &headers,
        "",
    );
    assert_ne!(by_token.status, 200, "{by_token:?}");

    // The POST gonk.js makes is unchanged.
    let posted = server.json("/auth/session", &token);
    assert_eq!(posted.status, 200, "{posted:?}");
}

/// ★ Every write a signed-in writer's pages render carries `data-ui-write`, on it or on the
/// form around it: a new write form left unmarked fails here, naming its tag.
#[test]
fn every_write_on_the_pages_carries_the_mark() {
    let (_config, _layout, _hub, server, token) = signed_in_writer();
    let mut writes = 0;
    for path in WRITE_PAGES {
        let page = server.page(path, Some(&token));
        assert_eq!(page.status, 200, "{path}: {page:?}");
        assert!(names_session(&page.body), "{path} names its session answer");
        assert_eq!(
            unmarked_writes(&page.body),
            Vec::<String>::new(),
            "{path}: a write with no data-ui-write"
        );
        writes += page.body.matches("data-ui-write").count();
    }
    assert!(writes >= 5, "the pages carry write forms: {writes}");
    // The check itself catches an unmarked write.
    assert_eq!(
        unmarked_writes("<form method=\"post\" hx-post=\"/act\"><button>Go</button></form>").len(),
        1
    );
    assert!(unmarked_writes("<div data-ui-write><button hx-delete=\"/x\">").len() == 1);
}
