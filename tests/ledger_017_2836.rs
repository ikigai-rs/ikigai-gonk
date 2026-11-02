//! Ledger #2836: gonk on `ikigai-ledger` 0.17, through a REAL gonk in a child process, under the
//! grant lists an operator mints.
//!
//! - A worker's hand-back records the worker as committer and its enrolled author as a claim, and
//!   a principal's free-text or unenrolled `author=` is refused.
//! - An alias and a `blocks` edge across two ledgers go through the HTTP door, and the store
//!   tokens they need write through the ledger only: the door still refuses them raw.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

mod common;
mod scratch_gonk;

use common::Authenticator;
use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_gonk::grants::{alias_grants, grants_for, link_in_grants, mirror_grants, Authority};
use ikigai_gonk::{identity, participants, quic};
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

const CLAUDE: &str = "urn:collab:participant:lawnboy-claude";
const CODEX: &str = "urn:collab:participant:lawnboy-codex";

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn json(answer: &Representation) -> serde_json::Value {
    serde_json::from_slice(&answer.bytes).expect("the ledger's JSON face")
}

fn gonk_cli(config: &Path, home: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(args)
        .arg("--config-home")
        .arg(config)
        .env("HOME", home)
        .output()
        .expect("run ikigai-gonk");
    assert!(
        out.status.success(),
        "ikigai-gonk {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

impl Gonk {
    fn owner(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }

    fn as_client(&self, name: &str, request: Request) -> Result<Representation, Error> {
        let bundle = self.layout.clients_dir().join(name);
        let client = self.dial_quic_as(&bundle).expect("dial the QUIC door");
        client.issue(request).map(|(repr, _)| repr)
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.http.port())
    }

    fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(self.http).expect("connect");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n",
            self.http.port()
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
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let response = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    fn auth(&self, path: &str, body: &str) -> (u16, String) {
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

    /// Enroll a passkey under `grant`, sign in, return the session token.
    fn sign_in(&self, grant: &[String]) -> String {
        static PERSON: AtomicUsize = AtomicUsize::new(0);
        let name = format!("person-{}", PERSON.fetch_add(1, Ordering::Relaxed));
        let authenticator = Authenticator::named(&name);
        let invite = identity::invite(
            &self.layout,
            &name,
            grant,
            false,
            30,
            identity::now_seconds(),
        )
        .unwrap();
        let challenge = |op: &str| {
            let (status, body) = self.auth(&format!("/auth/{op}"), "{}");
            assert_eq!(status, 200, "{body}");
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            v["challenge"].as_str().unwrap().to_string()
        };
        let c = challenge("register-options");
        let (status, body) = self.auth(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(status, 200, "{body}");
        let c = challenge("login-options");
        let (status, body) = self.auth(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        );
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["session"].as_str().unwrap().to_string()
    }

    /// A signed-in write through the HTTP door: `(status, body)`.
    fn write(&self, token: &str, method: &str, path: &str, body: &str) -> (u16, String) {
        self.raw(
            method,
            path,
            &[
                ("Content-Type", "text/plain".to_string()),
                ("Cookie", format!("{}={token}", identity::SESSION_COOKIE)),
            ],
            body,
        )
    }

    fn read(&self, token: &str, path: &str) -> (u16, String) {
        self.raw(
            "GET",
            path,
            &[
                ("Accept", "application/json".to_string()),
                ("Cookie", format!("{}={token}", identity::SESSION_COOKIE)),
            ],
            "",
        )
    }
}

/// `w1`, `w2` and `w3` on the default ledger as workers; `ledger enroll` runs once it is up.
fn with_workers() -> (Gonk, tempfile::TempDir) {
    let dirs = tempfile::tempdir().unwrap();
    let keep = std::sync::Mutex::new(None);
    let gonk = Gonk::with_quic_prepared("", &|config, home| {
        for name in ["w1", "w2", "w3"] {
            gonk_cli(
                config,
                home,
                &["client", "add", name, "--worker", "default"],
            );
        }
        *keep.lock().unwrap() = Some((config.to_path_buf(), home.to_path_buf()));
    });
    let (config, home) = keep.into_inner().unwrap().unwrap();
    std::fs::write(
        dirs.path().join("config"),
        config.to_string_lossy().as_bytes(),
    )
    .unwrap();
    std::fs::write(dirs.path().join("home"), home.to_string_lossy().as_bytes()).unwrap();
    (gonk, dirs)
}

fn enroll(gonk: &Gonk, dirs: &tempfile::TempDir, args: &[&str]) -> String {
    let path = |name: &str| std::fs::read_to_string(dirs.path().join(name)).unwrap();
    let socket = gonk.socket.to_string_lossy().into_owned();
    let mut all = vec!["ledger"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--socket", &socket]);
    gonk_cli(Path::new(&path("config")), Path::new(&path("home")), &all)
}

fn file(gonk: &Gonk, title: &str) {
    gonk.owner(request(
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", title)],
    ))
    .expect("the owner files");
}

#[test]
fn a_hand_back_records_the_worker_as_committer_and_a_claimed_author() {
    let (gonk, dirs) = with_workers();
    let w1 = quic::client_iri_of(&gonk.layout, "w1").unwrap();
    // Enrolled by client name.
    let said = enroll(&gonk, &dirs, &["enroll", "w1", "--acts-for", CLAUDE]);
    assert!(said.contains(CLAUDE), "{said}");
    file(&gonk, "Work for a worker");

    gonk.as_client(
        "w1",
        request(
            Verb::Sink,
            "urn:iki:ledger:claim",
            &[("item", "1"), ("content", "lawnboy-claude/1")],
        ),
    )
    .expect("the worker claims");
    gonk.as_client(
        "w1",
        request(
            Verb::Sink,
            "urn:iki:ledger:comment",
            &[
                ("item", "1"),
                ("handback", "true"),
                ("author", CLAUDE),
                ("content", "Done; over to the dispatcher"),
            ],
        ),
    )
    .expect("an enrolled worker hands back under its author");
    gonk.as_client(
        "w1",
        request(
            Verb::Sink,
            "urn:iki:ledger:comment",
            &[("item", "1"), ("content", "And in its own name")],
        ),
    )
    .expect("a worker writes as itself");

    let item = json(
        &gonk
            .owner(request(
                Verb::Source,
                "urn:iki:ledger:item:1",
                &[("as", "application/json")],
            ))
            .unwrap(),
    );
    let comments = item["item"]["comments"].as_array().unwrap();
    let handback = &comments[0]["attribution"];
    assert_eq!(handback["committer"], w1.as_str(), "{item}");
    assert_eq!(handback["author"], CLAUDE, "{item}");
    assert_eq!(handback["author_status"], "claimed", "{item}");
    assert_eq!(handback["author_source"], "delegate", "{item}");
    let own = &comments[1]["attribution"];
    assert_eq!(own["author"], w1.as_str(), "{item}");
    assert_eq!(own["committer"], w1.as_str(), "{item}");
    assert_eq!(own["author_status"], "verified", "{item}");

    // The page says the same in words.
    let (status, page) = gonk.raw(
        "GET",
        "/l/default/item/1",
        &[
            ("Accept", "text/html".to_string()),
            ("Sec-Fetch-Site", "same-origin".to_string()),
            ("Sec-Fetch-Mode", "navigate".to_string()),
            ("Sec-Fetch-Dest", "document".to_string()),
        ],
        "",
    );
    assert_eq!(status, 200, "{page}");
    assert!(
        page.contains(&format!(
            "{CLAUDE} (claimed by a delegate, committed by {w1})"
        )),
        "{page}"
    );
}

#[test]
fn a_principals_other_authors_are_refused() {
    let (gonk, dirs) = with_workers();
    enroll(&gonk, &dirs, &["enroll", "w1", "--acts-for", CLAUDE]);
    file(&gonk, "Work for a worker");
    let comment = |client: &str, author: &str| {
        gonk.as_client(
            client,
            request(
                Verb::Sink,
                "urn:iki:ledger:comment",
                &[("item", "1"), ("author", author), ("content", "words")],
            ),
        )
    };
    for (client, author, why) in [
        ("w1", "hermes", "free text"),
        ("w1", CODEX, "an author it was not enrolled for"),
        ("w2", CLAUDE, "another client's author"),
    ] {
        match comment(client, author) {
            Err(Error::Denied(refusal)) => {
                assert!(refusal.contains(&format!("author={author}")), "{refusal}")
            }
            other => panic!("{why}: {other:?}"),
        }
    }

    // A worker cannot enroll itself: the participants graph is refused raw at the door.
    let w2 = quic::client_iri_of(&gonk.layout, "w2").unwrap();
    let triple = participants::statement(&w2, &participants::Role::Importer).unwrap();
    let update = format!(
        "INSERT DATA {{ GRAPH <{}> {{ {triple} }} }}",
        participants::GRAPH
    );
    let raw = gonk.as_client(
        "w2",
        request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[("graph", participants::GRAPH), ("content", &update)],
        ),
    );
    assert!(matches!(raw, Err(Error::Denied(_))), "{raw:?}");

    // Unenrolled, the claim is refused again.
    enroll(&gonk, &dirs, &["unenroll", "w1", "--acts-for", CLAUDE]);
    assert!(matches!(comment("w1", CLAUDE), Err(Error::Denied(_))));
}

#[test]
fn an_import_principal_names_a_foreign_actor() {
    let (gonk, dirs) = with_workers();
    enroll(&gonk, &dirs, &["enroll", "w3", "--importer"]);
    file(&gonk, "Imported");
    gonk.as_client(
        "w3",
        request(
            Verb::Sink,
            "urn:iki:ledger:comment",
            &[
                ("item", "1"),
                ("author", "urn:example:github:user:octocat"),
                ("created", "2020-01-02T03:04:05Z"),
                ("content", "From before"),
            ],
        ),
    )
    .expect("an importer claims");
    let item = json(
        &gonk
            .owner(request(
                Verb::Source,
                "urn:iki:ledger:item:1",
                &[("as", "application/json")],
            ))
            .unwrap(),
    );
    let claim = &item["item"]["comments"][0]["attribution"];
    assert_eq!(claim["author_source"], "import", "{item}");
}

/// A grant over two ledgers, as the flags mint it: `write` on `cli`, read on `acme`, the link in.
fn linker() -> Vec<String> {
    let mut grant = grants_for("cli", Authority::Write).unwrap();
    grant.extend(grants_for("acme", Authority::Read).unwrap());
    grant.extend(link_in_grants("acme").unwrap());
    grant.extend(alias_grants("cli", "gh").unwrap());
    grant.sort();
    grant.dedup();
    grant
}

fn items(gonk: &Gonk) {
    for ledger in ["cli", "acme"] {
        gonk.owner(request(
            Verb::Sink,
            &format!("urn:iki:ledger:{ledger}:append"),
            &[("content", &format!("In {ledger}"))],
        ))
        .expect("the owner files");
    }
}

#[test]
fn an_alias_through_the_http_door() {
    let gonk = Gonk::start("");
    items(&gonk);
    let token = gonk.sign_in(&linker());

    let (status, body) = gonk.write(&token, "POST", "/iki/alias/scheme/gh", "");
    assert_eq!(status, 200, "register the scheme: {body}");
    let (status, body) = gonk.write(
        &token,
        "POST",
        "/iki/alias/scheme/gh/value/acme%252Fweb%252312",
        "cli#1",
    );
    assert_eq!(status, 200, "give the alias: {body}");

    let (status, body) = gonk.read(
        &token,
        "/iki/ledger/cli/item/gh:acme%252Fweb%252312?as=application/json",
    );
    assert_eq!(status, 200, "resolve the alias: {body}");
    let item: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(item["item"]["number"], 1, "{item}");

    // Without the scheme's grant, a holder of the ledger's write grant registers nothing.
    let other = gonk.sign_in(&grants_for("cli", Authority::Write).unwrap());
    let (status, body) = gonk.write(&other, "POST", "/iki/alias/scheme/kata", "");
    assert_eq!(status, 403, "{body}");

    // The aliases graph's store token is the ledger's to use, not the caller's: refused raw.
    let forged = format!(
        "INSERT DATA {{ GRAPH <{0}> {{ <{1}> <urn:x> <urn:y> }} }}",
        ikigai_ledger::alias::GRAPH,
        ikigai_ledger::alias::scheme_iri("gh")
    );
    let (status, body) = gonk.write(
        &token,
        "POST",
        &format!(
            "/iki/store/graph-update?graph={}",
            ikigai_ledger::alias::GRAPH
        ),
        &forged,
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("raw store write"), "{body}");
}

#[test]
fn a_blocks_edge_across_ledgers_through_the_http_door() {
    let gonk = Gonk::start("");
    items(&gonk);
    let token = gonk.sign_in(&linker());

    // `write` on the target does not stand in for the link in, and it is checked first.
    let mut no_link_in = grants_for("cli", Authority::Write).unwrap();
    no_link_in.extend(grants_for("acme", Authority::Write).unwrap());
    let refused = gonk.sign_in(&no_link_in);
    let (status, body) = gonk.write(
        &refused,
        "POST",
        "/iki/ledger/cli/link?item=1&type=blocks",
        "acme#1",
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("link-in"), "{body}");

    let (status, body) = gonk.write(
        &token,
        "POST",
        "/iki/ledger/cli/link?item=1&type=blocks",
        "acme#1",
    );
    assert_eq!(status, 200, "link across: {body}");

    // The target's ready set excludes it, read under the target's grants alone.
    let reader = gonk.sign_in(&grants_for("acme", Authority::Read).unwrap());
    let (status, body) = gonk.read(&reader, "/iki/ledger/acme/next?as=application/json");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("blocked_elsewhere"), "{body}");

    // A closer with the mirror's grants and no link in: the close writes the mirror.
    let mut closer = grants_for("cli", Authority::Write).unwrap();
    closer.extend(mirror_grants("acme").unwrap());
    let closer = gonk.sign_in(&closer);
    let (status, body) = gonk.write(
        &closer,
        "POST",
        "/iki/ledger/cli/link?item=1&type=blocks&_x=1",
        "acme#1",
    );
    assert_eq!(status, 403, "the mirror's grants add no link in: {body}");

    // Without them the close is refused before anything changes.
    let bare = gonk.sign_in(&grants_for("cli", Authority::Write).unwrap());
    let (status, body) = gonk.write(&bare, "POST", "/iki/ledger/cli/close?item=1", "done");
    assert_eq!(status, 403, "{body}");

    let (status, body) = gonk.write(&closer, "POST", "/iki/ledger/cli/close?item=1", "done");
    assert_eq!(status, 200, "close across: {body}");
    let (status, body) = gonk.read(&reader, "/iki/ledger/acme/next?as=application/json");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("In acme"), "{body}");

    // ★ The target's store write token writes only mirrors: refused raw at the door.
    let target = "urn:iki:ledger:graph:acme";
    let forged = format!("INSERT DATA {{ GRAPH <{target}> {{ <urn:x> <urn:y> <urn:z> }} }}");
    let (status, body) = gonk.write(
        &closer,
        "POST",
        &format!("/iki/store/graph-update?graph={target}"),
        &forged,
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("raw store write"), "{body}");
}
