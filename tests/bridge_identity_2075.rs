//! Ledger [#2075](http://localhost:1062/l/default/item/2075): the bridges file with an
//! identity, and the HTTP door's anonymous caller still writes nothing.
//!
//! Through ledger #1687 `roborev file` and `kata import` filed over the HTTP door as an
//! anonymous loopback caller. #1687 made that caller read-only, so both answered 403 and filed
//! nothing — reproduced on cb50107 with this file's first test, both commands exiting 1 with
//! `gonk refused POST /iki/ledger/append (403)`. They file over the owner-only socket now
//! (`crate::bridge`). Each test runs the REAL binary as `serve`, over a scratch home
//! (`tests/scratch_gonk`), and the bridges as the commands a roborev hook and a person run.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Output};

mod scratch_gonk;

use ikigai_core::{ArgRef, Iri, Request, Verb};
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

const REVIEW: &str = include_str!("fixtures/roborev-review-completed.md");
const EXPORT: &str = "tests/fixtures/kata-export.jsonl";

/// The binary, with HOME a scratch directory: no `~/.ikigai` of this machine is read.
fn bridge(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"));
    command.env("HOME", home);
    command
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The default ledger's items, every status, in its JSON face, read over the socket.
fn items(gonk: &Gonk) -> Vec<serde_json::Value> {
    let client = ikigai_ipc::connect(&gonk.socket).expect("connect");
    let request = [("status", "all"), ("as", "application/json")].iter().fold(
        Request::new(Verb::Source, Iri::parse("urn:iki:ledger:items").unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let (repr, _) = client.issue(request).expect("the items");
    let json: serde_json::Value = serde_json::from_slice(&repr.bytes).unwrap();
    json["items"].as_array().cloned().unwrap_or_default()
}

/// One anonymous POST at the HTTP door, as the bridges used to send it: `(status, body)`.
fn anonymous_post(gonk: &Gonk, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(gonk.http).expect("connect");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAccept: application/json\r\n\
         Content-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        gonk.http.port(),
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, body.to_string())
}

/// ★ The item's claim: a bridge files, and its items are there, filed as the bridge says.
#[test]
fn both_bridges_file_over_the_owners_socket() {
    let gonk = Gonk::start("");
    let home = tempfile::tempdir().unwrap();

    // The hook line a roborev config carries, and the import a person types.
    let review = || {
        stdout(
            &bridge(home.path())
                .args(["roborev", "file", "--socket"])
                .arg(&gonk.socket)
                .args(["--ledger", "default", "--root", "demo"])
                .args(["--job", "42", "--repo-path", "/work/demo"])
                .args(["--findings", REVIEW])
                .output()
                .unwrap(),
        )
    };
    let import = || {
        stdout(
            &bridge(home.path())
                .args(["kata", "import", EXPORT, "--socket"])
                .arg(&gonk.socket)
                .output()
                .unwrap(),
        )
    };

    let filed = review();
    assert_eq!(filed.matches("filed ").count(), 3, "{filed}");
    let imported = import();
    assert!(imported.contains("3 filed, 0 already there"), "{imported}");
    assert!(
        imported.contains("2 comment(s), 1 close(s), 3 link(s) added"),
        "{imported}"
    );

    let all = items(&gonk);
    assert_eq!(all.len(), 6, "{all:#?}");
    let authors: Vec<&str> = all.iter().filter_map(|i| i["author"].as_str()).collect();
    assert_eq!(
        authors.iter().filter(|a| **a == "roborev").count(),
        3,
        "roborev's items are filed `author roborev`: {authors:?}"
    );

    // Run again: the keys hold over the socket as they did over HTTP, so nothing is new.
    let again = review();
    assert_eq!(again.matches("already ").count(), 3, "{again}");
    let again = import();
    assert!(again.contains("0 filed, 3 already there"), "{again}");
    assert_eq!(items(&gonk).len(), 6);
}

/// The socket is found where `serve` put it, from the same config a server reads: a hook line
/// names no path when gonk runs with its defaults.
#[test]
fn the_socket_comes_from_the_config() {
    let gonk = Gonk::start("");
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.toml");
    std::fs::write(
        &config,
        format!("gonk.socket = \"{}\"\n", gonk.socket.display()),
    )
    .unwrap();
    let review = stdout(
        &bridge(home.path())
            .args(["roborev", "file", "--config"])
            .arg(&config)
            .args(["--ledger", "default", "--root", "demo"])
            .args(["--findings", REVIEW])
            .output()
            .unwrap(),
    );
    assert_eq!(review.matches("filed ").count(), 3, "{review}");
}

/// What the item did NOT ask for: the anonymous HTTP caller stays read-only, and the bridges'
/// old way in is refused, saying what to do instead.
#[test]
fn an_anonymous_http_write_is_still_refused() {
    let gonk = Gonk::start("");
    let home = tempfile::tempdir().unwrap();

    // The very request `roborev file` sent through ledger #1687's release.
    let (status, body) = anonymous_post(
        &gonk,
        "/iki/ledger/append?key=urn%3Aroborev%3Afinding%3A00&author=roborev&as=application%2Fjson",
        "A finding\n\nFiled by nobody.",
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("capability does not grant"), "{body}");

    // `--gonk` named that door: refused before anything is sent, and the refusal names `--socket`.
    let http = format!("http://127.0.0.1:{}", gonk.http.port());
    for args in [
        vec!["roborev", "file", "--ledger", "default", "--root", "demo"],
        vec!["kata", "import", EXPORT],
    ] {
        let mut command = bridge(home.path());
        command.args(&args).args(["--gonk", &http]);
        if args[0] == "roborev" {
            command.args(["--findings", REVIEW]);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--socket"), "{stderr}");
        assert!(stderr.contains("#1687"), "{stderr}");
    }
    assert!(items(&gonk).is_empty(), "nothing was filed anywhere");
}
