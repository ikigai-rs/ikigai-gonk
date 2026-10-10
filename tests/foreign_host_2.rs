//! Ledger [#2](http://localhost:1060/l/default/item/2): a request under a foreign `Host` is
//! REFUSED before resolution — `421 Misdirected Request` — and every spelling of this server's
//! own loopback name, with its port, is still answered.
//!
//! The item (filed 2026-09-15) said a foreign `Host` was answered with an EMPTY capability, so
//! every action that requires nothing was still described and served to a DNS-rebinding page
//! (`/iki/ledger/purge?description` answered `200`). Reproduced on `4a9bc19` against a scratch
//! gonk, that half no longer holds: ledger #864 (R2) turned the empty capability into a
//! refusal and ledger #879 moved it to the edge, ahead of `OPTIONS` and `?description`, so
//! every route below already answered `403` with no contract in the body. What remained was
//! the status: a `403` says this server is the authority for that name and declines, and the
//! item asked for `421`, which says the request named another server. Each `421` below was a
//! `403` on `4a9bc19`; everything else here passed there too and pins what must not move.
//!
//! Runs the binary this crate builds over a scratch home ([`scratch_gonk`]); nothing here
//! touches a live gonk.

use std::io::{Read, Write};
use std::net::TcpStream;

mod scratch_gonk;

/// One raw HTTP/1.1 exchange with exactly the `Host` given (none when `host` is `None`).
fn raw(
    gonk: &scratch_gonk::Gonk,
    method: &str,
    target: &str,
    host: Option<&str>,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(gonk.http).expect("connect");
    let mut head = format!("{method} {target} HTTP/1.1\r\n");
    if let Some(host) = host {
        head.push_str(&format!("Host: {host}\r\n"));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
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

/// A description, an open resource, a page, an asset, a ceremony, a write and a preflight:
/// one of each kind of thing a rebinding page could ask for.
const ROUTES: [(&str, &str); 9] = [
    ("GET", "/iki/ledger/append?description"),
    ("GET", "/l/default?description"),
    ("GET", "/iki/ledger/items"),
    ("GET", "/l/default"),
    ("GET", "/"),
    ("GET", "/static/gonk.css"),
    ("POST", "/auth/login-options"),
    ("POST", "/iki/ledger/append"),
    ("OPTIONS", "/iki/ledger/append"),
];

#[test]
fn a_foreign_host_is_misdirected_on_every_route_before_anything_answers() {
    let gonk = scratch_gonk::Gonk::start("");
    let port = gonk.http.port();
    let foreign = [
        format!("evil.example:{port}"),
        "evil.example".to_string(),
        // A rebinding page's own name, resolved to 127.0.0.1 by its DNS.
        format!("rebind.attacker.test:{port}"),
        // Our name on another port is another server.
        format!("localhost:{}", port.wrapping_add(1).max(1)),
        format!("127.0.0.1:{}", port.wrapping_add(1).max(1)),
        // A loopback-looking prefix is not the name.
        format!("localhost.evil.example:{port}"),
    ];
    for host in &foreign {
        for (method, target) in ROUTES {
            let (status, body) = raw(&gonk, method, target, Some(host), &[], "a rebound write");
            assert_eq!(status, 421, "{method} {target} under Host {host}: {body}");
            assert!(
                body.contains("loopback name"),
                "{method} {target} under Host {host} says why: {body}"
            );
            assert!(
                !body.contains("openapi") && !body.contains("urn:cap:"),
                "{method} {target} under Host {host} leaks no contract: {body}"
            );
        }
    }
    // No `Host` at all is not this server's name either.
    let (status, body) = raw(&gonk, "GET", "/l/default", None, &[], "");
    assert_eq!(status, 421, "no Host: {body}");

    // ⚠ Nothing a refused request asked for ran: the write above under every foreign Host
    // left the ledger empty.
    let (status, body) = raw(
        &gonk,
        "GET",
        "/iki/ledger/items",
        Some(&format!("localhost:{port}")),
        &[],
        "",
    );
    assert_eq!(status, 200, "{body}");
    assert!(
        !body.contains("a rebound write"),
        "a refused write reached the store: {body}"
    );
}

#[test]
fn every_spelling_of_our_own_name_is_answered() {
    let gonk = scratch_gonk::Gonk::start("");
    let port = gonk.http.port();
    // The three loopback names, with this port and bare (a default-port `Host` omits it),
    // in any letter case — a host name is case-insensitive.
    let ours = [
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "[::1]".to_string(),
        format!("LocalHost:{port}"),
    ];
    for host in &ours {
        for (method, target) in [
            ("GET", "/"),
            ("GET", "/l/default"),
            ("GET", "/iki/ledger/items"),
            ("GET", "/l/default?description"),
            ("GET", "/static/gonk.css"),
        ] {
            let (status, body) = raw(&gonk, method, target, Some(host), &[], "");
            assert_eq!(status, 200, "{method} {target} under Host {host}: {body}");
        }
        let (status, body) = raw(&gonk, "OPTIONS", "/iki/ledger/append", Some(host), &[], "");
        assert_eq!(status, 204, "OPTIONS under Host {host}: {body}");
    }
    // A write from a local process, under our name: still the door's whole point.
    let (status, body) = raw(
        &gonk,
        "POST",
        "/iki/ledger/append",
        Some(&format!("localhost:{port}")),
        &[],
        "Filed under our own name",
    );
    assert_eq!(status, 200, "{body}");
}

#[test]
fn a_cross_site_write_under_our_name_is_still_forbidden_not_misdirected() {
    let gonk = scratch_gonk::Gonk::start("");
    let host = format!("localhost:{}", gonk.http.port());
    for header in [
        ("Origin", "http://evil.example"),
        ("Sec-Fetch-Site", "cross-site"),
        ("Sec-Fetch-Site", "same-site"),
    ] {
        let (status, body) = raw(
            &gonk,
            "POST",
            "/iki/ledger/append",
            Some(&host),
            &[header],
            "from another site",
        );
        assert_eq!(status, 403, "a cross-site write with {header:?}: {body}");
        assert!(body.contains("another site"), "{body}");
    }
    // And both at once: the name is checked first, so the answer is the misdirection.
    let (status, body) = raw(
        &gonk,
        "POST",
        "/iki/ledger/append",
        Some("evil.example"),
        &[("Origin", "http://evil.example")],
        "x",
    );
    assert_eq!(status, 421, "{body}");
}
