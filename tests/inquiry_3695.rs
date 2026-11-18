//! Ledger [#3695](http://localhost:1062/l/default/item/3695): the `ikigai-inquiry` layers client,
//! mounted behind inquiry tokens, against a SCRATCH layer host.
//!
//! The host is the published `ikigai-layers` in process, opened on the layer files
//! `ikigai-inquiry` 0.1.0 ships (`tests/inquiry_host`), behind a door that mints Brian (a
//! person) and counts what reaches it. gonk's HTTP door is a scratch one on `127.0.0.1:0`, with
//! passkeys signed in by a software authenticator. One inquiry waits in the spam layer and one in
//! the inquiry layer.
//!
//! - [`a_reader_sees_both_views`]
//! - [`without_the_read_token_the_view_is_denied_and_the_host_hears_nothing`]
//! - [`not_spam_needs_triage_and_curate_then_moves_and_leaves_an_intent`]
//! - [`a_second_action_on_a_taken_inquiry_answers_the_conflict`]
//! - [`an_anonymous_loopback_caller_holds_none_of_the_tokens`]
//! - [`a_cross_origin_post_is_refused_and_moves_nothing`]
//! - [`no_layer_name_resolves_through_the_hub_from_the_socket`]
//! - [`an_unconfigured_gonk_serves_the_not_configured_page`]
//! - [`the_client_writes_only_as_a_person`]
//! - [`another_pages_navigation_keeps_no_inquiry_token`]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

mod common;
mod inquiry_host;

use common::Authenticator;
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_fn::html::url_segment as encode;
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::inquiry::{Inquiry, Token};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;
use inquiry_host::{Host, INQUIRY, INTENT, OWNER, SPAM};

/// The day the spam arrived, which the spam view is asked for.
const DAY: &str = "2026-11-14";

/// One inquiry in each of the two layers a person works through.
fn seeded() -> Host {
    let host = Host::open();
    host.arrive(
        SPAM,
        "s1",
        "a.example",
        "Cheap watches, today only",
        "2026-11-14T09:00:00.000Z",
    );
    host.arrive(
        INQUIRY,
        "q1",
        "b.example",
        "Could you speak at our meetup?",
        "2026-11-14T10:00:00.000Z",
    );
    host
}

fn tokens(words: &[Token]) -> Vec<String> {
    words.iter().map(|t| t.iri(OWNER)).collect()
}

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

/// A scratch gonk HTTP door with the inquiry client over `host`'s door as `slug`, or none.
struct Gonk {
    addr: SocketAddr,
    layout: quic::Layout,
    hub: Arc<Kernel>,
    _config: tempfile::TempDir,
}

impl Gonk {
    fn start(inquiry: Option<Arc<Inquiry>>, anonymous: Vec<String>) -> Gonk {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let passkeys = Arc::new(Passkeys::new(layout.clone(), addr.port()));
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
            inquiry: inquiry.as_ref().map(|i| i.owner().to_string()),
        });
        let pages = web::door_space(Arc::clone(&face), None, inquiry);
        let http = Arc::new(doors::http_kernel_with(Arc::clone(&hub), pages, None));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
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
        Gonk {
            addr,
            layout,
            hub,
            _config: config,
        }
    }

    /// Over `host`'s door as `slug`, its views and actions under `owner`.
    fn over(host: &Host, slug: &str) -> Gonk {
        let inquiry = Inquiry::new(OWNER, host.door(slug)).unwrap();
        Gonk::start(Some(inquiry), anonymous())
    }

    /// Enroll a passkey under a grant of `scopes`, sign in, and answer the session token.
    fn sign_in(&self, grant: &str, scopes: &[String]) -> String {
        let authenticator = Authenticator::named(grant);
        let invite = identity::invite(
            &self.layout,
            grant,
            scopes,
            false,
            30,
            identity::now_seconds(),
        )
        .unwrap();
        let json = |path: &str, body: &str| {
            let answer = self.raw(
                "POST",
                path,
                &[
                    ("Accept", "application/json".to_string()),
                    ("Content-Type", "application/json".to_string()),
                    ("Origin", self.origin()),
                    ("Sec-Fetch-Site", "same-origin".to_string()),
                ],
                body,
            );
            assert_eq!(answer.status, 200, "{answer:?}");
            serde_json::from_str::<serde_json::Value>(&answer.body).unwrap()
        };
        let challenge = |op: &str| {
            json(&format!("/auth/{op}"), "{}")["challenge"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let c = challenge("register-options");
        json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        let c = challenge("login-options");
        json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        )["session"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    /// A person opening a page, signed in with `session` (or not).
    fn page(&self, path: &str, session: Option<&str>) -> Response {
        let mut headers = vec![
            ("Accept", CHROME_ACCEPT.to_string()),
            ("Sec-Fetch-Site", "none".to_string()),
            ("Sec-Fetch-Mode", "navigate".to_string()),
        ];
        if let Some(session) = session {
            headers.push(("Cookie", format!("{}={session}", identity::SESSION_COOKIE)));
        }
        self.raw("GET", path, &headers, "")
    }

    /// A view's button pressed: a form post from `origin`, signed in with `session` (or not).
    fn press(&self, path: &str, id: &str, session: Option<&str>, origin: &str) -> Response {
        let body = format!("inquiry={}", encode(id));
        let same = origin == self.origin();
        let mut headers = vec![
            ("Accept", CHROME_ACCEPT.to_string()),
            ("Origin", origin.to_string()),
            (
                "Sec-Fetch-Site",
                if same { "same-origin" } else { "cross-site" }.to_string(),
            ),
            ("Sec-Fetch-Mode", "navigate".to_string()),
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
        ];
        if let Some(session) = session {
            headers.push(("Cookie", format!("{}={session}", identity::SESSION_COOKIE)));
        }
        self.raw("POST", path, &headers, &body)
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
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        Response::parse(&String::from_utf8_lossy(&response))
    }
}

/// What gonk mints for any loopback process.
fn anonymous() -> Vec<String> {
    ikigai_gonk::grants::anonymous_http_grants(&["default".to_string()]).unwrap()
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

    /// The page's status region, as served.
    fn status_region(&self) -> &str {
        let at = self
            .body
            .find("id='status'")
            .or_else(|| self.body.find("id=\"status\""))
            .unwrap_or_else(|| panic!("no status region: {self:?}"));
        let rest = &self.body[at..];
        &rest[..rest.find("</div>").unwrap_or(rest.len())]
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.head, self.body)
    }
}

/// No `style` attribute anywhere on a page: gonk's CSP (`style-src 'self'`) refuses each one.
fn no_inline_style(page: &Response) {
    assert!(!page.body.contains(" style="), "{page:?}");
}

fn spam_path() -> String {
    format!("/inquiry/view/name/spam?day={DAY}")
}

const NOT_SPAM: &str = "/inquiry/view/name/spam/action/name/not-spam";
const REMOVE: &str = "/inquiry/view/name/spam/action/name/remove";

#[test]
fn a_reader_sees_both_views() {
    let host = seeded();
    let gonk = Gonk::over(&host, "brian");
    let session = gonk.sign_in("brian", &tokens(&[Token::Read]));

    let pending = gonk.page("/inquiry/view/name/pending", Some(&session));
    assert_eq!(pending.status, 200, "{pending:?}");
    assert!(
        pending.body.contains("Could you speak at our meetup?"),
        "{pending:?}"
    );
    assert!(pending.body.contains("b.example"), "{pending:?}");
    assert!(!pending.body.contains("Cheap watches"), "{pending:?}");
    // In gonk's chrome, with the crate's buttons posting under this view's base.
    assert!(
        pending.body.contains("<title>Inquiries · gonk</title>"),
        "{pending:?}"
    );
    assert!(
        pending
            .body
            .contains("action=\"/inquiry/view/name/pending/action/name/respond\""),
        "{pending:?}"
    );
    // The header offers the client to a holder of the read token, as the current page.
    assert!(pending.body.contains(">Inquiries</a>"), "{pending:?}");
    // The full message keeps its line breaks by gonk's stylesheet (`.inquiry-message`), never an
    // inline style, which the page's CSP (`style-src 'self'`) refuses: ikigai-inquiry 0.1.1
    // writes none, and gonk strips none.
    assert!(pending.body.contains("<details>"), "{pending:?}");
    assert!(
        pending.body.contains("class=\"inquiry-message\""),
        "{pending:?}"
    );
    no_inline_style(&pending);

    let spam = gonk.page(&spam_path(), Some(&session));
    assert_eq!(spam.status, 200, "{spam:?}");
    assert!(spam.body.contains("Cheap watches"), "{spam:?}");
    assert!(spam.body.contains(&format!("Spam for {DAY}")), "{spam:?}");
    assert!(
        spam.body
            .contains("action=\"/inquiry/view/name/spam/action/name/not-spam?day=2026-11-14\""),
        "{spam:?}"
    );
    // The day picker shows the day the page is for.
    assert!(
        spam.body.contains(&format!("value='{DAY}'"))
            || spam.body.contains(&format!("value=\"{DAY}\"")),
        "{spam:?}"
    );
    no_inline_style(&spam);
    // Reading moved nothing.
    assert_eq!(host.ids(SPAM), ["s1"]);
    assert_eq!(host.ids(INQUIRY), ["q1"]);
}

#[test]
fn without_the_read_token_the_view_is_denied_and_the_host_hears_nothing() {
    let host = seeded();
    let gonk = Gonk::over(&host, "brian");
    // Every action's token, and another owner's read token: none of them reads Brian's.
    let mut scopes = tokens(&[Token::Triage, Token::Curate, Token::Block, Token::Respond]);
    scopes.push(Token::Read.iri("chris"));
    let session = gonk.sign_in("brian", &scopes);
    for path in ["/inquiry/view/name/pending".to_string(), spam_path()] {
        let page = gonk.page(&path, Some(&session));
        assert_eq!(page.status, 403, "{path}: {page:?}");
        assert!(
            page.body.contains(&Token::Read.iri(OWNER)),
            "the refusal names the token: {page:?}"
        );
        assert!(!page.body.contains("Cheap watches"), "{page:?}");
        assert!(!page.body.contains("meetup"), "{page:?}");
        assert!(
            !page.body.contains(">Inquiries</a>"),
            "no header link: {page:?}"
        );
    }
    assert_eq!(host.seen(), 0, "the layer host saw {:?}", host.targets());
}

#[test]
fn not_spam_needs_triage_and_curate_then_moves_and_leaves_an_intent() {
    let host = seeded();
    let gonk = Gonk::over(&host, "brian");

    let triage = gonk.sign_in("triage", &tokens(&[Token::Read, Token::Triage]));
    let refused = gonk.press(NOT_SPAM, "s1", Some(&triage), &gonk.origin());
    assert_eq!(refused.status, 403, "{refused:?}");
    assert!(
        refused.body.contains(&Token::Curate.iri(OWNER)),
        "the refusal names the missing token: {refused:?}"
    );
    assert_eq!(host.seen(), 0, "the layer host saw {:?}", host.targets());
    assert_eq!(host.ids(SPAM), ["s1"]);
    assert_eq!(host.ids(INQUIRY), ["q1"]);
    assert!(host.pending(INTENT).is_empty());

    let both = gonk.sign_in(
        "curator",
        &tokens(&[Token::Read, Token::Triage, Token::Curate]),
    );
    // Pressed on that day's page, whose buttons carry the day.
    let moved = gonk.press(
        &format!("{NOT_SPAM}?day={DAY}"),
        "s1",
        Some(&both),
        &gonk.origin(),
    );
    assert_eq!(moved.status, 200, "{moved:?}");
    // Back on the view the button was on, the crate's sentence in the status region.
    assert!(
        moved
            .status_region()
            .contains("Moved inquiry s1 from spam to your inquiries."),
        "{}",
        moved.status_region()
    );
    assert!(
        moved
            .body
            .contains(&format!("<title>Spam for {DAY} · gonk</title>")),
        "{moved:?}"
    );
    no_inline_style(&moved);
    assert!(host.ids(SPAM).is_empty());
    let mut ids = host.ids(INQUIRY);
    ids.sort();
    assert_eq!(ids, ["q1", "s1"]);
    // Written by the person the door names, never by this server.
    let copy = host
        .pending(INQUIRY)
        .into_iter()
        .find(|n| n["inquiry"] == "s1")
        .unwrap();
    assert_eq!(copy["attributedTo"], inquiry_host::participant("brian"));
    let intents = host.pending(INTENT);
    assert_eq!(intents.len(), 1, "{intents:?}");
    assert_eq!(intents[0]["intent"], "train-ham");
    assert_eq!(intents[0]["inquiry"], "s1");
}

#[test]
fn a_second_action_on_a_taken_inquiry_answers_the_conflict() {
    let host = seeded();
    let gonk = Gonk::over(&host, "brian");
    let session = gonk.sign_in("brian", &tokens(&[Token::Read, Token::Triage]));
    let first = gonk.press(REMOVE, "s1", Some(&session), &gonk.origin());
    assert_eq!(first.status, 200, "{first:?}");
    assert!(
        first
            .status_region()
            .contains("Removed inquiry s1 from spam."),
        "{}",
        first.status_region()
    );
    assert!(host.ids(SPAM).is_empty());
    // A page loaded before the first press, pressed again.
    let again = gonk.press(REMOVE, "s1", Some(&session), &gonk.origin());
    assert_eq!(again.status, 200, "{again:?}");
    let region = again.status_region();
    assert!(region.contains("flash error"), "{region}");
    assert!(
        region.contains(&inquiry_host::participant("brian")),
        "the Conflict names who took it: {region}"
    );
    assert!(host.ids(SPAM).is_empty());
    assert_eq!(host.ids(INQUIRY), ["q1"]);
}

#[test]
fn an_anonymous_loopback_caller_holds_none_of_the_tokens() {
    let anonymous = anonymous();
    assert!(
        !anonymous.iter().any(|s| s.starts_with("urn:cap:inquiry:")),
        "{anonymous:?}"
    );
    let host = seeded();
    let gonk = Gonk::over(&host, "brian");
    for path in ["/inquiry/view/name/pending".to_string(), spam_path()] {
        let page = gonk.page(&path, None);
        assert_eq!(page.status, 403, "{path}: {page:?}");
        assert!(
            page.body.contains("Sign in to read the inquiries."),
            "{page:?}"
        );
        assert!(
            !page.body.contains(">Inquiries</a>"),
            "no header link: {page:?}"
        );
    }
    for action in [
        "remove",
        "not-spam",
        "confirm-spam",
        "flag-spam",
        "block",
        "unblock",
        "respond",
    ] {
        let path = format!("/inquiry/view/name/spam/action/name/{action}");
        let refused = gonk.press(&path, "s1", None, &gonk.origin());
        assert_eq!(refused.status, 403, "{action}: {refused:?}");
    }
    assert_eq!(host.seen(), 0, "the layer host saw {:?}", host.targets());
    assert_eq!(host.ids(SPAM), ["s1"]);
    assert_eq!(host.ids(INQUIRY), ["q1"]);
}

#[test]
fn a_cross_origin_post_is_refused_and_moves_nothing() {
    let host = seeded();
    let gonk = Gonk::over(&host, "brian");
    let session = gonk.sign_in(
        "brian",
        &tokens(&[Token::Read, Token::Triage, Token::Curate]),
    );
    for origin in ["https://evil.example", "http://localhost:8090"] {
        let refused = gonk.press(NOT_SPAM, "s1", Some(&session), origin);
        assert_eq!(refused.status, 403, "{origin}: {refused:?}");
    }
    assert_eq!(host.seen(), 0, "the layer host saw {:?}", host.targets());
    assert_eq!(host.ids(SPAM), ["s1"]);
}

#[test]
fn no_layer_name_resolves_through_the_hub_from_the_socket() {
    let host = seeded();
    let gonk = Gonk::over(&host, "brian");
    // The socket door as `main` builds it, under the owner's root capability.
    let socket = doors::door_kernel(Arc::clone(&gonk.hub));
    for (verb, target) in [
        (Verb::Source, format!("urn:layer:{SPAM}:tuples")),
        (Verb::Source, format!("urn:layer:{INQUIRY}:tuples")),
        (Verb::Sink, format!("urn:layer:{INQUIRY}:tuples")),
        (Verb::Source, "urn:layers:me".to_string()),
        (Verb::Source, "urn:collab:inquiry".to_string()),
        (Verb::Source, ikigai_inquiry::PENDING_VIEW.to_string()),
        (Verb::Source, ikigai_inquiry::SPAM_VIEW.to_string()),
        (Verb::Sink, "urn:iki:inquiry:action:name:remove".to_string()),
    ] {
        let request = Request::new(verb, Iri::parse(target.as_str()).unwrap())
            .with_arg("inquiry", ArgRef::Inline(b"s1".to_vec()));
        let answer = block_on(socket.issue(request, &Capability::root()));
        assert!(answer.is_err(), "{target} resolved through the hub");
    }
    assert_eq!(host.seen(), 0, "the layer host saw {:?}", host.targets());
    assert_eq!(host.ids(SPAM), ["s1"]);
}

#[test]
fn an_unconfigured_gonk_serves_the_not_configured_page() {
    let gonk = Gonk::start(None, anonymous());
    // Anyone may read that it is not configured: there is nothing behind it to protect.
    for path in ["/inquiry/view/name/pending", "/inquiry/view/name/spam"] {
        let page = gonk.page(path, None);
        assert_eq!(page.status, 200, "{path}: {page:?}");
        assert!(page.body.contains("not configured"), "{page:?}");
        assert!(page.body.contains("gonk.inquiry.owner"), "{page:?}");
        assert!(page.body.contains("gonk.layers.view"), "{page:?}");
    }
    let session = gonk.sign_in("brian", &tokens(&[Token::Read, Token::Triage]));
    let action = gonk.press(REMOVE, "s1", Some(&session), &gonk.origin());
    assert_eq!(action.status, 404, "{action:?}");
    // And the rest of gonk serves as before.
    let home = gonk.page("/", None);
    assert_eq!(home.status, 200, "{home:?}");
    assert!(!home.body.contains(">Inquiries</a>"), "{home:?}");
}

#[test]
fn the_client_writes_only_as_a_person() {
    let host = seeded();
    // A door whose certificate is a machine's, holding Brian's verbs at the host.
    let gonk = Gonk::over(&host, "plasma-gonk");
    let session = gonk.sign_in("brian", &tokens(&[Token::Read, Token::Triage]));
    let spam = gonk.page(&spam_path(), Some(&session));
    assert_eq!(spam.status, 200, "reading is allowed: {spam:?}");
    let refused = gonk.press(REMOVE, "s1", Some(&session), &gonk.origin());
    assert_eq!(refused.status, 403, "{refused:?}");
    assert!(refused.body.contains("not a person"), "{refused:?}");
    assert_eq!(host.ids(SPAM), ["s1"]);
    assert!(
        !host.targets().iter().any(|t| !t.starts_with("Source ")),
        "only reads reached the host: {:?}",
        host.targets()
    );
}

#[test]
fn another_pages_navigation_keeps_no_inquiry_token() {
    let all = tokens(&[
        Token::Read,
        Token::Triage,
        Token::Curate,
        Token::Block,
        Token::Respond,
    ]);
    let kept = ikigai_gonk::admit::foreign_page_scopes(&all);
    assert_eq!(kept, [ikigai_gonk::admit::FOREIGN_PAGE], "{kept:?}");
}
