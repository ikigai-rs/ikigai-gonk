//! The HTML face, driven over real HTTP the way a browser drives it.
//!
//! ⚠ **Every browser-shaped request here sends Chrome's `Accept` header**, never curl's
//! `*/*`. `ikigai-web` negotiates `Accept` against the faces a resource declares (since
//! ikigai-cli PR #333), and Chrome's header prefers `text/html`, so a page whose HTML face went
//! missing would be answered in another face — or `406` — while every `*/*` test passed.
//!
//! - [`a_listing_renders_a_bounded_number_of_rows_and_says_what_it_left_out`]
//! - [`a_browser_gets_html_pages_and_a_readable_404`]
//! - [`a_hostile_ledger_link_is_refused_never_resolved`]
//! - [`a_person_files_edits_comments_and_closes_through_forms`]
//! - [`a_form_can_send_only_what_the_ledger_declares`]
//! - [`a_cross_site_write_and_a_rebound_host_get_nothing`]
//! - [`sparql_is_confined_to_one_ledger_graph`]
//! - [`every_sample_query_returns_rows`]
//! - [`the_sparql_page_offers_the_samples_without_running_them`]
//! - [`the_cross_graph_example_is_shown_and_never_loadable`]
//! - [`a_ledger_iri_reads_as_its_local_name_only_in_the_html_face`]
//! - [`a_passkey_identity_adds_its_grant_and_only_its_grant`]
//! - [`a_signed_in_form_write_is_attributed_to_the_passkey_and_renders_its_label`]
//! - [`an_anonymous_form_write_has_no_author_and_a_form_may_not_name_one`]
//! - [`the_mechanical_door_carries_the_principal_and_the_ledger_ignores_it`]
//! - [`an_http_delete_answers_with_the_ledgers_text`]
//! - [`a_post_expecting_100_continue_is_invited_at_once`]
//! - [`the_palette_clears_the_contrast_floor`]
//! - [`a_result_column_is_never_narrower_than_its_own_word`]
//! - [`a_sample_button_is_a_toggle_the_server_renders_unpressed`]
//! - [`the_active_sample_is_cleared_by_typing_in_the_editor`]
//! - [`the_doctor_says_what_it_judged_against_on_one_line`]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;
use p256::ecdsa::SigningKey;

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

struct Server {
    addr: SocketAddr,
    layout: quic::Layout,
    /// The hub behind the door, for a test that must seed what no door may write.
    hub: Arc<ikigai_core::Kernel>,
    _config: tempfile::TempDir,
}

impl Server {
    fn start() -> Server {
        Server::start_with_rules(ikigai_gonk::rules::DEFAULT_RULES)
    }

    /// A server whose render rules are `rules` rather than the shipped table — what a
    /// deployment gets by dropping its own `gonk/render-rules.ttl` beside `grants.json`.
    fn start_with_rules(rules: &str) -> Server {
        Server::start_configured(rules, grants_for("default", Authority::Write).unwrap())
    }

    /// A server whose ANONYMOUS loopback caller holds `anonymous` — empty, for a caller that
    /// may read no ledger. (A rebound `Host` used to be that caller; since ledger #864 it is
    /// refused outright, so it holds nothing to render a page with.)
    fn start_configured(rules: &str, anonymous: Vec<String>) -> Server {
        Server::start_with_roots(rules, anonymous, Vec::new())
    }

    /// [`Server::start_configured`] with browse ROOT NAMES configured: the header's entry
    /// points are computed from them. No browse family is mounted behind them.
    fn start_with_roots(rules: &str, anonymous: Vec<String>, browse_roots: Vec<String>) -> Server {
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
            browse_roots,
            passkeys: Arc::clone(&passkeys),
            rules: rules.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
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
            layout,
            hub,
            _config: config,
        }
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    fn raw(&self, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> Response {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let mut head = format!("{method} {path} HTTP/1.1\r\n");
        let has_host = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host"));
        if !has_host {
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
        Response::parse(&response)
    }

    /// A browser navigation.
    fn page(&self, path: &str, cookie: Option<&str>) -> Response {
        let mut headers = vec![
            ("Accept", CHROME_ACCEPT.to_string()),
            ("Sec-Fetch-Site", "none".to_string()),
        ];
        if let Some(cookie) = cookie {
            headers.push(("Cookie", format!("{}={cookie}", identity::SESSION_COOKIE)));
        }
        self.raw("GET", path, &headers, "")
    }

    /// An htmx form post from this origin.
    fn form(&self, fields: &[(&str, &str)], cookie: Option<&str>) -> Response {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
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
        if let Some(cookie) = cookie {
            headers.push(("Cookie", format!("{}={cookie}", identity::SESSION_COOKIE)));
        }
        self.raw("POST", "/act", &headers, &body)
    }

    /// A same-origin JSON post, as gonk.js makes one.
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
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.head, self.body)
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

/// The first `urn:iki:ledger:default:item:{id}` in `text`, as `(iri, id)`.
fn first_item(text: &str) -> (String, String) {
    let at = text
        .find("urn:iki:ledger:default:item:")
        .unwrap_or_else(|| panic!("no item IRI in: {text}"));
    let id: String = text[at + "urn:iki:ledger:default:item:".len()..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    (format!("urn:iki:ledger:default:item:{id}"), id)
}

fn file(server: &Server, content: &str) -> (String, String) {
    let filed = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "append"),
            ("_then", "items"),
            ("content", content),
        ],
        None,
    );
    assert_eq!(filed.status, 200, "{filed:?}");
    first_item(&filed.body)
}

/// ★ **A listing renders a BOUNDED number of rows and says so.** `xrust` costs ~6–15 ms per
/// row and the cost per row GROWS with the set, so 410 rows measured 6.5 s on the live
/// server — a page whose slowness reads as an outage (ledger #443). The bound is only half
/// of it: a page that silently showed its first fifty would repeat the defect ledger #419
/// records on the text face, where a page's length is reported as a total.
#[test]
fn a_listing_renders_a_bounded_number_of_rows_and_says_what_it_left_out() {
    let server = Server::start();
    for n in 1..=6 {
        file(&server, &format!("Item number {n}"));
    }

    let three = server.page("/l/default?limit=3", None);
    assert_eq!(three.status, 200, "{three:?}");
    assert_eq!(
        three.body.matches("<li class='row open'>").count(),
        3,
        "three rows, not six: {three:?}"
    );
    assert!(
        three
            .body
            .contains("showing the 3 most recently updated of 6 open items"),
        "the page says what it rendered and out of what: {three:?}"
    );
    // The NEWEST three, which is the order the stylesheet sorts in — not an arbitrary three.
    assert!(
        three.body.contains("Item number 6")
            && three.body.contains("Item number 4")
            && !three.body.contains("Item number 3"),
        "{three:?}"
    );
    assert!(
        three.body.contains(">Show 6</a>")
            && three
                .body
                .contains("href='/l/default?status=open&amp;limit=6'"),
        "and offers the rest: {three:?}"
    );

    let all = server.page("/l/default?limit=all", None);
    assert_eq!(
        all.body.matches("<li class='row open'>").count(),
        6,
        "{all:?}"
    );
    assert!(
        all.body.contains("6 open items") && !all.body.contains("Show 6"),
        "nothing is left out, so nothing is offered: {all:?}"
    );

    // A search narrows the total the page reports, and the link carries the search on.
    let searched = server.page("/l/default?limit=1&text=number", None);
    assert!(
        searched
            .body
            .contains("showing the 1 most recently updated of 6 open items")
            && searched.body.contains("limit=6&amp;text=number"),
        "{searched:?}"
    );

    let refused = server.page("/l/default?limit=none", None);
    assert_eq!(refused.status, 400, "{refused:?}");
    assert!(
        refused
            .body
            .contains("`none` is not a positive number of rows"),
        "{refused:?}"
    );

    // ★ Ledger #480: a count past the cap is the queue page's rule too — drawn at the cap,
    // never refused, and the page says what it drew. (The clause naming the cap when it is
    // what stopped the page is pinned in `ikigai_gonk::rows`; six items never reach it.)
    let past = server.page(
        &format!("/l/default?limit={}", ikigai_gonk::rows::MAX_ROWS + 500),
        None,
    );
    assert_eq!(past.status, 200, "{past:?}");
    assert!(
        past.body.matches("<li class='row open'>").count() == 6
            && past.body.contains("6 open items"),
        "{past:?}"
    );

    // ★ Ledger #419, gonk's half: the page reads only the rows it draws, from `offset=`, and
    // states the ledger's own total — so every item is reachable a page at a time, and the
    // count is never a floor of what one read held.
    let second = server.page("/l/default?limit=2&offset=2", None);
    assert_eq!(second.status, 200, "{second:?}");
    assert_eq!(
        second.body.matches("<li class='row open'>").count(),
        2,
        "{second:?}"
    );
    assert!(
        second.body.contains("Item number 4")
            && second.body.contains("Item number 3")
            && !second.body.contains("Item number 5")
            && !second.body.contains("Item number 2"),
        "the third and fourth most recently updated: {second:?}"
    );
    assert!(
        second
            .body
            .contains("showing 3–4 of 6 open items, most recently updated first"),
        "{second:?}"
    );
    assert!(
        second
            .body
            .contains("href='/l/default?status=open&amp;limit=4&amp;offset=2'"),
        "it can grow to the four that remain from its start: {second:?}"
    );
    for (bad, says) in [
        (
            "offset=6",
            "offset=6 is past the last of the 6 matching item(s)",
        ),
        ("offset=ten", "`ten` is not a number of items to skip"),
    ] {
        let refused = server.page(&format!("/l/default?{bad}"), None);
        assert_eq!(refused.status, 400, "{bad}: {refused:?}");
        assert!(refused.body.contains(says), "{bad}: {refused:?}");
    }
}

#[test]
fn a_browser_gets_html_pages_and_a_readable_404() {
    let server = Server::start();
    for path in ["/", "/l/default", "/sparql"] {
        let page = server.page(path, None);
        assert_eq!(page.status, 200, "{path}: {page:?}");
        assert!(page.head.contains("text/html"), "{path}: {page:?}");
        assert!(page.body.starts_with("<!DOCTYPE html>"), "{path}: {page:?}");
        // ★ The HTML serialization, not XML's: a self-closed script would swallow the page.
        // (xrust writes attributes in name order, so nothing here depends on their order.)
        assert!(
            page.body.contains("src='/static/htmx.min.js'></script>"),
            "{path}: {page:?}"
        );
        for tag in ["script", "textarea", "div", "section", "ol", "select"] {
            let self_closed = page
                .body
                .split(&format!("<{tag}"))
                .skip(1)
                .any(|rest| rest.split('>').next().unwrap_or("").ends_with('/'));
            assert!(!self_closed, "{path}: a <{tag}> was self-closed: {page:?}");
        }
    }
    let home = server.page("/", None);
    assert!(home.body.contains("No items match."), "{home:?}");
    assert!(
        home.body.contains("File an item"),
        "anonymous loopback may write: {home:?}"
    );
    let fragment = server.page("/l/default/items?status=closed", None);
    assert_eq!(fragment.status, 200, "{fragment:?}");
    assert!(
        fragment
            .body
            .starts_with("<section class='ledger' id='ledger'>"),
        "a fragment is the section alone, no page around it: {fragment:?}"
    );
    for asset in ["gonk.css", "gonk.js", "htmx.min.js"] {
        let got = server.raw("GET", &format!("/static/{asset}"), &[], "");
        assert_eq!(got.status, 200, "{asset}: {got:?}");
    }

    // ★ The hub's finding: an unrouted path was a 500 echoing the resolver. It is a 404, and
    // since ledger #3 it is the LIBRARY's 404 — gonk's own catch-all is gone, so this is the
    // pin that `ikigai-web` still maps the kernel's `Unresolved` to 404 and not to 500. The
    // body is the kernel's sentence, naming the IRI the path became.
    for (path, iri) in [
        ("/favicon.ico", "urn:favicon.ico"),
        ("/no/such/page", "urn:no:such:page"),
    ] {
        let missing = server.page(path, None);
        assert_eq!(missing.status, 404, "{path}: {missing:?}");
        assert!(
            missing
                .body
                .contains(&format!("no endpoint resolved for {iri}")),
            "{missing:?}"
        );
    }
    // Every verb, not only a read: a write or a delete to a name nothing binds is a 404 too,
    // never a 500 and never a `204` from the DELETE tombstone path (which answers only a
    // bound endpoint's `NotFound`).
    for method in ["POST", "DELETE"] {
        let missing = server.raw(method, "/no/such/page", &[], "");
        assert_eq!(missing.status, 404, "{method}: {missing:?}");
    }
    let missing_item = server.page("/l/default/item/0000000000zzzzzz", None);
    assert_eq!(missing_item.status, 404, "{missing_item:?}");
}

/// ★ **A hostile ledger link is refused and never resolved: at the edge when it carries a
/// delimiter, at the page when it is merely not a ledger name.**
///
/// `rules::percent` escapes a ledger name before it goes into a link, so a hostile name
/// `../../etc` becomes `/l/..%2F..%2Fetc/item/244` (pinned in `rules`'s own unit test). Where
/// that link is refused has moved with `ikigai-web` twice:
///
/// - Through 0.1.29 the decoder turned `%2F` back into a separator, the path had six segments,
///   no route matched, and the mechanical mapping answered.
/// - From 0.1.30 an encoded slash is data inside its segment (RFC 3986 §2.2), so the path had
///   FOUR segments and matched `/l/{ledger}/item/{id}` with the ledger `../../etc`; what
///   refused it was the page, through `Ledger::parse`.
/// - From 0.1.37 (ledger #740) a route variable is STRICT: `{ledger}` refuses a decoded value
///   carrying any of `: / ? # [ ] @` with a FINAL `400` that names the variable, so neither a
///   later route nor the mechanical mapping answers, and the page never sees the name.
///
/// So this pins two refusals, on every page shape that takes a `{ledger}`: a name carrying a
/// delimiter (`..%2F..%2Fetc`, `default%2F`) is refused at the EDGE, and a name that is only
/// wrong by the ledger's own rule (`Default`: lowercase letters, digits, `-` and `_`) still
/// reaches the page and is refused THERE. Both are a `400` naming the rule, and neither reads
/// anything.
///
/// A `:` is pinned beside the slash because it is the character that could re-split the page
/// IRI, `urn:iki:gonk:page:item:{ledger}:{id}`. Through 0.1.36 it was not refused: the template
/// bound `{ledger}` to the colon-free run before the first `:`, so `default%3Aitem%3A1` read as
/// the ledger `default` with the item id `item:1:1` and answered `404`. That could not reach
/// another ledger, but it resolved a name the link never spelled. Strict `{ledger}` closes the
/// re-split at the edge, and `{id}` is strict the same way.
#[test]
fn a_hostile_ledger_link_is_refused_never_resolved() {
    let server = Server::start();
    let item = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "append"),
            ("content", "the one real item"),
        ],
        None,
    );
    assert_eq!(item.status, 200, "{item:?}");

    let pages = |name: &str| {
        [
            format!("/l/{name}"),
            format!("/l/{name}/items"),
            format!("/l/{name}/item/1"),
            format!("/l/{name}/item/1/card"),
        ]
    };

    // What the renderer emits for a hostile ledger name — the exact href `rules` pins — and
    // the other delimiter-carrying spellings: refused by the edge, before any page runs.
    let hostile = ikigai_gonk::rules::percent("../../etc");
    assert_eq!(hostile, "..%2F..%2Fetc");
    let colon = ikigai_gonk::rules::percent("default:item:1");
    assert_eq!(colon, "default%3Aitem%3A1");
    for (name, delim) in [
        (hostile.as_str(), '/'),
        ("default%2F", '/'),
        (colon.as_str(), ':'),
    ] {
        for path in pages(name) {
            let got = server.page(&path, None);
            assert_eq!(got.status, 400, "{path}: {got:?}");
            assert!(
                got.body
                    .contains(&format!("route variable {{ledger}} cannot carry '{delim}'")),
                "refused at the edge, naming the variable: {path}: {got:?}"
            );
            assert!(
                !got.body.contains("is not a ledger name"),
                "{path}: {got:?}"
            );
            assert!(!got.body.contains("the one real item"), "{path}: {got:?}");
        }
    }
    let got = server.page("/l/default/item/1%3A1", None);
    assert_eq!(got.status, 400, "{got:?}");
    assert!(
        got.body.contains("route variable {id} cannot carry ':'"),
        "{got:?}"
    );

    // A name with no delimiter in it passes the edge, and the page refuses it by the
    // ledger's own rule.
    for path in pages("Default") {
        let got = server.page(&path, None);
        assert_eq!(got.status, 400, "{path}: {got:?}");
        assert!(got.body.contains("is not a ledger name"), "{path}: {got:?}");
        assert!(!got.body.contains("the one real item"), "{path}: {got:?}");
    }

    // …and the edge refuses a malformed escape before gonk sees the request at all.
    for path in ["/l/%zz/item/1", "/l/default/item/%", "/l/%E9/item/1"] {
        let got = server.page(path, None);
        assert_eq!(got.status, 400, "{path}: {got:?}");
    }
}

#[test]
fn a_person_files_edits_comments_and_closes_through_forms() {
    let server = Server::start();
    let (iri, id) = file(
        &server,
        "Wire gonk <into> the cli\n\nIt needs a README & tests.",
    );
    let listing = server.page("/", None);
    assert!(
        listing.body.contains("Wire gonk &lt;into&gt; the cli"),
        "a title is escaped, not interpreted: {listing:?}"
    );

    let item = server.page(&format!("/l/default/item/{id}"), None);
    assert_eq!(item.status, 200, "{item:?}");
    assert!(
        item.body.contains("It needs a README &amp; tests."),
        "{item:?}"
    );
    assert!(item.body.contains("Work on it"), "{item:?}");
    assert!(
        !item.body.contains("id='delete-title'") && !item.body.contains("id='purge-title'"),
        "an anonymous caller holds neither delete nor purge, so neither is offered: {item:?}"
    );

    let edited = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "item"),
            ("_id", &id),
            ("_then", "card"),
            ("content", "Wire gonk into the cli\n\nRenamed body."),
            ("priority", "1"),
        ],
        None,
    );
    assert_eq!(edited.status, 200, "{edited:?}");
    assert!(
        edited
            .body
            .starts_with("<article class='item-page' id='item'>"),
        "{edited:?}"
    );
    assert!(
        edited.body.contains("Renamed body.") && edited.body.contains("p1"),
        "{edited:?}"
    );

    let commented = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "comment"),
            ("_id", &id),
            ("_then", "card"),
            ("item", &iri),
            ("content", "Looked at it."),
        ],
        None,
    );
    assert_eq!(commented.status, 200, "{commented:?}");
    assert!(commented.body.contains("Looked at it."), "{commented:?}");

    let closed = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "close"),
            ("_verb", "Sink"),
            ("_id", &id),
            ("_then", "card"),
            ("item", &iri),
            ("reason", "wontfix"),
        ],
        None,
    );
    assert_eq!(closed.status, 200, "{closed:?}");
    assert!(closed.body.contains("closed · wontfix"), "{closed:?}");
    assert!(
        closed.body.contains(">Reopen<"),
        "a closed item offers reopen: {closed:?}"
    );
    let open_list = server.page("/l/default/items", None);
    assert!(open_list.body.contains("No items match."), "{open_list:?}");
    let closed_list = server.page("/l/default/items?status=closed", None);
    assert!(
        closed_list.body.contains("Wire gonk into the cli"),
        "{closed_list:?}"
    );

    // Deleting needs the delete grant, which an anonymous caller does not hold.
    let refused = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "item"),
            ("_verb", "Delete"),
            ("_id", &id),
            ("_then", "gone"),
        ],
        None,
    );
    assert_eq!(refused.status, 403, "{refused:?}");
    assert!(
        refused.body.contains("urn:cap:ledger:delete"),
        "{refused:?}"
    );
}

/// Ledger [#1501](http://localhost:1062/l/default/item/1501): an item's page lists its
/// CHILDREN, the inbound `ledger:parent` edges ikigai-ledger 0.12.0 puts in the item's Turtle
/// face, as links into each child's page; a child's page names its parent and lists none.
#[test]
fn an_item_page_lists_its_children_as_links() {
    let server = Server::start();
    let (parent_iri, parent_id) = file(&server, "The epic");
    let mut children = Vec::new();
    for title in ["First step", "Second step"] {
        let (child_iri, child_id) = file(&server, title);
        let linked = server.form(
            &[
                ("_ledger", "default"),
                ("_action", "link"),
                ("_verb", "Sink"),
                ("_id", &child_id),
                ("_then", "card"),
                ("item", &child_iri),
                ("type", "parent"),
                ("content", &parent_iri),
            ],
            None,
        );
        assert_eq!(linked.status, 200, "{linked:?}");
        children.push(child_id);
    }
    children.sort();

    let page = server.page(&format!("/l/default/item/{parent_id}"), None);
    assert_eq!(page.status, 200, "{page:?}");
    let section = page
        .body
        .split("<section aria-labelledby='children-title' class='panel children'>")
        .nth(1)
        .unwrap_or_else(|| panic!("no Children section: {page:?}"));
    let section = &section[..section.find("</section>").expect("a closed section")];
    assert!(
        section.contains("<h2 id='children-title'>Children</h2>"),
        "{section}"
    );
    let links: Vec<&str> = section.matches("<li><a href=").collect();
    assert_eq!(links.len(), 2, "one link per child: {section}");
    let first = section
        .find(&format!(
            "<a href='/l/default/item/{}'>default {}</a>",
            children[0], children[0]
        ))
        .unwrap_or_else(|| panic!("no link to {}: {section}", children[0]));
    let second = section
        .find(&format!(
            "<a href='/l/default/item/{}'>default {}</a>",
            children[1], children[1]
        ))
        .unwrap_or_else(|| panic!("no link to {}: {section}", children[1]));
    assert!(first < second, "children in a stable order: {section}");

    // The child names its parent among its links, and has no children of its own.
    let child = server.page(&format!("/l/default/item/{}", children[0]), None);
    assert_eq!(child.status, 200, "{child:?}");
    assert!(
        child.body.contains(&format!(
            "<a href='/l/default/item/{parent_id}'>default {parent_id}</a>"
        )),
        "{child:?}"
    );
    assert!(!child.body.contains("children-title"), "{child:?}");
}

#[test]
fn a_form_can_send_only_what_the_ledger_declares() {
    let server = Server::start();
    let (iri, _) = file(&server, "A target");
    let smuggled = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "close"),
            ("item", &iri),
            ("graph", "urn:iki:ledger:graph:acme"),
        ],
        None,
    );
    assert_eq!(smuggled.status, 400, "{smuggled:?}");
    assert!(
        smuggled.body.contains("does not declare this input"),
        "{smuggled:?}"
    );
    let elsewhere = server.form(&[("_ledger", "default"), ("_action", "update")], None);
    assert_eq!(elsewhere.status, 400, "{elsewhere:?}");
    let wrong_verb = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "append"),
            ("_verb", "Delete"),
        ],
        None,
    );
    assert_eq!(wrong_verb.status, 400, "{wrong_verb:?}");
    let other_ledger = server.form(
        &[("_ledger", "acme"), ("_action", "append"), ("content", "x")],
        None,
    );
    assert_eq!(other_ledger.status, 403, "{other_ledger:?}");
}

#[test]
fn a_cross_site_write_and_a_rebound_host_get_nothing() {
    let server = Server::start();
    let body = "_ledger=default&_action=append&content=forged";
    let urlencoded = (
        "Content-Type",
        "application/x-www-form-urlencoded".to_string(),
    );
    let forged = server.raw(
        "POST",
        "/act",
        &[
            ("Origin", "https://evil.example".to_string()),
            urlencoded.clone(),
        ],
        body,
    );
    assert_eq!(forged.status, 403, "a foreign Origin: {forged:?}");
    let forged = server.raw(
        "POST",
        "/iki/ledger/append",
        &[("Sec-Fetch-Site", "cross-site".to_string())],
        "forged",
    );
    assert_eq!(
        forged.status, 403,
        "the first arc's door is covered too: {forged:?}"
    );
    let rebound = server.raw(
        "GET",
        "/iki/ledger/items",
        &[("Host", format!("evil.example:{}", server.addr.port()))],
        "",
    );
    assert_eq!(
        rebound.status, 421,
        "a rebound Host reads nothing, and is told it named another server: {rebound:?}"
    );
    // ★ Not even the page: a rebound Host is REFUSED before dispatch (ledger #864, R2, and
    // PENDING item 2). It used to be answered with an empty capability, which still served
    // every capability-free resource — this page, and the passkey ceremonies.
    let home = server.raw(
        "GET",
        "/",
        &[
            ("Host", "evil.example".to_string()),
            ("Accept", CHROME_ACCEPT.to_string()),
        ],
        "",
    );
    assert_eq!(home.status, 421, "{home:?}");
    assert!(home.body.contains("Host"), "the refusal says why: {home:?}");

    // A local process — no Origin, no Sec-Fetch-Site — is what the door was always for.
    let curl = server.raw("POST", "/iki/ledger/append", &[], "Filed by curl");
    assert_eq!(curl.status, 200, "{curl:?}");
}

/// ★ A caller who may read no ledger reads a sentence first (ledger #739): the front page's
/// notice is `web::Lacking`'s, signed-out sentence visible and free of capability IRIs, the
/// signed-in sentence and its exact grant hidden for the script to show. And a ledger page
/// refused outright — a typed 403 in plain text, with no page to carry a button — says the
/// same thing in the same order: the words, then the grant.
#[test]
fn a_caller_short_of_a_ledger_grant_reads_a_sentence_before_a_grant() {
    // An anonymous caller holding no ledger grant, so the front page has no ledger to show.
    let bare = Server::start_configured(ikigai_gonk::rules::DEFAULT_RULES, Vec::new());
    let home = bare.raw("GET", "/", &[("Accept", CHROME_ACCEPT.to_string())], "");
    let body = home.body.replace("&apos;", "'");
    let notice = &body[body
        .find("data-signin-notice")
        .unwrap_or_else(|| panic!("no sign-in notice: {home:?}"))..];
    let (seen, rest) = notice.split_once("</p>").expect("the signed-out paragraph");
    assert!(seen.contains("Sign in to read the ledgers."), "{seen}");
    assert!(!seen.contains("urn:cap:"), "no IRI is read first: {seen}");
    assert!(
        !body.contains("holds no grant"),
        "the old sentence is gone: {body}"
    );
    let (inside, _) = rest.split_once("</details>").expect("the grant footnote");
    assert!(
        inside.contains("class='signed-in' hidden='hidden'"),
        "{inside}"
    );
    assert!(
        inside.contains("Your passkey's grant doesn't include any ledger."),
        "{inside}"
    );
    assert!(
        inside.contains("<code>urn:cap:ledger:read:&lt;ledger&gt;</code>"),
        "{inside}"
    );

    // A ledger this browser's grant does not name: the plain-text refusal. (Past the page's
    // floor, `urn:cap:ledger:read:*`, which the default anonymous grant satisfies.)
    let server = Server::start();
    let refused = server.page("/l/acme", None);
    assert_eq!(refused.status, 403, "{refused:?}");
    let lines: Vec<&str> = refused.body.lines().collect();
    assert_eq!(
        lines.first().copied(),
        // `denied: ` is `ikigai-web`'s prefix for a typed refusal, not gonk's word.
        Some("denied: Sign in to read the ledger \"acme\"."),
        "{refused:?}"
    );
    assert!(
        refused.body.contains(
            "Signed in already? Your passkey's grant doesn't include the ledger \"acme\"."
        ),
        "{refused:?}"
    );
    assert!(
        lines
            .last()
            .is_some_and(|l| l.contains("urn:cap:ledger:read:acme")),
        "the grant comes LAST: {refused:?}"
    );

    // No root configured is the operator's sentence, unchanged, on both pages that say it —
    // there is nothing to sign in FOR, so no notice offers to.
    for (path, sentence) in [
        (
            "/browse",
            "no browse root configured (`gonk.browse.root`), so there is nothing to browse",
        ),
        (
            "/queue",
            "no browse root configured (`gonk.browse.root`), so no review pass can run",
        ),
    ] {
        let page = server.page(path, None);
        assert_eq!(page.status, 200, "{path}: {page:?}");
        assert!(page.body.contains(sentence), "{path}: {page:?}");
        assert!(
            !page.body.contains("data-signin-notice"),
            "{path}: {page:?}"
        );
    }
}

#[test]
fn sparql_is_confined_to_one_ledger_graph() {
    let server = Server::start();
    file(&server, "Queryable title");
    let query =
        "PREFIX dcterms: <http://purl.org/dc/terms/> SELECT ?t WHERE { ?i dcterms:title ?t }";

    let json = server.raw(
        "GET",
        &format!("/sparql?query={}", encode(query)),
        &[("Accept", "application/sparql-results+json".to_string())],
        "",
    );
    assert_eq!(json.status, 200, "{json:?}");
    assert!(json.body.contains("Queryable title"), "{json:?}");

    let fragment = server.page(&format!("/sparql/results?query={}", encode(query)), None);
    assert_eq!(fragment.status, 200, "{fragment:?}");
    assert!(
        fragment.body.contains("<table>") && fragment.body.contains("Queryable title"),
        "{fragment:?}"
    );
    assert!(
        fragment
            .body
            .contains("1 row(s) from urn:iki:ledger:graph:default"),
        "{fragment:?}"
    );

    let from = "SELECT * FROM <urn:iki:ledger:graph:acme> WHERE { ?s ?p ?o }";
    let refused = server.page(&format!("/sparql?query={}", encode(from)), None);
    assert_eq!(refused.status, 200, "{refused:?}");
    assert!(refused.body.contains("FROM"), "{refused:?}");
    assert!(refused.body.contains("class='flash error'"), "{refused:?}");

    let other = server.page(
        &format!("/sparql?ledger=acme&query={}", encode(query)),
        None,
    );
    assert_eq!(other.status, 403, "no grant over acme's graph: {other:?}");
    let update = server.page(
        &format!("/sparql/results?query={}", encode("DROP ALL")),
        None,
    );
    assert!(
        update.body.contains("Updates are not accepted"),
        "{update:?}"
    );
}

/// A corpus in the shape `ikigai-ledger` writes: priorities set and unset, `repo:` labels,
/// a `security` label, comments, and a closed item with its reason. Enough for every sample
/// query on the SPARQL page to have something true to return. Returns the filed items as
/// `(iri, id)`, in filing order — so item N is `filed[N - 1]`.
fn seed(server: &Server) -> Vec<(String, String)> {
    let filed: Vec<(String, String)> = [
        (
            "Refuse a foreign Host\n\nThe edge hands an empty capability instead of refusing.",
            "1",
            "repo:ikigai-gonk,security",
        ),
        (
            "Drop the HTTP workarounds\n\nThe passkey path is settled; the pin can move.",
            "1",
            "repo:ikigai-gonk",
        ),
        (
            "No passkey list or revoke command\n\nA passkey cannot be named or removed.",
            "2",
            "repo:ikigai-gonk,security",
        ),
        (
            "Dependency floors never re-resolved at the floor\n\nA caret resolve says nothing.",
            "3",
            "repo:ikigai-core",
        ),
        (
            "Forms are hand-written in the stylesheet\n\nNot rendered from the manifold.",
            "",
            "repo:ikigai-core",
        ),
    ]
    .iter()
    .map(|&(content, priority, labels)| {
        let mut fields = vec![
            ("_ledger", "default"),
            ("_action", "append"),
            ("_then", "items"),
            ("content", content),
            ("labels", labels),
        ];
        if !priority.is_empty() {
            fields.push(("priority", priority));
        }
        let filed = server.form(&fields, None);
        assert_eq!(filed.status, 200, "{filed:?}");
        first_item(&filed.body)
    })
    .collect();

    for (iri, id) in filed.iter().take(2) {
        let commented = server.form(
            &[
                ("_ledger", "default"),
                ("_action", "comment"),
                ("_id", id),
                ("_then", "card"),
                ("item", iri),
                ("content", "Looked at it; still open."),
            ],
            None,
        );
        assert_eq!(commented.status, 200, "{commented:?}");
    }

    let (iri, id) = &filed[4];
    let closed = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "close"),
            ("_verb", "Sink"),
            ("_id", id),
            ("_then", "card"),
            ("item", iri),
            ("reason", "superseded"),
        ],
        None,
    );
    assert_eq!(closed.status, 200, "{closed:?}");
    filed
}

/// One more comment on one item, so a test can make a COUNT and an item number differ.
fn comment(server: &Server, item: &(String, String), text: &str) {
    let (iri, id) = item;
    let posted = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "comment"),
            ("_id", id),
            ("_then", "card"),
            ("item", iri),
            ("content", text),
        ],
        None,
    );
    assert_eq!(posted.status, 200, "{posted:?}");
}

/// ★ **The samples are RUN here, not merely rendered.** A sample query that errors is worse
/// than no sample query, and nothing else in this repo would notice: the page renders a store
/// refusal as HTML with a 200, so a broken sample looks exactly like a working one to any
/// test that only checks the status. Each one must come back with at least one row AND no
/// `view:error`.
#[test]
fn every_sample_query_returns_rows() {
    let server = Server::start();
    seed(&server);
    for (id, label, query) in ikigai_gonk::web::SAMPLES {
        let json = server.raw(
            "GET",
            &format!("/sparql?query={}", encode(query)),
            &[("Accept", "application/sparql-results+json".to_string())],
            "",
        );
        assert_eq!(json.status, 200, "{label}: {json:?}");
        let parsed: serde_json::Value =
            serde_json::from_str(&json.body).unwrap_or_else(|e| panic!("{label}: {e}: {json:?}"));
        let rows = parsed["results"]["bindings"]
            .as_array()
            .unwrap_or_else(|| panic!("{label}: no bindings: {json:?}"))
            .len();
        assert!(rows > 0, "{label} ({id}) returned no rows: {json:?}");
        println!("sample `{label}` ({id}): {rows} row(s)");

        // And through the page, where a refusal would be rendered rather than raised.
        let fragment = server.page(&format!("/sparql/results?query={}", encode(query)), None);
        assert_eq!(fragment.status, 200, "{label}: {fragment:?}");
        assert!(
            !fragment.body.contains("class='flash error'"),
            "{label}: the page rendered an error: {fragment:?}"
        );
        assert!(
            fragment.body.contains(&format!("{rows} row(s) from")),
            "{label}: the HTML face disagrees with the JSON one about the row count: \
             {fragment:?}"
        );
    }
}

#[test]
fn the_sparql_page_offers_the_samples_without_running_them() {
    let server = Server::start();
    seed(&server);
    let page = server.page("/sparql", None);
    assert_eq!(page.status, 200, "{page:?}");
    for (id, label, query) in ikigai_gonk::web::SAMPLES {
        assert!(
            page.body.contains(&format!("data-query='{id}'")),
            "no button for {label}: {page:?}"
        );
        assert!(page.body.contains(&format!(">{label}<")), "{page:?}");
        // The query text reaches the browser intact — newlines and all, which is why it
        // travels as element text and not as an attribute.
        let first_line = query.lines().next().unwrap();
        assert!(
            page.body.contains(&format!("id='{id}'")),
            "no text block for {label}: {page:?}"
        );
        assert!(
            page.body
                .contains(&first_line.replace('<', "&lt;").replace('>', "&gt;")),
            "{label}: the query text did not survive the render: {page:?}"
        );
    }
    assert!(
        page.body.contains("class='sample-text'"),
        "the query text blocks are rendered: {page:?}"
    );
    // The page says what `dcterms:created` actually means, because the ledger cannot answer
    // "the oldest p1" and a button labeled that way would be a lie.
    assert!(
        page.body.contains(
            &ikigai_gonk::web::CREATED_IS_FILING_TIME
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        ),
        "{page:?}"
    );
    // Nothing ran: the editor holds the default query and no results table is present.
    assert!(!page.body.contains("<table>"), "{page:?}");
    assert!(!page.body.contains("row(s) from"), "{page:?}");
}

/// ★ The cross-graph example is SHOWN and must never become loadable (#401).
///
/// The page sends one graph, so a multi-graph query pressed into the editor returns no rows —
/// which would present a real capability of `urn:iki:store:graph-select` as a broken feature.
/// The prose is what drifted here and prose is not testable; the MARKUP constraint is, so that
/// is what this pins: the example reaches the reader, and the only things a click can load are
/// the [`SAMPLES`](ikigai_gonk::web::SAMPLES) themselves.
#[test]
fn the_cross_graph_example_is_shown_and_never_loadable() {
    let server = Server::start();
    seed(&server);
    let page = server.page("/sparql", None);
    assert_eq!(page.status, 200, "{page:?}");

    // It is there, as its own block, and it says why it is not a button.
    assert!(page.body.contains("class='cross-graph'"), "{page:?}");
    assert!(page.body.contains("shown, not loadable"), "{page:?}");
    // The query survives the render — both graphs named, and the FILTER at the top level of
    // the WHERE, which is the difference between 39 rows and zero (see web::CROSS_GRAPH).
    for line in [
        "GRAPH <urn:iki:browse:graph:default> {",
        "GRAPH <urn:iki:ledger:graph:default> {",
        "FILTER(?label = CONCAT(\"repo:ikigai-\", ?repo))",
    ] {
        let escaped = line
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        assert!(
            page.body.contains(&escaped),
            "the cross-graph example lost `{line}`: {page:?}"
        );
    }

    // ⚠ The pin that matters: nothing clickable was added. `web/gonk.js` loads a query by
    // looking a button's `data-query` up as an element id, so one `data-query` per sample and
    // no more is exactly "the example cannot be loaded into the editor".
    assert_eq!(
        page.body.matches("data-query='").count(),
        ikigai_gonk::web::SAMPLES.len(),
        "only the samples are loadable: {page:?}"
    );
}

#[test]
fn a_ledger_iri_reads_as_its_local_name_only_in_the_html_face() {
    let server = Server::start();
    seed(&server);
    let query = "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#> \
                 SELECT ?status ?item WHERE { ?item a ledger:Item ; ledger:status ?status } \
                 ORDER BY ?item LIMIT 1";

    let fragment = server.page(&format!("/sparql/results?query={}", encode(query)), None);
    assert_eq!(fragment.status, 200, "{fragment:?}");
    // ★ #241's answer for a cell that does NOT become a link: the shortening is a CURIE,
    // and the prefix it uses is bound once in the page as TEXT. The `title` is still there
    // for a pointer user, but it is no longer the only place the full IRI exists — which
    // was the whole complaint, since screen readers mostly do not announce it.
    assert!(
        fragment.body.contains(
            "<td class='uri' title='https://ikigai-rs.dev/ns/ledger#open'>ledger:open</td>"
        ),
        "a ledger IRI reads as a CURIE, with the full IRI on the cell: {fragment:?}"
    );
    assert!(
        fragment.body.contains(&format!(
            "<p class='result-meta legend'>{}</p>",
            ikigai_gonk::web::PREFIX_LEGEND
        )),
        "the prefix is bound once, in text: {fragment:?}"
    );
    // And exactly once, however many rows shortened an IRI.
    assert_eq!(
        fragment.body.matches("result-meta legend").count(),
        1,
        "{fragment:?}"
    );
    // An item's own IRI is the identifier a person copies: never shortened — and now a
    // LINK, which is a better answer to #241 than a tooltip ever was.
    assert!(
        fragment
            .body
            .contains("<td class='uri'><a aria-label='Open urn:iki:ledger:default:item:"),
        "{fragment:?}"
    );
    assert!(
        fragment
            .body
            .contains("class='cell-link' data-action='link' href='/l/default/item/"),
        "{fragment:?}"
    );
    // A linked cell drops the tooltip rather than carrying both.
    assert!(
        !fragment.body.contains("title='urn:iki:ledger:"),
        "{fragment:?}"
    );

    // ★ The bytes the store returned are untouched: this is a DISPLAY change in one face.
    let json = server.raw(
        "GET",
        &format!("/sparql?query={}", encode(query)),
        &[("Accept", "application/sparql-results+json".to_string())],
        "",
    );
    assert_eq!(json.status, 200, "{json:?}");
    assert!(
        json.body.contains("https://ikigai-rs.dev/ns/ledger#open"),
        "the JSON face still carries the full IRI: {json:?}"
    );
}

/// ★★ **THE ACCEPTANCE TEST** — the `Items with comments` query, which is the one that was
/// on screen when this work was asked for. It renders `number` and `comments` side by side,
/// both as bare integers, and the whole difficulty of "any query" lives in that pair.
///
/// ⚠ The counterexample is made SHARP here on purpose: the counts are 3 and 2 while the
/// numbers are 1 and 2, and items 2 and 3 both EXIST. So a renderer that linked every
/// integer, or that linked an integer only when an item with that number exists, would send
/// "3 comments" to item #3. Neither would fail a gentler corpus; both fail this one.
#[test]
fn the_issues_with_comments_query_links_numbers_and_leaves_counts_alone() {
    let server = Server::start();
    let filed = seed(&server);
    // seed() leaves one comment each on items 1 and 2. Make the counts 3 and 2.
    comment(&server, &filed[0], "Second look.");
    comment(&server, &filed[0], "Third look.");
    comment(&server, &filed[1], "Second look.");

    let (_, _, query) = ikigai_gonk::web::SAMPLES
        .iter()
        .find(|(id, _, _)| *id == "sample-comments")
        .expect("the issues-with-comments sample");
    let fragment = server.page(&format!("/sparql/results?query={}", encode(query)), None);
    assert_eq!(fragment.status, 200, "{fragment:?}");

    // The item numbers are links.
    for number in [1, 2] {
        assert!(
            fragment
                .body
                .contains(&format!("href='/l/default/item/{number}'")),
            "item {number} is not linked: {fragment:?}"
        );
    }
    // ★ And the COUNT is not — even though item 3 exists and is reachable by its own number.
    assert!(
        !fragment.body.contains("href='/l/default/item/3'"),
        "a comment count of 3 was linked to item #3: {fragment:?}"
    );
    assert!(
        fragment.body.contains("<td class='literal'>3</td>"),
        "the count renders as plain text: {fragment:?}"
    );
    // The accessible name contains the visible text, so the link is not renamed out from
    // under a screen-reader user (WCAG 2.5.3).
    assert!(
        fragment
            .body
            .contains("<a aria-label='Open item 1 in default' class='cell-link' data-action='link' href='/l/default/item/1'>1</a>"),
        "{fragment:?}"
    );
    // Title stays plain text: an unmatched cell is exactly what it was.
    assert!(
        fragment
            .body
            .contains("<td class='literal'>Refuse a foreign Host</td>"),
        "{fragment:?}"
    );
}

/// ★ #315: a repo label is a control, and activating it LEAVES the current result set.
///
/// The `Count by repo` sample returns one row per repo and no item at all. Following the
/// `repo:ikigai-gonk` cell must answer with that repo's OPEN items — items the first query
/// never returned — because the point is to ask the ledger a new question rather than to
/// filter the rows already on screen. The closed one must not come back.
#[test]
fn a_repo_label_asks_the_ledger_the_next_question() {
    let server = Server::start();
    seed(&server);
    let (_, _, counts) = ikigai_gonk::web::SAMPLES
        .iter()
        .find(|(id, _, _)| *id == "sample-repos")
        .expect("the count-by-repo sample");
    let fragment = server.page(&format!("/sparql/results?query={}", encode(counts)), None);
    assert_eq!(fragment.status, 200, "{fragment:?}");
    assert!(
        fragment.body.contains("data-action='query'"),
        "a repo label is a query control: {fragment:?}"
    );
    assert!(
        fragment
            .body
            .contains("aria-label='Open items labeled repo:ikigai-gonk'"),
        "{fragment:?}"
    );

    // Follow it exactly as a browser would: the href, verbatim.
    let href = {
        let at = fragment
            .body
            .find("aria-label='Open items labeled repo:ikigai-gonk'")
            .expect("the repo cell");
        let rest = &fragment.body[at..];
        let start = rest.find("href='").expect("an href") + "href='".len();
        let end = rest[start..].find('\'').expect("the closing quote");
        rest[start..start + end].replace("&amp;", "&")
    };
    let followed = server.page(&href, None);
    assert_eq!(followed.status, 200, "{followed:?}");
    assert!(
        !followed.body.contains("class='flash error'"),
        "the query the rule built was refused: {followed:?}"
    );
    // The three OPEN ikigai-gonk items, and not the ikigai-core ones.
    assert!(
        followed.body.contains("Refuse a foreign Host"),
        "{followed:?}"
    );
    assert!(
        followed.body.contains("Drop the HTTP workarounds"),
        "{followed:?}"
    );
    assert!(followed.body.contains("No passkey list"), "{followed:?}");
    assert!(
        !followed.body.contains("Dependency floors"),
        "another repo's item came back: {followed:?}"
    );
    assert!(
        followed
            .body
            .contains("3 row(s) from urn:iki:ledger:graph:default"),
        "{followed:?}"
    );
}

/// ⚠ The label comes out of the store, and the store holds text other people wrote. A label
/// carrying a quote and a closing brace must not be able to change the SHAPE of the query it
/// runs — the whole value stays one string literal, so the query is well-formed and simply
/// matches nothing.
#[test]
fn a_label_that_looks_like_sparql_cannot_reshape_the_query_it_runs() {
    let server = Server::start();
    let hostile = r#"repo:x" } INSERT DATA { <urn:x:a> <urn:x:b> "pwned" } #"#;
    let filed = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "append"),
            ("_then", "items"),
            ("content", "A label that reads like a query\n\nbody"),
            ("labels", hostile),
        ],
        None,
    );
    assert_eq!(filed.status, 200, "{filed:?}");

    let query = "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>\n\
                 SELECT ?label WHERE { ?i a ledger:Item ; ledger:label ?label \
                 FILTER(STRSTARTS(?label, \"repo:\")) }";
    let fragment = server.page(&format!("/sparql/results?query={}", encode(query)), None);
    assert_eq!(fragment.status, 200, "{fragment:?}");
    assert!(
        fragment.body.contains("data-action='query'"),
        "{fragment:?}"
    );

    let href = {
        let start = fragment.body.find("href='").expect("an href") + "href='".len();
        let end = fragment.body[start..]
            .find('\'')
            .expect("the closing quote");
        fragment.body[start..start + end].replace("&amp;", "&")
    };
    let followed = server.page(&href, None);
    assert_eq!(followed.status, 200, "{followed:?}");
    // Well-formed, so no parse error — and matching nothing, because no item carries that
    // exact label... except the one that does, which is the point: it is DATA, not syntax.
    assert!(
        !followed.body.contains("class='flash error'"),
        "the escaped query did not parse: {followed:?}"
    );
    assert!(
        followed.body.contains("A label that reads like a query"),
        "the literal matched the item it names: {followed:?}"
    );
    // Nothing was written: the INSERT DATA inside the label is text.
    let count = server.raw(
        "GET",
        &format!(
            "/sparql?query={}",
            encode("SELECT (COUNT(*) AS ?n) WHERE { <urn:x:a> ?p ?o }")
        ),
        &[("Accept", "application/sparql-results+json".to_string())],
        "",
    );
    assert!(count.body.contains("\"value\":\"0\""), "{count:?}");
}

/// ★ The rule table is a RESOURCE: served, readable, and replaceable per deployment.
#[test]
fn the_render_rules_are_a_resource_a_deployment_can_replace() {
    let server = Server::start();
    let served = server.raw("GET", "/render-rules", &[], "");
    assert_eq!(served.status, 200, "{served:?}");
    assert!(
        served.body.contains("text/turtle"),
        "the table is served as Turtle: {served:?}"
    );
    assert!(
        served.body.contains("urn:iki:gonk:render:rule:item-number"),
        "{served:?}"
    );
    // What is served is what the renderer acted on, byte for byte.
    assert!(
        served.body.ends_with(ikigai_gonk::rules::DEFAULT_RULES),
        "{served:?}"
    );

    // A deployment whose queries spell the column differently writes its own table, and
    // gonk obeys THAT — no patch to this binary.
    let theirs = r#"@prefix render: <urn:iki:gonk:render#> .
<urn:x:rule:ticket> a render:Rule ;
    render:order 5 ;
    render:matchKind "literal" ;
    render:matchShape "integer" ;
    render:matchVar "ticket" ;
    render:link "/l/{ledger}/item/{value}" ;
    render:label "Open ticket {value}" .
"#;
    let server = Server::start_with_rules(theirs);
    seed(&server);
    let query = "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#> \
                 SELECT ?ticket ?number WHERE { ?i a ledger:Item ; ledger:number ?ticket \
                 BIND(?ticket AS ?number) } ORDER BY ?ticket LIMIT 1";
    let fragment = server.page(&format!("/sparql/results?query={}", encode(query)), None);
    assert_eq!(fragment.status, 200, "{fragment:?}");
    assert!(
        fragment.body.contains("aria-label='Open ticket 1'"),
        "their column links: {fragment:?}"
    );
    // And the shipped default is GONE, not merged: `number` is no longer special here.
    assert!(
        !fragment
            .body
            .contains("aria-label='Open item 1 in default'"),
        "the shipped table was merged rather than replaced: {fragment:?}"
    );
    assert!(
        server
            .raw("GET", "/render-rules", &[], "")
            .body
            .contains("urn:x:rule:ticket"),
        "the served table is the one in effect"
    );
}

fn challenge(server: &Server, op: &str) -> String {
    let got = server.json(&format!("/auth/{op}"), "{}");
    assert_eq!(got.status, 200, "{got:?}");
    let v: serde_json::Value = serde_json::from_str(&got.body).unwrap();
    v["challenge"].as_str().unwrap().to_string()
}

#[test]
fn a_passkey_identity_adds_its_grant_and_only_its_grant() {
    let server = Server::start();
    let (_, id) = file(&server, "Delete me once signed in");
    let authenticator = Authenticator::new();
    let invite = identity::invite(
        &server.layout,
        "brian",
        &grants_for("default", Authority::Delete).unwrap(),
        false,
        30,
        identity::now_seconds(),
    )
    .unwrap();

    // A ceremony run at the wrong origin is refused, and does not burn the invite.
    let c = challenge(&server, "register-options");
    let wrong = server.json(
        "/auth/register",
        &authenticator.register_body(&c, &invite, "http://127.0.0.1:1060"),
    );
    assert_eq!(wrong.status, 400, "{wrong:?}");
    assert!(wrong.body.contains("localhost"), "{wrong:?}");

    let c = challenge(&server, "register-options");
    let enrolled = server.json(
        "/auth/register",
        &authenticator.register_body(&c, &invite, &server.origin()),
    );
    assert_eq!(enrolled.status, 200, "{enrolled:?}");
    // ★ One grant table: the passkey sits in the same clients.json the QUIC door reads, and
    // that file still parses as the QUIC door's enrollment.
    let clients = std::fs::read_to_string(server.layout.clients_json()).unwrap();
    assert!(
        clients.contains("\"passkeys\"") && clients.contains("\"grant\": \"brian\""),
        "{clients}"
    );
    quic::parse_enrolment(&clients).unwrap();
    let c = challenge(&server, "register-options");
    let reused = server.json(
        "/auth/register",
        &Authenticator {
            key: SigningKey::from_bytes(&[0x43u8; 32].into()).unwrap(),
            id: b"another".to_vec(),
        }
        .register_body(&c, &invite, &server.origin()),
    );
    assert_eq!(reused.status, 400, "an invite is single use: {reused:?}");

    let c = challenge(&server, "login-options");
    let login = server.json(
        "/auth/login",
        &authenticator.login_body(&c, &server.origin(), 1),
    );
    assert_eq!(login.status, 200, "{login:?}");
    let v: serde_json::Value = serde_json::from_str(&login.body).unwrap();
    let token = v["session"].as_str().unwrap().to_string();
    let replay = server.json(
        "/auth/login",
        &authenticator.login_body(&c, &server.origin(), 2),
    );
    assert_eq!(replay.status, 403, "a challenge answers once: {replay:?}");

    let who = server.json("/auth/session", &token);
    assert_eq!(who.status, 200, "{who:?}");
    assert!(who.body.contains("\"grant\":\"brian\""), "{who:?}");

    // Signed in: delete is offered and allowed; purge is neither.
    let item = server.page(&format!("/l/default/item/{id}"), Some(&token));
    assert!(item.body.contains("id='delete-title'"), "{item:?}");
    assert!(!item.body.contains("id='purge-title'"), "{item:?}");
    let deleted = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "item"),
            ("_verb", "Delete"),
            ("_id", &id),
            ("_then", "gone"),
            ("reason", "tested"),
        ],
        Some(&token),
    );
    assert_eq!(deleted.status, 200, "{deleted:?}");
    assert!(deleted.body.contains("deleted #1"), "{deleted:?}");
    assert!(deleted.body.contains("recoverable"), "{deleted:?}");
    let gone = server.page(&format!("/l/default/item/{id}"), Some(&token));
    assert_eq!(gone.status, 404, "{gone:?}");

    // A forged or ended session is no identity.
    let (_, other) = file(&server, "Still here");
    let forged = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "item"),
            ("_verb", "Delete"),
            ("_id", &other),
        ],
        Some("not-a-session"),
    );
    assert_eq!(forged.status, 403, "{forged:?}");
    server.json("/auth/logout", &token);
    let after = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "item"),
            ("_verb", "Delete"),
            ("_id", &other),
        ],
        Some(&token),
    );
    assert_eq!(after.status, 403, "{after:?}");
}

#[test]
fn the_palette_clears_the_contrast_floor() {
    use ikigai_a11y::color::{ratio, Rgba};
    let css = include_str!("../web/gonk.css");
    let floor = 4.5;
    for scheme in ["light", "dark"] {
        let start = css
            .find(&format!("/* palette: {scheme} */"))
            .unwrap_or_else(|| panic!("no {scheme} palette block"));
        let block = &css[start..];
        let block = &block[..block.find("/* end palette */").expect("an end marker")];
        let token = |name: &str| {
            let at = block
                .find(&format!("--{name}:"))
                .unwrap_or_else(|| panic!("{scheme}: no --{name}"));
            let value = block[at + name.len() + 3..]
                .split(';')
                .next()
                .unwrap()
                .trim();
            Rgba::parse(value).unwrap_or_else(|e| panic!("{scheme} --{name} `{value}`: {e:?}"))
        };
        for (fg, bg) in [
            ("fg", "bg"),
            ("fg", "surface"),
            ("muted", "bg"),
            ("muted", "surface"),
            ("accent", "bg"),
            ("accent", "surface"),
            ("danger", "bg"),
            ("danger", "surface"),
            ("ok", "bg"),
            ("ok", "surface"),
            ("accent-fg", "accent"),
            ("danger-fg", "danger"),
        ] {
            let got = ratio(token(fg), token(bg));
            assert!(
                got >= floor,
                "{scheme}: --{fg} on --{bg} is {got:.2}:1, under the {floor}:1 floor"
            );
        }
    }
}

/// The CSS declaration in [`fn@a_result_column_is_never_narrower_than_its_own_word`]'s sights,
/// as it appears in the file.
const TABLE_CELL_RULE: &str = "th, td {";

/// ★ **#368, pinned as a rule about MIN-CONTENT and not about how a table looked once.**
///
/// `overflow-wrap: anywhere` on a result cell lowers the cell's *minimum* intrinsic width to a
/// single character — unlike `break-word`, which leaves min-content at the longest word — so
/// auto table layout was free to squeeze `number` into `nu/mb/er` and break `ledger:open` at
/// its colon. Nothing overflowed and no test could see it: the HTML was correct and the
/// browser was obeying the stylesheet.
///
/// Measured in Chrome before the change was written, on `SELECT ?item ?number ?priority
/// ?status ?title` against the live ledger: without the declaration the table's min-content is
/// 835px inside a 637px `.table-wrap`, the wrapper scrolls, and the document's own
/// `scrollWidth` stays at the viewport. So the long-IRI case the declaration was defending is
/// the case `.table-wrap` already exists to handle, and this test holds both halves together —
/// the declaration stays gone, and the wrapper that replaces it stays.
///
/// ⚠ The repair that suggests itself — scope `anywhere` to `td.uri`, "the class that holds
/// IRIs" — is wrong, and asserted against here: a shortened status CURIE wears that same
/// class, so it would go on breaking at its colon.
#[test]
fn a_result_column_is_never_narrower_than_its_own_word() {
    let css = include_str!("../web/gonk.css");
    let at = css.find(TABLE_CELL_RULE).expect("the result-cell rule");
    let rule = &css[at..at + css[at..].find('}').expect("an unterminated rule") + 1];
    assert!(
        !rule.contains("overflow-wrap"),
        "a result cell must set no overflow-wrap — `anywhere` collapses its min-content to one \
         character and `break-word` cannot fire in a table nothing caps: {rule}"
    );
    assert!(
        css.contains(".table-wrap { overflow-x: auto;"),
        "the wrapper that carries the long-IRI case instead must still scroll"
    );
    let th = css
        .find("\nth { background:")
        .map(|at| &css[at + 1..at + 1 + css[at + 1..].find('}').expect("unterminated") + 1])
        .expect("the th rule");
    assert!(
        th.contains("white-space: nowrap"),
        "a column header is a short label and must not wrap: {th}"
    );
    for line in css.lines() {
        let line = line.trim();
        if line.starts_with("td.uri") || line.starts_with(".uri") {
            assert!(
                !line.contains("overflow-wrap: anywhere"),
                "td.uri holds BOTH a full IRI and a shortened CURIE — break-anywhere there \
                 leaves the status column broken at its colon: {line}"
            );
        }
    }
}

/// ★ **#370: an active sample is a state a screen reader can read.**
///
/// The samples are mutually exclusive toggles, so the state is `aria-pressed` on the button
/// rather than a CSS class — a background tint alone says nothing to a reader that is not
/// looking at the page. The SERVER renders every one `false`, because a freshly served page
/// holds the default query and never a sample; only `web/gonk.js` ever writes `true`.
#[test]
fn a_sample_button_is_a_toggle_the_server_renders_unpressed() {
    let server = Server::start();
    seed(&server);
    let page = server.page("/sparql", None);
    assert_eq!(page.status, 200, "{page:?}");
    let unpressed = page.body.matches("aria-pressed='false'").count();
    assert_eq!(
        unpressed,
        ikigai_gonk::web::SAMPLES.len(),
        "every sample button carries an unpressed state: {page:?}"
    );
    assert!(
        !page.body.contains("aria-pressed='true'"),
        "a served page holds the default query, so no sample is active: {page:?}"
    );
    assert!(
        include_str!("../web/gonk.css").contains(".samples button[aria-pressed=\"true\"]"),
        "the stylesheet dresses the state the markup carries"
    );
}

/// ★ **The half of #370 that carries the design, and the only fence this repo can put around
/// it.** There is no JavaScript runtime in this test suite, so what is asserted is the WIRING
/// in `web/gonk.js`, not its effect: the editor's `input` event clears the pressed sample.
///
/// It is worth a source-shape test even so. Everything else about the feature is cosmetic and
/// self-evident on the screen; this is the part that is invisible when it breaks, and when it
/// breaks the page asserts something false — a badge claiming the editor holds a sample it no
/// longer holds. Verified for real in Chrome against a locally built binary before merging;
/// this keeps the listener from being quietly dropped afterwards.
#[test]
fn the_active_sample_is_cleared_by_typing_in_the_editor() {
    let js = include_str!("../web/gonk.js");
    let at = js
        .find("function wireSamples()")
        .expect("the sample wiring");
    let body = &js[at..];
    assert!(
        body.contains("box.addEventListener(\"input\""),
        "the pressed state must be cleared on `input` — not on blur, not on submit"
    );
    assert!(
        !body.contains("addEventListener(\"blur\"")
            && !body.contains("addEventListener(\"change\""),
        "`blur` and `change` are both too late: the claim is false from the first keystroke"
    );
    assert!(
        body.contains("aria-pressed"),
        "the state the stylesheet dresses is the state this function writes"
    );
}

/// Enroll [`Authenticator::new`] under `grant` holding `scopes`, sign it in, and return the
/// session token and the passkey's stable IRI — the `principal` the door stamps on a write.
fn sign_in(server: &Server, grant: &str, scopes: &[String]) -> (String, String) {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let authenticator = Authenticator::new();
    let invite = identity::invite(
        &server.layout,
        grant,
        scopes,
        false,
        30,
        identity::now_seconds(),
    )
    .unwrap();
    let c = challenge(server, "register-options");
    let enrolled = server.json(
        "/auth/register",
        &authenticator.register_body(&c, &invite, &server.origin()),
    );
    assert_eq!(enrolled.status, 200, "{enrolled:?}");
    let c = challenge(server, "login-options");
    let login = server.json(
        "/auth/login",
        &authenticator.login_body(&c, &server.origin(), 1),
    );
    assert_eq!(login.status, 200, "{login:?}");
    let v: serde_json::Value = serde_json::from_str(&login.body).unwrap();
    let token = v["session"].as_str().unwrap().to_string();
    let iri = identity::passkey_iri(&URL_SAFE_NO_PAD.encode(&authenticator.id));
    (token, iri)
}

/// The item and its comments as the STORE holds them — the ledger's Turtle face over the
/// mechanical route, where a display label has no business appearing.
fn turtle(server: &Server, id: &str) -> String {
    let got = server.raw(
        "GET",
        &format!("/iki/ledger/item/{id}"),
        &[("Accept", "text/turtle".to_string())],
        "",
    );
    assert_eq!(got.status, 200, "{got:?}");
    got.body
}

/// A comment through the form, as the caller `cookie` names (or nobody).
fn comment_as(server: &Server, iri: &str, id: &str, text: &str, cookie: Option<&str>) -> Response {
    server.form(
        &[
            ("_ledger", "default"),
            ("_action", "comment"),
            ("_id", id),
            ("_then", "card"),
            ("item", iri),
            ("content", text),
        ],
        cookie,
    )
}

/// ★ Ledger #76: a write from a signed-in browser carries its author. The door already knew
/// who the session was (it minted the capability from that knowledge) and told nobody, so a
/// decision comment typed while signed in landed "(unattributed)". Now the transport stamps
/// the write with `principal` — the passkey's STABLE IRI — the form adapter forwards it as
/// the ledger's `author`, and the face renders it as the passkey's CURRENT label.
#[test]
fn a_signed_in_form_write_is_attributed_to_the_passkey_and_renders_its_label() {
    let server = Server::start();
    let (token, who) = sign_in(
        &server,
        "brian",
        &grants_for("default", Authority::Write).unwrap(),
    );
    assert!(who.starts_with("urn:iki:gonk:passkey:"), "{who}");
    let credential = who.rsplit(':').next().unwrap().to_string();

    let filed = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "append"),
            ("_then", "items"),
            ("content", "Attributed item"),
        ],
        Some(&token),
    );
    assert_eq!(filed.status, 200, "{filed:?}");
    let (iri, id) = first_item(&filed.body);
    let commented = comment_as(&server, &iri, &id, "Decided.", Some(&token));
    assert_eq!(commented.status, 200, "{commented:?}");

    // The store holds the IRI — the credential, never the label. A relabeled passkey keeps
    // its history, and two passkeys named alike stay distinct.
    let graph = turtle(&server, &id);
    assert_eq!(
        graph.matches(&format!("\"{who}\"")).count(),
        2,
        "the item and its comment are both attributed to the passkey IRI: {graph}"
    );
    assert!(
        !graph.contains("software"),
        "the label is never written to the store: {graph}"
    );

    // The face translates: the item's `Filed … by` and the comment's meta line both show
    // the label, and the IRI appears nowhere on the page — for an anonymous reader too.
    let page = server.page(&format!("/l/default/item/{id}"), None);
    assert_eq!(page.status, 200, "{page:?}");
    assert!(page.body.contains("by software"), "{page:?}");
    assert!(
        page.body.contains("<p class='comment-meta'>")
            && page.body.matches("software").count() >= 2,
        "{page:?}"
    );
    assert!(
        !page.body.contains("urn:iki:gonk:passkey:") && !page.body.contains("unattributed"),
        "{page:?}"
    );

    // ★ The CURRENT label: relabel the passkey in clients.json and the page follows on the
    // next request, with nothing rewritten in the store. (⚠ Not "Touch ID": the `#enrol`
    // panel's placeholder says that on every page, so a negative check on it is void.)
    let clients = server.layout.clients_json();
    let text = std::fs::read_to_string(&clients).unwrap();
    assert!(text.contains("\"label\": \"software\""), "{text}");
    std::fs::write(
        &clients,
        text.replace("\"label\": \"software\"", "\"label\": \"desk key\""),
    )
    .unwrap();
    let page = server.page(&format!("/l/default/item/{id}"), None);
    assert!(
        page.body.contains("by desk key") && !page.body.contains("software"),
        "{page:?}"
    );
    assert_eq!(
        turtle(&server, &id),
        graph,
        "a relabel touches the store not at all"
    );

    // A passkey since deleted still names its writes: the IRI's tail, never "unattributed".
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&clients).unwrap()).unwrap();
    assert!(doc["passkeys"]
        .as_object_mut()
        .unwrap()
        .remove(&credential)
        .is_some());
    std::fs::write(&clients, doc.to_string()).unwrap();
    let page = server.page(&format!("/l/default/item/{id}"), None);
    assert!(
        page.body.contains(&format!("by {credential}")) && !page.body.contains("desk key"),
        "{page:?}"
    );
}

/// The other side of the same rule: nobody signed in means no author — exactly as before —
/// and a form field named `author` is refused whether or not anyone is signed in, because
/// the door names the author and a submitter may not.
#[test]
fn an_anonymous_form_write_has_no_author_and_a_form_may_not_name_one() {
    let server = Server::start();
    let (iri, id) = file(&server, "Anonymous item");
    let commented = comment_as(&server, &iri, &id, "Looked at it.", None);
    assert_eq!(commented.status, 200, "{commented:?}");
    let graph = turtle(&server, &id);
    assert!(
        !graph.contains("author"),
        "an anonymous loopback caller is nobody, and nobody is written: {graph}"
    );
    let page = server.page(&format!("/l/default/item/{id}"), None);
    assert!(page.body.contains("unattributed"), "{page:?}");

    let (token, _) = sign_in(
        &server,
        "brian",
        &grants_for("default", Authority::Write).unwrap(),
    );
    for cookie in [None, Some(token.as_str())] {
        let forged = server.form(
            &[
                ("_ledger", "default"),
                ("_action", "comment"),
                ("_id", &id),
                ("_then", "card"),
                ("item", &iri),
                ("content", "Forged."),
                ("author", "mallory"),
            ],
            cookie,
        );
        assert_eq!(
            forged.status,
            400,
            "signed in: {}: {forged:?}",
            cookie.is_some()
        );
        assert!(
            forged
                .body
                .contains("the door names the author; a form may not"),
            "{forged:?}"
        );
    }
    let graph = turtle(&server, &id);
    assert!(
        !graph.contains("Forged.") && !graph.contains("mallory"),
        "a refused form writes nothing: {graph}"
    );
}

/// The mechanical route (`POST /iki/ledger/append`, what `curl` and the cli's HTTP mount
/// use) is unchanged: the transport stamps `principal` on it too, and the ledger — which
/// declares `author` and not `principal` — ignores it. Reading `principal` as an `author`
/// fallback would be `ikigai-ledger`'s own decision, not this server's.
#[test]
fn the_mechanical_door_carries_the_principal_and_the_ledger_ignores_it() {
    let server = Server::start();
    let (token, who) = sign_in(
        &server,
        "brian",
        &grants_for("default", Authority::Write).unwrap(),
    );
    let appended = server.raw(
        "POST",
        "/iki/ledger/append",
        &[("Cookie", format!("{}={token}", identity::SESSION_COOKIE))],
        "Filed over http, signed in",
    );
    assert_eq!(appended.status, 200, "{appended:?}");
    let (_, id) = first_item(&appended.body);
    let graph = turtle(&server, &id);
    assert!(
        !graph.contains(&who) && !graph.contains("author"),
        "the ledger declares no `principal`, so the mechanical route is exactly as it was: {graph}"
    );
}

/// ★ **An HTTP `DELETE` on a ledger resource answers `200` with the ledger's text** (ledger
/// #1378). Through `ikigai-web` 0.1.56 the edge answered every successful Delete `204 No
/// Content` and dropped the body; from 0.1.57 (ikigai-cli PR 474) a Delete whose endpoint
/// answers with a representation is a `200` carrying it, and only an EMPTY answer is a `204`.
/// Every ledger Delete answers a sentence, so on the mechanical door (`DELETE /iki/ledger/…`,
/// what `curl` and the cli's HTTP mount use) each is now a `200`.
///
/// Three neighbors of that change are pinned beside it, because 0.1.57 moved them too:
///
/// - the REPEAT of a Delete that succeeded is still `204` (the edge's tombstone), and since
///   0.1.57 the tombstone keys on the query as well as the path;
/// - a Delete of something that never existed is a `404` carrying the LEDGER's own message,
///   where through 0.1.56 the edge answered a bare `not found`;
/// - the browser never sends a `DELETE`: its forms post to `/act` with `_verb=Delete`, a Sink
///   at the edge, so the UI's answers did not move (`a_passkey_identity_adds_its_grant_and_only_its_grant`
///   deletes that way and expects `200`, as it always did).
#[test]
fn an_http_delete_answers_with_the_ledgers_text() {
    let server = Server::start_configured(
        ikigai_gonk::rules::DEFAULT_RULES,
        grants_for("default", Authority::Delete).unwrap(),
    );
    let (_, id) = file(&server, "Delete me over http");
    let deferred = server.raw("POST", &format!("/iki/ledger/defer?item={id}"), &[], "");
    assert_eq!(deferred.status, 200, "{deferred:?}");

    // A Delete that names its target by a query argument: the deferral comes off, and the
    // answer says so. (Not a label: a label's Delete takes the label as `content`, and the
    // edge forwards neither a DELETE's body nor a `content` query argument, so that Delete
    // is out of an HTTP `DELETE`'s reach in every release, 0.1.57 included. It is reached
    // through `/act`, which is how the browser removes a label.)
    let resumed = server.raw("DELETE", &format!("/iki/ledger/defer?item={id}"), &[], "");
    assert_eq!(resumed.status, 200, "{resumed:?}");
    assert!(
        resumed
            .head
            .to_ascii_lowercase()
            .contains("content-type: text/plain"),
        "{resumed:?}"
    );
    assert_eq!(resumed.body, "#1 resumed\n", "{resumed:?}");

    // The item itself: the ledger's receipt comes back, naming where the quads went.
    let deleted = server.raw("DELETE", &format!("/iki/ledger/item/{id}"), &[], "");
    assert_eq!(deleted.status, 200, "{deleted:?}");
    assert!(deleted.body.starts_with("deleted #1 "), "{deleted:?}");
    assert!(deleted.body.contains("recoverable"), "{deleted:?}");
    let gone = server.page(&format!("/l/default/item/{id}"), None);
    assert_eq!(gone.status, 404, "{gone:?}");

    // The same request again: the item is gone, and the edge's tombstone makes the retry
    // idempotent — an empty `204`, not the `404` a fresh request for it would get.
    let again = server.raw("DELETE", &format!("/iki/ledger/item/{id}"), &[], "");
    assert_eq!(again.status, 204, "{again:?}");
    assert_eq!(again.body, "", "{again:?}");

    // Never existed: a `404` that carries the ledger's own sentence, not the edge's.
    let never = server.raw("DELETE", "/iki/ledger/item/0000000000zzzzzz", &[], "");
    assert_eq!(never.status, 404, "{never:?}");
    assert_ne!(never.body.trim(), "not found", "{never:?}");
    assert!(never.body.contains("0000000000zzzzzz"), "{never:?}");
}

/// How long a client that sent `Expect: 100-continue` waits for the door's first word before
/// it sends its body anyway: curl's expect timeout (a JVM HTTP client waits 3 s). A door that
/// says nothing costs every such write at least this long.
const EXPECT_PROMPT: Duration = Duration::from_secs(1);

impl Server {
    /// Send `head` the way a client honoring `Expect: 100-continue` does: the head alone, then
    /// wait for the door's first answer before the body. Returns that first answer's head, how
    /// long it took to arrive, and the final response. After an interim `100` the body is sent
    /// and the final response is what follows; a door may instead answer its final status at
    /// once (RFC 9110 §10.1.1), and then the body is never sent.
    fn expecting(&self, head: &str, body: &str) -> (String, Duration, Response) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        // Bounded so a silent door fails the test instead of hanging it: through `ikigai-web`
        // 0.1.59 the door waited for the body until its 30 s body deadline and answered `408`.
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let started = Instant::now();
        stream.write_all(head.as_bytes()).unwrap();
        let mut got = Vec::new();
        let mut chunk = [0u8; 1024];
        let end = loop {
            if let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match stream.read(&mut chunk) {
                Ok(0) => break got.len(),
                Ok(n) => got.extend_from_slice(&chunk[..n]),
                Err(e) => panic!(
                    "the door said nothing to a head asking `Expect: 100-continue` in {:?}: {e}",
                    started.elapsed()
                ),
            }
        };
        let waited = started.elapsed();
        let first = String::from_utf8_lossy(&got[..end]).into_owned();
        let mut rest = got[end..].to_vec();
        if first.starts_with("HTTP/1.1 100 ") {
            stream.write_all(body.as_bytes()).unwrap();
        } else {
            rest = got;
        }
        stream.read_to_end(&mut rest).unwrap();
        let response = String::from_utf8_lossy(&rest).into_owned();
        (first, waited, Response::parse(&response))
    }
}

/// ★ **A write that asks `Expect: 100-continue` is invited at once** (ledger #1408). A client
/// that sends the expectation holds its body back until the door's interim `100 Continue`.
/// Through `ikigai-web` 0.1.59 the door read the body without ever sending it, so every such
/// POST waited out the client's own expect timeout (curl's 1 s, a JVM HTTP client's 3 s), and a
/// client that never gives up waited the 30 s body deadline for a `408`. From 0.1.60
/// (ikigai-cli PR 482) `read_request` sends the `100` once the head has passed the checks made
/// on the head alone. This is gonk's own door — its routes, its `CapFn` and its admission —
/// with a mechanical append, the write `curl --data-binary` makes:
///
/// - the interim `100` arrives well inside [`EXPECT_PROMPT`], the body follows, and the item is
///   filed;
/// - a write the door refuses (a cross-site one) is answered promptly too, its `403` either
///   instead of the invitation or right after the body, never after a stall.
///
/// FLOOR TEST for `ikigai-web` 0.1.60: on 0.1.58 the first read times out with no answer at all.
#[test]
fn a_post_expecting_100_continue_is_invited_at_once() {
    let server = Server::start();
    let body = "Filed with an expectation";
    let head = format!(
        "POST /iki/ledger/append HTTP/1.1\r\nHost: localhost:{}\r\nContent-Type: text/plain\r\n\
         Expect: 100-continue\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        server.addr.port(),
        body.len()
    );
    let (first, waited, filed) = server.expecting(&head, body);
    assert_eq!(first, "HTTP/1.1 100 Continue\r\n\r\n", "{filed:?}");
    assert!(
        waited < EXPECT_PROMPT,
        "the invitation took {waited:?}, so a client waits out its own expect timeout"
    );
    assert_eq!(filed.status, 200, "{filed:?}");
    let (_, id) = first_item(&filed.body);
    let item = server.page(&format!("/l/default/item/{id}"), None);
    assert_eq!(item.status, 200, "{item:?}");
    assert!(
        item.body.contains(body),
        "the body reached the ledger: {item:?}"
    );

    let forged = "forged with an expectation";
    let head = format!(
        "POST /iki/ledger/append HTTP/1.1\r\nHost: localhost:{}\r\nContent-Type: text/plain\r\n\
         Sec-Fetch-Site: cross-site\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        server.addr.port(),
        forged.len()
    );
    let (first, waited, refused) = server.expecting(&head, forged);
    assert!(
        waited < EXPECT_PROMPT,
        "a refused write's first answer took {waited:?}: {first}"
    );
    assert_eq!(refused.status, 403, "{refused:?}");
}

/// A search term comes back into the page as the search box's `value` attribute, so a hostile term must
/// arrive ESCAPED, never as markup. The machine reviewer raised this as an XSS claim (finding 603f11e2, and
/// its kind on notes and error text) during findings sweep 2 (ledger #706); a probe showed the stylesheet's
/// serializer escapes it. This pins that, so a template change that stops escaping fails here.
#[test]
fn a_hostile_search_term_comes_back_escaped_not_as_markup() {
    let server = Server::start();
    file(&server, "Item one");
    let hostile = "x'\"><script>alert(1)</script>";
    let page = server.page(&format!("/l/default?text={}", encode(hostile)), None);
    assert!(
        !page.body.contains("<script>alert(1)</script>"),
        "the term must not reach the page as markup: {page:?}"
    );
    assert!(
        page.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "the term is echoed into the search box, escaped: {page:?}"
    );
}

/// Load `turtle` into `graph` as root, through the hub — a graph no door's caller may write.
fn load(server: &Server, graph: &str, turtle: &str) {
    let request = [
        ("content", turtle),
        ("format", "text/turtle"),
        ("graph", graph),
    ]
    .iter()
    .fold(
        ikigai_core::Request::new(
            ikigai_core::Verb::Sink,
            ikigai_core::Iri::parse("urn:iki:store:load").unwrap(),
        ),
        |request, (name, value)| {
            request.with_arg(
                *name,
                ikigai_core::ArgRef::Inline(value.as_bytes().to_vec()),
            )
        },
    );
    futures::executor::block_on(ikigai_core::Kernel::issue(
        &server.hub,
        request,
        &ikigai_core::Capability::root(),
    ))
    .unwrap_or_else(|e| panic!("loading {graph}: {e}"));
}

const TITLES: &str =
    "PREFIX dcterms: <http://purl.org/dc/terms/> SELECT ?t WHERE { ?s dcterms:title ?t }";

/// ★ **`/sparql` is the SPARQL 1.1 Protocol over `urn:sparql:*`** (ledger #836): asked for a
/// results type and naming no ledger, it reads the UNION of the graphs the caller may read —
/// the anonymous caller its ledger's, a signed-in passkey that plus what its grant names —
/// by GET and by both POST forms, and it refuses what it cannot honor rather than answering
/// a different question.
#[test]
fn the_sparql_protocol_reads_the_union_the_callers_grant_allows() {
    let server = Server::start();
    file(&server, "A ledger title");
    load(
        &server,
        "urn:iki:browse:graph:default",
        "<urn:iki:annotation:p1> <http://purl.org/dc/terms/title> \"A browse title\" .",
    );
    let json = || vec![("Accept", "application/sparql-results+json".to_string())];

    // Anonymous: the ledger's graph is all its grant reads.
    let anonymous = server.raw(
        "GET",
        &format!("/sparql?query={}", encode(TITLES)),
        &json(),
        "",
    );
    assert_eq!(anonymous.status, 200, "{anonymous:?}");
    assert!(anonymous.body.contains("A ledger title"), "{anonymous:?}");
    assert!(!anonymous.body.contains("A browse title"), "{anonymous:?}");

    // Signed in under a grant that adds the browse graph: the union is both.
    let (token, _) = sign_in(
        &server,
        "reader",
        &ikigai_gonk::grants::browse_graph_grants(Authority::Read).unwrap(),
    );
    let mut headers = json();
    headers.push(("Cookie", format!("{}={token}", identity::SESSION_COOKIE)));
    let signed = server.raw(
        "GET",
        &format!("/sparql?query={}", encode(TITLES)),
        &headers,
        "",
    );
    assert_eq!(signed.status, 200, "{signed:?}");
    assert!(signed.body.contains("A ledger title"), "{signed:?}");
    assert!(signed.body.contains("A browse title"), "{signed:?}");
    // …and `graph` narrows it, to a graph the grant names.
    let narrowed = server.raw(
        "GET",
        &format!(
            "/sparql?graph=urn:iki:browse:graph:default&query={}",
            encode(TITLES)
        ),
        &headers,
        "",
    );
    assert!(narrowed.body.contains("A browse title"), "{narrowed:?}");
    assert!(!narrowed.body.contains("A ledger title"), "{narrowed:?}");
    // Anonymously, naming the browse graph is a 403, not an empty answer.
    let refused = server.raw(
        "GET",
        &format!(
            "/sparql?graph=urn:iki:browse:graph:default&query={}",
            encode(TITLES)
        ),
        &json(),
        "",
    );
    assert_eq!(refused.status, 403, "{refused:?}");

    // POST, both protocol forms: the same answer as the GET.
    let mut sparql_query = json();
    sparql_query.push(("Content-Type", "application/sparql-query".to_string()));
    let posted = server.raw("POST", "/sparql", &sparql_query, TITLES);
    assert_eq!(posted.status, 200, "{posted:?}");
    assert_eq!(posted.body, anonymous.body);
    let mut form = json();
    form.push((
        "Content-Type",
        "application/x-www-form-urlencoded".to_string(),
    ));
    let formed = server.raw(
        "POST",
        "/sparql",
        &form,
        &format!("query={}", encode(TITLES)),
    );
    assert_eq!(formed.status, 200, "{formed:?}");
    assert_eq!(formed.body, anonymous.body);

    // What it cannot honor, it refuses.
    let mut plain = json();
    plain.push(("Content-Type", "text/plain".to_string()));
    assert_eq!(server.raw("POST", "/sparql", &plain, TITLES).status, 400);
    let dataset = server.raw(
        "GET",
        &format!(
            "/sparql?default-graph-uri=urn:iki:ledger:graph:default&query={}",
            encode(TITLES)
        ),
        &json(),
        "",
    );
    assert_eq!(dataset.status, 400, "{dataset:?}");
    assert!(dataset.body.contains("default-graph-uri"), "{dataset:?}");
    // A POST is a read: an update in the body is not a query, and nothing is written.
    let update = server.raw(
        "POST",
        "/sparql",
        &sparql_query,
        "INSERT DATA { <urn:x> <http://purl.org/dc/terms/title> \"injected\" }",
    );
    assert_eq!(update.status, 400, "{update:?}");
    let after = server.raw(
        "GET",
        &format!("/sparql?query={}", encode(TITLES)),
        &json(),
        "",
    );
    assert_eq!(after.body, anonymous.body);

    // The editor page still reads one ledger, and says so.
    let page = server.page(&format!("/sparql/results?query={}", encode(TITLES)), None);
    assert!(
        page.body
            .contains("1 row(s) from urn:iki:ledger:graph:default"),
        "{page:?}"
    );
}

/// Ledger [#989](http://localhost:1060/l/default/item/989): an item's and a comment's body
/// render their http(s) URLs and `ledger #N` references as links — and nothing else. The
/// trailing `)` and `.` of the sentence stay outside the link, a `javascript:` URL stays text,
/// and a `<` in the body stays escaped text (`render::linkify`).
#[test]
fn urls_and_ledger_references_in_bodies_are_links_and_nothing_else_is() {
    let server = Server::start();
    let body = "A link (see https://example.com/a(b)c). Then http://x.example/y?p=1&q=2. \
                Not javascript:alert(1), not <script>alert(2)</script>, and ledger #12, \
                but not ledger #12a.";
    let (iri, id) = file(&server, &format!("Links in the body\n\n{body}"));
    let commented = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "comment"),
            ("_id", &id),
            ("_then", "card"),
            ("item", &iri),
            ("content", "Comment: https://example.org/c."),
        ],
        None,
    );
    assert_eq!(commented.status, 200, "{commented:?}");
    let page = server.page(&format!("/l/default/item/{id}"), None);
    assert_eq!(page.status, 200, "{page:?}");
    let html = &page.body;
    assert!(
        html.contains("<a href='https://example.com/a(b)c'>https://example.com/a(b)c</a>). Then"),
        "a URL followed by `).`: {html}"
    );
    assert!(
        html.contains(
            "<a href='http://x.example/y?p=1&amp;q=2'>http://x.example/y?p=1&amp;q=2</a>. Not"
        ),
        "a URL followed by `.`, its `&` escaped once: {html}"
    );
    assert!(
        html.contains("Not javascript:alert(1),") && !html.contains("href='javascript"),
        "a javascript: URL stays text: {html}"
    );
    assert!(
        html.contains("&lt;script&gt;alert(2)&lt;/script&gt;") && !html.contains("<script>alert"),
        "a `<` stays escaped text: {html}"
    );
    assert!(
        html.contains("and <a href='/l/default/item/12'>ledger #12</a>, but not ledger #12a."),
        "`ledger #12` links to the item: {html}"
    );
    assert!(
        html.contains("Comment: <a href='https://example.org/c'>https://example.org/c</a>."),
        "a comment's body is linked too: {html}"
    );
}

/// Ledger [#1376](http://localhost:1062/l/default/item/1376): the linker compared a body's
/// first eight BYTES at each word boundary, so `http:/` followed by a wide character, or any
/// word whose eighth byte fell inside one, panicked the render and the item's page failed for
/// every reader. Anyone who may write a body could do that to a page. Both bodies render now,
/// as written, and a URL beside a wide character still links.
#[test]
fn a_body_with_a_wide_character_after_a_partial_scheme_still_renders() {
    let server = Server::start();
    let body = "Broken scheme http:/€uro and prose like abcdefg€ or wxyz𝄞 here.";
    let (iri, id) = file(&server, &format!("Wide characters\n\n{body}"));
    let commented = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "comment"),
            ("_id", &id),
            ("_then", "card"),
            ("item", &iri),
            (
                "content",
                "Comment: https:/€ then https://a.example/é€ done.",
            ),
        ],
        None,
    );
    assert_eq!(commented.status, 200, "{commented:?}");
    let page = server.page(&format!("/l/default/item/{id}"), None);
    assert_eq!(page.status, 200, "{page:?}");
    let html = &page.body;
    assert!(
        html.contains(body),
        "the item body renders as written: {html}"
    );
    assert!(
        html.contains(
            "Comment: https:/€ then \
             <a href='https://a.example/é€'>https://a.example/é€</a> done."
        ),
        "the comment renders, and its whole URL links: {html}"
    );
}

/// gonk's default port moved from 1060 to 1062 on 2026-10-12. A stored link
/// to gonk at the OLD origin renders onto the server serving the page (a root-relative href,
/// which the browser resolves against this door's origin), in an item's body and a comment's;
/// a 1060 link that is not one of gonk's routes is left alone; and the STORE still holds
/// every body exactly as written (`render::rehome`).
#[test]
fn a_stored_link_to_gonk_at_port_1060_renders_onto_this_server() {
    let server = Server::start();
    let body = "Old links: [#739](http://localhost:1060/l/default/item/739) and \
                http://127.0.0.1:1060/browse/urn:repo:x:tree, but not \
                http://localhost:1060/other/service/page.";
    let (iri, id) = file(&server, &format!("Old port links\n\n{body}"));
    let commented = server.form(
        &[
            ("_ledger", "default"),
            ("_action", "comment"),
            ("_id", &id),
            ("_then", "card"),
            ("item", &iri),
            (
                "content",
                "See http://localhost:1060/queue?state=published&group=file.",
            ),
        ],
        None,
    );
    assert_eq!(commented.status, 200, "{commented:?}");
    let page = server.page(&format!("/l/default/item/{id}"), None);
    assert_eq!(page.status, 200, "{page:?}");
    let html = &page.body;
    for (href, text) in [
        (
            "/l/default/item/739",
            "http://localhost:1060/l/default/item/739",
        ),
        (
            "/browse/urn:repo:x:tree",
            "http://127.0.0.1:1060/browse/urn:repo:x:tree",
        ),
        (
            "/queue?state=published&amp;group=file",
            "http://localhost:1060/queue?state=published&amp;group=file",
        ),
    ] {
        assert!(
            html.contains(&format!("<a href='{href}'>{text}</a>")),
            "{href}: {html}"
        );
    }
    assert!(
        html.contains(
            "<a href='http://localhost:1060/other/service/page'>\
             http://localhost:1060/other/service/page</a>."
        ),
        "a 1060 link that is not gonk's stays as written: {html}"
    );
    let stored = server.raw(
        "GET",
        &format!("/iki/ledger/item/{id}"),
        &[("Accept", "text/turtle".to_string())],
        "",
    );
    assert_eq!(stored.status, 200, "{stored:?}");
    assert!(
        stored
            .body
            .contains("[#739](http://localhost:1060/l/default/item/739)")
            && stored
                .body
                .contains("http://localhost:1060/queue?state=published&group=file"),
        "the store holds the bodies as written: {stored:?}"
    );
}

/// Ledger [#505](http://localhost:1062/l/default/item/505): the item page's "Close as" menu is
/// `urn:iki:ledger:close`'s own `reason` set, in the contract's order and nothing else — read
/// from the hub's contract, never a copy in the stylesheet.
#[test]
fn the_close_menu_is_the_ledger_contracts_reason_set_in_order() {
    let server = Server::start();
    let (_iri, id) = file(&server, "Close me some way");
    let item = server.page(&format!("/l/default/item/{id}"), None);
    assert_eq!(item.status, 200, "{item:?}");
    let at = item
        .body
        .find("id='close-reason'")
        .unwrap_or_else(|| panic!("a close menu: {item:?}"));
    let start = item.body[..at]
        .rfind("<select")
        .expect("the menu is a select");
    let end = start
        + item.body[start..]
            .find("</select>")
            .expect("the menu closes");
    let menu = &item.body[start..end];
    let offered: Vec<&str> = menu
        .split("<option")
        .skip(1)
        .map(|option| {
            let at = option.find("value='").expect("an option carries a value") + 7;
            &option[at..at + option[at..].find('\'').expect("the value closes")]
        })
        .collect();
    let declared: Vec<&str> = ikigai_ledger::vocabulary::CLOSE_REASONS
        .iter()
        .map(|(word, _)| *word)
        .collect();
    assert_eq!(offered, declared, "{menu}");
    for word in &declared {
        assert!(menu.contains(&format!(">{word}</option>")), "{menu}");
    }
}

/// Ledger [#666](http://localhost:1062/l/default/item/666): `#flash` is cleared before a
/// request a person made, and NOT before the header badge's timed poll — which runs on every
/// page every few seconds and used to empty `#flash` before anyone had read it.
///
/// There is no JavaScript runtime in this suite (see
/// `the_active_sample_is_cleared_by_typing_in_the_editor`), so what is pinned is the wiring:
/// the clearing listener returns early for a request from inside `.queue-badge`, before it
/// clears. The behavior was checked under node with a stubbed DOM before merging.
#[test]
fn the_badge_poll_does_not_clear_the_flash() {
    let js = include_str!("../web/gonk.js");
    let at = js
        .find("addEventListener(\"htmx:beforeRequest\"")
        .expect("the clearing listener");
    let body = &js[at..at + js[at..].find("\n  });").expect("the listener ends")];
    let skip = body
        .find(".closest(\".queue-badge\")) return;")
        .unwrap_or_else(|| panic!("the badge's request is skipped: {body}"));
    let clear = body
        .find("flash(\"\", \"ok\")")
        .unwrap_or_else(|| panic!("other requests still clear: {body}"));
    assert!(skip < clear, "the skip comes before the clear: {body}");
    assert_eq!(
        js.matches("htmx:beforeRequest").count(),
        1,
        "one clearing listener, so no second one clears on the poll"
    );
    // The badge's markup still carries the class the listener keys on.
    assert!(
        include_str!("../web/gonk.xsl").contains("<span class=\"queue-badge\""),
        "the badge is a .queue-badge span"
    );
}

/// Ledger [#446](http://localhost:1062/l/default/item/446): when the header leaves out a section
/// this server has because the caller's grant does not reach it, the header says so — one
/// generic sentence, never which section — and says nothing when nothing was left out.
#[test]
fn the_header_says_when_a_section_is_withheld_and_only_then() {
    let hidden = "Some sections are hidden";
    let rules = ikigai_gonk::rules::DEFAULT_RULES;

    // Everything this server has is in the header: nothing to say.
    let whole = Server::start();
    let page = whole.page("/", None);
    assert_eq!(page.status, 200, "{page:?}");
    assert!(!page.body.contains(hidden), "{page:?}");

    // A configured ledger the caller may not read.
    let no_ledger = Server::start_configured(rules, Vec::new());
    let page = no_ledger.page("/", None);
    assert!(
        page.body.contains(web::WITHHELD_SIGNED_OUT) && page.body.contains(web::WITHHELD_SIGNED_IN),
        "both halves are drawn, the script picks one: {page:?}"
    );
    assert!(
        page.body.contains("data-withheld-notice='true'"),
        "drawn as a notice web/gonk.js switches: {page:?}"
    );
    assert!(
        include_str!("../web/gonk.js").contains("[data-signin-notice], [data-withheld-notice]"),
        "the script switches it with the sign-in notices"
    );

    // Browse roots configured, none readable: Browse and the Queue are left out.
    let roots = Server::start_with_roots(
        rules,
        grants_for("default", Authority::Write).unwrap(),
        vec!["demo".to_string()],
    );
    let page = roots.page("/", None);
    assert!(!page.body.contains(">Browse</a>"), "{page:?}");
    assert!(page.body.contains(web::WITHHELD_SIGNED_OUT), "{page:?}");
    assert!(
        !page.body.contains("demo"),
        "the sentence never names what is hidden: {page:?}"
    );
}

/// ★ **Ledger #1604: the doctor says what it judged against, on ONE line a reader can parse.**
///
/// The page used to put its lifecycle, its checked time and its checks in a `<p class="meta">`,
/// and `.meta` is the item card's `<dl>` grid (`max-content 1fr`): every text run and `<code>`
/// in the paragraph became a grid cell, so "lifecycle" sat alone at the left, `kata-flight`
/// sat in monospace at the right, and "· checked … · orphaned, …" fell to a row of its own.
/// Nothing in it said what "lifecycle" or "kata-flight" meant, or what "checked" was.
///
/// Pinned here: one paragraph, in reading order, that names the lifecycle as a LINK to its
/// states (the ledger's own `urn:iki:ledger:lifecycle:{name}`, through the mechanical
/// `/iki/…` door, which a browser can follow), marks the checked time as a `<time>` and says
/// it is when the cached check last ran (decided in ledger #1567), and labels the checks.
/// The paragraph is not in a grid or flex container, so at any width it flows as a sentence.
/// A clean bill reads as a sentence about those checks, not a bare "No problems."
#[test]
fn the_doctor_says_what_it_judged_against_on_one_line() {
    let server = Server::start();
    let page = server.page("/l/default/doctor", None);
    assert_eq!(page.status, 200, "{page:?}");

    let at = page
        .body
        .find("doctor-scope")
        .unwrap_or_else(|| panic!("no doctor-scope paragraph: {page:?}"));
    let open = page.body[..at].rfind('<').expect("the paragraph's tag");
    assert!(
        page.body[open..].starts_with("<p "),
        "the scope is ONE paragraph: {page:?}"
    );
    let line = &page.body[open..open + page.body[open..].find("</p>").expect("closed") + 4];
    let text = {
        let mut out = String::new();
        let mut in_tag = false;
        for c in line.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => out.push(c),
                _ => {}
            }
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let checked = text
        .split(" · checked ")
        .nth(1)
        .and_then(|rest| rest.split(" (when").next())
        .unwrap_or_else(|| panic!("no checked time in: {text}"));
    assert!(
        checked.ends_with(" UTC") && checked.len() == "2026-10-15 15:56 UTC".len(),
        "the checked time reads as a time: {text}"
    );
    assert_eq!(
        text,
        format!(
            "Judged against the kata-flight lifecycle · checked {checked} (when this cached \
             check last ran) · checks: orphaned, abandoned, lease-expired, unknown-state, \
             two-states, awaiting-closed"
        ),
        "{line}"
    );
    assert!(
        line.contains("<a href='/iki/ledger/lifecycle/kata-flight'>kata-flight lifecycle</a>"),
        "the lifecycle is a link to its states: {line}"
    );
    assert!(
        line.contains("<time datetime='"),
        "the checked time is a <time> with its instant: {line}"
    );
    assert!(
        !line.contains("<code>"),
        "no monospace run to wrap on its own: {line}"
    );

    // The paragraph's class lays out as text: no grid, no flex.
    let css = include_str!("../web/gonk.css");
    for rule in css.split('}') {
        let Some((selectors, body)) = rule.split_once('{') else {
            continue;
        };
        if selectors
            .split(',')
            .any(|s| s.split_whitespace().last() == Some(".doctor-scope"))
        {
            assert!(
                !body.contains("display"),
                "the scope line is a paragraph of text, never a layout: {rule}"
            );
        }
    }

    assert!(
        page.body
            .contains("<p class='empty'>None of these checks found a problem.</p>"),
        "{page:?}"
    );
    assert!(!page.body.contains("No problems."), "{page:?}");

    // ★ The link goes somewhere a browser can read: the lifecycle's states, in order.
    let states = server.page("/iki/ledger/lifecycle/kata-flight", None);
    assert_eq!(states.status, 200, "{states:?}");
    for state in [
        "filed",
        "queued",
        "reviewed",
        "resolving",
        "refining",
        "shipping",
    ] {
        assert!(states.body.contains(state), "{state}: {states:?}");
    }
}
