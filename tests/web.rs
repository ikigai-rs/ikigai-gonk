//! The HTML face, driven over real HTTP the way a browser drives it.
//!
//! ⚠ **Every browser-shaped request here sends Chrome's `Accept` header**, never curl's
//! `*/*`. `ikigai-web` turns the FIRST type in `Accept` into `as=`, so a page that served
//! only plain text would answer a browser `400` while every `*/*` test passed.
//!
//! - [`a_browser_gets_html_pages_and_a_readable_404`]
//! - [`a_person_files_edits_comments_and_closes_through_forms`]
//! - [`a_form_can_send_only_what_the_ledger_declares`]
//! - [`a_cross_site_write_and_a_rebound_host_get_nothing`]
//! - [`sparql_is_confined_to_one_ledger_graph`]
//! - [`every_sample_query_returns_rows`]
//! - [`the_sparql_page_offers_the_samples_without_running_them`]
//! - [`the_cross_graph_example_is_shown_and_never_loadable`]
//! - [`a_ledger_iri_reads_as_its_local_name_only_in_the_html_face`]
//! - [`a_passkey_identity_adds_its_grant_and_only_its_grant`]
//! - [`the_palette_clears_the_contrast_floor`]
//! - [`a_result_column_is_never_narrower_than_its_own_word`]
//! - [`a_sample_button_is_a_toggle_the_server_renders_unpressed`]
//! - [`the_active_sample_is_cleared_by_typing_in_the_editor`]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

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
    _config: tempfile::TempDir,
}

impl Server {
    fn start() -> Server {
        Server::start_with_rules(ikigai_gonk::rules::DEFAULT_RULES)
    }

    /// A server whose render rules are `rules` rather than the shipped table — what a
    /// deployment gets by dropping its own `gonk/render-rules.ttl` beside `grants.json`.
    fn start_with_rules(rules: &str) -> Server {
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
            browse_roots: Vec::new(),
            passkeys: Arc::clone(&passkeys),
            rules: rules.into(),
        });
        let http = Arc::new(doors::http_kernel(hub, web::space(face)));
        let cap = doors::http_cap(doors::HttpDoor {
            anonymous: grants_for("default", Authority::Write).unwrap(),
            port: addr.port(),
            passkeys: Some(passkeys),
        });
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                cap,
                listener,
                doors::edge_config(),
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
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
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

    // ★ The hub's finding: an unrouted path was a 500 echoing the resolver. Now a 404.
    for path in ["/favicon.ico", "/no/such/page"] {
        let missing = server.page(path, None);
        assert_eq!(missing.status, 404, "{path}: {missing:?}");
        assert!(missing.body.contains("nothing is served at"), "{missing:?}");
    }
    let missing_item = server.page("/l/default/item/0000000000zzzzzz", None);
    assert_eq!(missing_item.status, 404, "{missing_item:?}");
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
        rebound.status, 403,
        "a rebound Host reads nothing: {rebound:?}"
    );
    let home = server.raw(
        "GET",
        "/",
        &[
            ("Host", "evil.example".to_string()),
            ("Accept", CHROME_ACCEPT.to_string()),
        ],
        "",
    );
    assert!(home.body.contains("No ledger to show"), "{home:?}");

    // A local process — no Origin, no Sec-Fetch-Site — is what the door was always for.
    let curl = server.raw("POST", "/iki/ledger/append", &[], "Filed by curl");
    assert_eq!(curl.status, 200, "{curl:?}");
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
    // "the oldest p1" and a button labelled that way would be a lie.
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
            .contains("aria-label='Open items labelled repo:ikigai-gonk'"),
        "{fragment:?}"
    );

    // Follow it exactly as a browser would: the href, verbatim.
    let href = {
        let at = fragment
            .body
            .find("aria-label='Open items labelled repo:ikigai-gonk'")
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
    // that file still parses as the QUIC door's enrolment.
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
