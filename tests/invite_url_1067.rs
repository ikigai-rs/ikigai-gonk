//! Ledger [#1067](http://localhost:1060/l/default/item/1067): `passkey invite` printed
//! `http://localhost:{port}/#invite=…` whatever `gonk.bind` said.
//!
//! An invite is opened to run a WebAuthn ceremony, and the relying party is always `localhost`
//! (WebAuthn refuses an IP address), so the link cannot fall back to the bind's own IP the way
//! the startup banner does ([`ikigai_gonk::doors::http_url`]). On `127.0.0.2` the printed link
//! named an address where nothing listens, and the command still wrote a grant and minted the
//! invite. Reproduced before the fix as
//! [`an_invite_on_a_bind_localhost_cannot_reach_is_refused_and_writes_nothing`], which saw the
//! command succeed and print `http://localhost:1070/#invite=`.
//!
//! The real binary against a scratch config home: no server is started, nothing touches the
//! operator's config home.

use std::path::PathBuf;
use std::process::{Command, Output};

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    fn bound(bind: &str) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for dir in [&root.join("home"), &root.join("xdg/ikigai")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            root.join("xdg/ikigai/config.toml"),
            format!("gonk.bind = \"{bind}\"\n"),
        )
        .unwrap();
        World { _tmp: tmp, root }
    }

    fn gonk_dir(&self) -> PathBuf {
        self.root.join("xdg/ikigai/gonk")
    }

    fn invite(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(["passkey", "invite", "brian", "--ledger", "default=delete"])
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("xdg"))
            .output()
            .expect("run ikigai-gonk")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn an_invite_on_127_0_0_1_opens_at_localhost_with_no_warning() {
    let world = World::bound("127.0.0.1:1070");
    let out = world.invite();
    assert!(out.status.success(), "{}", stderr(&out));
    let said = stdout(&out);
    assert!(
        said.contains("  open  http://localhost:1070/#invite="),
        "{said}"
    );
    assert!(!said.contains('⚠'), "no warning on 127.0.0.1: {said}");
    assert!(world.gonk_dir().join("invites.json").exists());
}

#[test]
fn an_invite_on_ipv6_loopback_says_localhost_must_resolve_to_it() {
    let world = World::bound("[::1]:1070");
    let out = world.invite();
    assert!(out.status.success(), "{}", stderr(&out));
    let said = stdout(&out);
    assert!(
        said.contains("  open  http://localhost:1070/#invite="),
        "{said}"
    );
    assert!(
        said.contains("only where localhost resolves to ::1"),
        "the [::1] caveat is printed: {said}"
    );
}

#[test]
fn an_invite_on_a_bind_localhost_cannot_reach_is_refused_and_writes_nothing() {
    let world = World::bound("127.0.0.2:1070");
    let out = world.invite();
    assert!(
        !out.status.success(),
        "an invite on 127.0.0.2 must be refused, not printed: {}",
        stdout(&out)
    );
    assert!(
        !stdout(&out).contains("#invite="),
        "no dead link: {}",
        stdout(&out)
    );
    let why = stderr(&out);
    for part in [
        "a passkey invite cannot work on this bind (127.0.0.2:1070)",
        "localhost names 127.0.0.1, not 127.0.0.2",
        "--bind 127.0.0.1:1070",
    ] {
        assert!(why.contains(part), "`{part}` in: {why}");
    }
    assert!(
        !world.gonk_dir().join("invites.json").exists(),
        "a refused invite mints nothing"
    );
    assert!(
        !world.gonk_dir().join("grants.json").exists(),
        "a refused invite writes no grant"
    );
}
