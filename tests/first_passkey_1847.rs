//! Ledger [#1847](http://localhost:1062/l/default/item/1847), from a report on a fresh gonk under
//! WSL2: the sign-in button raised the system's passkey dialog (the Windows dialog, then an iPhone
//! QR) and it failed, with nothing saying that `ikigai-gonk passkey invite` comes first.
//!
//! And `--ledger default=write` was refused as "adds nothing" while the anonymous loopback caller
//! still wrote `default`. Since ledger #1687 that caller only reads, so `write` is accepted, and
//! what is left to refuse is a grant it really does already hold: the refusal says that, and
//! offers the next step up on the ledger that was asked for.
//!
//! The real binary over scratch config homes.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Output};

mod common;
mod scratch_gonk;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity;
use scratch_gonk::Gonk;

/// The sentence's start, which is on the page exactly when the note is. The stylesheet's writer
/// single-quotes attributes and leaves this text alone.
const NOTE: &str = "No passkey is enrolled on this server yet";

/// The page at `path`, as a browser navigating to it: its status and body.
fn navigate(gonk: &Gonk, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(gonk.http).expect("connect");
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html\r\nSec-Fetch-Mode: navigate\r\n\
         Sec-Fetch-Dest: document\r\nConnection: close\r\n\r\n",
        gonk.http.port()
    );
    stream.write_all(head.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

/// Enroll one passkey through its one-time invite, over the door as a browser would.
fn enroll(gonk: &Gonk) {
    let post = |path: &str, body: &str| -> (u16, String) {
        let mut stream = TcpStream::connect(gonk.http).expect("connect");
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: application/json\r\n\
             Content-Type: application/json\r\nOrigin: http://localhost:{port}\r\n\
             Sec-Fetch-Site: same-origin\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len(),
            port = gonk.http.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, body.to_string())
    };
    let invite = identity::invite(
        &gonk.layout,
        "ada",
        &grants_for("default", Authority::Write).unwrap(),
        false,
        5,
        identity::now_seconds(),
    )
    .expect("invite");
    let (status, body) = post("/auth/register-options", "{}");
    assert_eq!(status, 200, "{body}");
    let options: serde_json::Value = serde_json::from_str(&body).unwrap();
    let origin = format!("http://localhost:{}", gonk.http.port());
    let registration = Authenticator::new().register_body(
        options["challenge"].as_str().unwrap(),
        &invite,
        &origin,
    );
    let (status, body) = post("/auth/register", &registration);
    assert_eq!(status, 200, "{body}");
}

#[test]
fn a_fresh_gonk_says_an_invite_comes_first_until_a_passkey_is_enrolled() {
    let gonk = Gonk::start("");

    // The page's own words, as text, and with no script: a person reads the way on before pressing
    // the button that fails. A ledger the caller reads is the ledger in the command.
    let (status, page) = navigate(&gonk, "/l/default");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains(NOTE), "{page}");
    assert!(
        page.contains(
            "<code>ikigai-gonk passkey invite &lt;name&gt; --ledger default=write</code>"
        ),
        "the command to run, as code: {page}"
    );
    let note = page.find("id='first-passkey'").expect("the note's id");
    assert!(
        !page[note..page[note..].find('>').unwrap() + note].contains("hidden"),
        "no script is needed to read it: {page}"
    );
    // A refusal page is where the sign-in takes focus, so it says the same.
    let (status, refused) = navigate(&gonk, "/l/ventures");
    assert_eq!(status, 403, "{refused}");
    assert!(refused.contains(NOTE), "{refused}");
    // One note on a page.
    assert_eq!(page.matches("id='first-passkey'").count(), 1, "{page}");

    enroll(&gonk);

    // No restart, and no page held over from before: the next page has no note.
    for path in ["/l/default", "/l/ventures"] {
        let (_, page) = navigate(&gonk, path);
        assert!(!page.contains(NOTE), "{path} after enrolling: {page}");
        assert!(!page.contains("first-passkey"), "{path}: {page}");
    }
}

#[test]
fn the_script_shows_the_way_on_to_a_control_and_takes_it_down_on_enrolling() {
    let script = include_str!("../web/gonk.js");
    let stylesheet = include_str!("../web/gonk.xsl");
    // WIRING: the buttons are shown only by the script, so it is the script that points them at
    // the sentence (a note beside its control, ACCESSIBILITY.md); an invite link and a created
    // passkey each make the sentence false, so each takes it down.
    assert!(script.contains("describeSignIn(scope)"), "{script}");
    assert!(
        script.contains("hideFirstPasskey()"),
        "an invite link and a created passkey: {script}"
    );
    assert!(stylesheet.contains("id=\"first-passkey\""), "{stylesheet}");
}

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    fn new(config: &str) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for dir in [&root.join("home"), &root.join("xdg/ikigai")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(root.join("xdg/ikigai/config.toml"), config).unwrap();
        World { _tmp: tmp, root }
    }

    fn invite(&self, flags: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(["passkey", "invite", "brian"])
            .args(flags)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("xdg"))
            .output()
            .expect("run ikigai-gonk")
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn write_on_an_anonymous_ledger_is_accepted_since_anonymous_only_reads() {
    let out = World::new("").invite(&["--ledger", "default=write"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("#invite="),
        "{out:?}"
    );
}

#[test]
fn read_on_an_anonymous_ledger_is_refused_for_the_reason_and_offers_write_there() {
    let world = World::new("gonk.http.ledger = \"default\"\ngonk.http.ledger = \"crew\"\n");
    let out = world.invite(&["--ledger", "crew=read"]);
    assert!(!out.status.success());
    let why = stderr(&out);
    for said in [
        "that grant adds nothing",
        // The reason, and the setting that is its cause.
        "anonymous loopback caller already holds all of it",
        "it reads default, crew (`gonk.http.ledger`)",
        // The next step up, on the ledger that was asked for.
        "`--ledger crew=write`",
    ] {
        assert!(why.contains(said), "{said}: {why}");
    }
    // Not a wider role than the one that cures it.
    assert!(!why.contains("=delete"), "{why}");
    // Refused before anything was minted.
    assert!(
        !world.root.join("xdg/ikigai/gonk/grants.json").exists()
            || !std::fs::read_to_string(world.root.join("xdg/ikigai/gonk/grants.json"))
                .unwrap()
                .contains("brian"),
        "{why}"
    );
}

#[test]
fn a_public_script_grant_is_refused_for_the_scripts_not_for_a_ledger_alone() {
    let out = World::new("").invite(&["--script", "public=run"]);
    assert!(!out.status.success());
    let why = stderr(&out);
    for said in [
        "that grant adds nothing",
        // What was asked for is in the reason, not only the ledgers it did not ask about.
        "reads and runs public scripts",
        "`--ledger default=write`",
    ] {
        assert!(why.contains(said), "{said}: {why}");
    }
}
