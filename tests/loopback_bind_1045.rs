//! Ledger [#1045](http://localhost:1060/l/default/item/1045): a gonk bound on another loopback
//! IP answers its own address.
//!
//! [`ikigai_gonk::config::refuse_non_loopback`] admits any loopback bind, `127.0.0.2:1070`
//! among them, and through `716b34b` the door then refused that IP literal as a foreign `Host`
//! (`421`), while `localhost` resolves to `127.0.0.1`, where nothing listens: a server that
//! started and could not be reached over HTTP at all. Reproduced on `716b34b` as
//! [`a_door_bound_to_another_loopback_ip_answers_that_ip`]'s first assertion, which returned
//! `Some(REFUSED_FOREIGN_HOST)` there.
//!
//! ⚠ **macOS will not bind `127.0.0.2` without an `lo0` alias** (`Can't assign requested
//! address`), so [`a_scratch_gonk_bound_on_127_0_0_2_is_reachable_there`] runs end to end on
//! Linux (CI) and says it skipped on a Mac without the alias. The decision itself is pinned on
//! every platform by the tests that call [`doors::http_refusal`] directly.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};

use ikigai_gonk::{admit, doors};
use ikigai_web::HttpRequest;

mod scratch_gonk;

fn door(bind: &str) -> doors::HttpDoor {
    doors::HttpDoor {
        anonymous: Vec::new(),
        bind: bind.parse().unwrap(),
        passkeys: None,
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    }
}

fn request(method: &str, headers: &[(&str, &str)]) -> HttpRequest {
    HttpRequest {
        method: method.to_string(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        path: "/".to_string(),
        query: Vec::new(),
        body: Vec::new(),
        peer: None,
    }
}

fn get(host: &str) -> HttpRequest {
    request("GET", &[("host", host)])
}

#[test]
fn a_door_bound_to_another_loopback_ip_answers_that_ip() {
    let door = door("127.0.0.2:1070");
    // The bug: its own address, with its port and bare.
    assert_eq!(doors::http_refusal(&door, &get("127.0.0.2:1070")), None);
    assert_eq!(doors::http_refusal(&door, &get("127.0.0.2")), None);
    // The loopback names keep answering, exactly as on a 127.0.0.1 door.
    for host in [
        "localhost:1070",
        "127.0.0.1:1070",
        "[::1]:1070",
        "LocalHost",
    ] {
        assert_eq!(doors::http_refusal(&door, &get(host)), None, "Host {host}");
    }
    // Every other spelling stays foreign: another loopback IP, another port, a name that
    // merely starts with the address.
    for host in [
        "127.0.0.3:1070",
        "127.0.0.2:1071",
        "localhost:1071",
        "127.0.0.2.evil.example:1070",
        "evil.example:1070",
        "evil.example",
    ] {
        assert_eq!(
            doors::http_refusal(&door, &get(host)),
            Some(admit::REFUSED_FOREIGN_HOST),
            "Host {host}"
        );
    }
}

#[test]
fn a_door_on_127_0_0_1_does_not_answer_the_rest_of_the_loopback_block() {
    // Only the BOUND address joins the names; a default door is not widened to 127/8.
    let door = door("127.0.0.1:1060");
    for host in ["127.0.0.2:1060", "127.0.0.2"] {
        assert_eq!(
            doors::http_refusal(&door, &get(host)),
            Some(admit::REFUSED_FOREIGN_HOST),
            "Host {host}"
        );
    }
    assert_eq!(doors::http_refusal(&door, &get("127.0.0.1:1060")), None);
}

#[test]
fn its_own_page_may_write_and_another_origin_may_not() {
    let own = door("127.0.0.2:1070");
    // A form on the page this door served posts with that page's origin.
    let from = |origin: &str| {
        request(
            "POST",
            &[
                ("host", "127.0.0.2:1070"),
                ("origin", origin),
                ("sec-fetch-site", "same-origin"),
            ],
        )
    };
    assert_eq!(
        doors::http_refusal(&own, &from("http://127.0.0.2:1070")),
        None
    );
    for origin in [
        "http://127.0.0.3:1070",
        "http://127.0.0.2:1071",
        "http://evil.example",
    ] {
        assert_eq!(
            doors::http_refusal(&own, &from(origin)),
            Some(admit::REFUSED_CROSS_SITE),
            "Origin {origin}"
        );
    }
    // And a 127.0.0.1 door does not take a 127.0.0.2 page's write as its own.
    let default = door("127.0.0.1:1070");
    let write = request(
        "POST",
        &[
            ("host", "localhost:1070"),
            ("origin", "http://127.0.0.2:1070"),
        ],
    );
    assert_eq!(
        doors::http_refusal(&default, &write),
        Some(admit::REFUSED_CROSS_SITE)
    );
}

#[test]
fn an_ipv6_spelled_bind_answers_its_bracketed_form() {
    // `refuse_non_loopback` admits the IPv4-mapped loopback too; its Host form is bracketed.
    let door = door("[::ffff:127.0.0.2]:1070");
    assert_eq!(
        doors::http_refusal(&door, &get("[::ffff:127.0.0.2]:1070")),
        None
    );
    assert_eq!(
        doors::http_refusal(&door, &get("::ffff:127.0.0.2:1070")),
        Some(admit::REFUSED_FOREIGN_HOST)
    );
}

/// One raw HTTP/1.1 exchange with exactly the `Host` given.
fn raw(at: SocketAddr, method: &str, target: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let mut stream = TcpStream::connect(at).expect("connect");
    let mut head = format!("{method} {target} HTTP/1.1\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let body = "Filed on another loopback IP";
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    stream.write_all(head.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line: {response}"));
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

#[test]
fn a_scratch_gonk_bound_on_127_0_0_2_is_reachable_there() {
    let ip: IpAddr = "127.0.0.2".parse().unwrap();
    if let Err(e) = std::net::TcpListener::bind((ip, 0)) {
        eprintln!(
            "SKIPPED: this machine will not bind {ip} ({e}); on macOS that needs \
             `ifconfig lo0 alias {ip}`. The decision is pinned by the http_refusal tests above, \
             and CI (Linux) runs this one."
        );
        return;
    }
    let gonk = scratch_gonk::Gonk::start_bound("gonk.bind = \"127.0.0.2:0\"\n", ip);
    assert_eq!(gonk.http.ip(), ip);
    let port = gonk.http.port();
    let host = format!("127.0.0.2:{port}");

    for target in ["/", "/l/default", "/iki/ledger/items", "/static/gonk.css"] {
        let (status, body) = raw(gonk.http, "GET", target, &[("Host", &host)]);
        assert_eq!(status, 200, "GET {target} under Host {host}: {body}");
    }
    // A write from its own page, labeled the way a browser labels it.
    let origin = format!("http://{host}");
    let (status, body) = raw(
        gonk.http,
        "POST",
        "/iki/ledger/append",
        &[
            ("Host", &host),
            ("Origin", &origin),
            ("Sec-Fetch-Site", "same-origin"),
        ],
    );
    assert_eq!(status, 200, "a same-origin write: {body}");
    let (status, body) = raw(gonk.http, "GET", "/iki/ledger/items", &[("Host", &host)]);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("Filed on another loopback IP"), "{body}");

    // A foreign spelling reaching the same socket is still misdirected.
    for foreign in [format!("127.0.0.3:{port}"), format!("evil.example:{port}")] {
        let (status, body) = raw(gonk.http, "GET", "/l/default", &[("Host", &foreign)]);
        assert_eq!(status, 421, "Host {foreign}: {body}");
    }
}
