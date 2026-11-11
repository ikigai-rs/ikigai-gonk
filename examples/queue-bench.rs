//! `cargo run --release --example queue-bench -- --archive <backup.nq[.gz]> --store <dir>
//! --roots <config.toml> [--gonk <binary>] [--polls N] [--page PATH]…` — how long the Queue page
//! and the header badge take for a SIGNED-IN caller on a store of a real shape (ledger
//! [#947](http://localhost:1060/l/default/item/947)).
//!
//! What it runs, and why each piece is the real one:
//!
//! - **The store** is a gonk backup loaded into `--store` through `urn:iki:store:load`, the door
//!   `urn:iki:gonk:restore` loads through. Loaded once: an existing `--store` is reused, so a
//!   before/after pair measures the same dataset. Never the live store — a backup is a file
//!   `urn:iki:gonk:backup` already wrote, and nothing here opens `~/.ikigai/store`.
//! - **The server** is the `ikigai-gonk` binary (`--gonk`, default `target/release/…`) over a
//!   scratch config home and data home, on a random loopback port, with no QUIC and no backup.
//!   Its config carries only the `gonk.browse.root`, `gonk.queue.*` and `gonk.review.space`
//!   lines of `--roots`: the roots are real checkouts, read and watched, never written.
//! - **The caller** is a software passkey enrolled through `POST /auth/register` under a grant
//!   holding `urn:cap:browse:read:*` and `urn:cap:annotate` (what the live `brian` grant holds
//!   for the Queue), then signed in through `POST /auth/login`: every GET carries its
//!   `gonk_session` cookie, as a browser does.
//!
//! It prints the wall time of each GET and the `dur=` the server's own access line recorded,
//! which is the number ledger #947 was diagnosed from.

#[path = "../tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::Authenticator;
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_store::DurableStore;

/// The live `brian` grant's Queue-relevant half: the ledger, the browse family, and annotate.
const GRANT: [&str; 10] = [
    "urn:cap:ledger:read:default",
    "urn:cap:store:read:graph:urn:iki:ledger:graph:default",
    "urn:cap:ledger:write:default",
    "urn:cap:store:write:graph:urn:iki:ledger:graph:default",
    "urn:cap:ledger:delete:default",
    "urn:cap:store:write:graph:urn:iki:ledger:graph:default:deleted",
    "urn:cap:browse:read:*",
    "urn:cap:store:read:graph:urn:iki:browse:graph:default",
    "urn:cap:store:write:graph:urn:iki:browse:graph:default",
    "urn:cap:annotate",
];

struct Args {
    archive: Option<PathBuf>,
    store: PathBuf,
    roots: PathBuf,
    gonk: PathBuf,
    polls: usize,
    pages: Vec<String>,
    probe: bool,
    hold: u64,
}

fn args() -> Args {
    let mut it = std::env::args().skip(1);
    let mut archive = None;
    let mut store = None;
    let mut roots = None;
    let mut gonk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/ikigai-gonk");
    let mut polls = 5;
    let mut pages = Vec::new();
    let mut probe = true;
    let mut hold = 0;
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--archive" => archive = Some(PathBuf::from(value())),
            "--store" => store = Some(PathBuf::from(value())),
            "--roots" => roots = Some(PathBuf::from(value())),
            "--gonk" => gonk = PathBuf::from(value()),
            "--polls" => polls = value().parse().expect("--polls N"),
            "--page" => pages.push(value()),
            "--no-probe" => probe = false,
            "--hold" => hold = value().parse().expect("--hold SECONDS"),
            other => panic!("unknown argument {other}"),
        }
    }
    if pages.is_empty() {
        pages = vec!["/queue".to_string(), "/queue/rows".to_string()];
    }
    Args {
        archive,
        store: store.expect("--store <dir>"),
        roots: roots.expect("--roots <config.toml>"),
        gonk,
        polls,
        pages,
        probe,
        hold,
    }
}

/// Load `archive` into a new store at `dir` — or reuse `dir` if it already holds one.
fn load(archive: Option<&Path>, dir: &Path) {
    if dir.exists() {
        eprintln!("store: reusing {}", dir.display());
        return;
    }
    let archive = archive.expect("--archive: there is no store at --store yet");
    let started = Instant::now();
    let nquads = if archive.extension().is_some_and(|e| e == "gz") {
        let out = Command::new("gunzip")
            .arg("-c")
            .arg(archive)
            .output()
            .expect("gunzip");
        assert!(out.status.success(), "gunzip -c {}", archive.display());
        out.stdout
    } else {
        std::fs::read(archive).expect("the archive")
    };
    std::fs::create_dir_all(dir).expect("the store directory");
    let store = DurableStore::open(dir).expect("a new store");
    let kernel = Kernel::new(Arc::new(ikigai_store::space(store)));
    block_on(
        kernel.issue(
            Request::new(Verb::Sink, Iri::parse("urn:iki:store:load").unwrap())
                .with_arg("content", ArgRef::Inline(nquads))
                .with_arg("format", ArgRef::Inline(b"application/n-quads".to_vec())),
            &Capability::scoped([ikigai_store::CAP_WRITE]),
        ),
    )
    .expect("load");
    eprintln!(
        "store: loaded {} into {} in {:.1}s",
        archive.display(),
        dir.display(),
        started.elapsed().as_secs_f64()
    );
}

/// The lines of `config.toml` this bench copies: the roots and the Queue's own settings.
///
/// ⚠ A root written `~/…` is expanded HERE, against this process's home: the server runs with
/// `HOME` set to the scratch home, where `~` would name nothing.
fn config_lines(roots: &Path) -> String {
    let home = std::env::var("HOME").expect("HOME, to expand a `~/` root");
    let text = std::fs::read_to_string(roots)
        .expect("--roots")
        .replace("=~/", &format!("={home}/"));
    text.lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("gonk.browse.root")
                || line.starts_with("gonk.queue.")
                || line.starts_with("gonk.review.space")
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

struct Server {
    child: Child,
    port: u16,
    socket: PathBuf,
    access: Arc<Mutex<Vec<String>>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// One root's pending findings over the owner's socket, as the Queue reads them: wall
    /// time, rows, bytes.
    fn findings(&self, root: &str) -> (Duration, usize, usize) {
        let client = ikigai_ipc::connect(&self.socket).expect("the socket");
        let request = Request::new(
            Verb::Source,
            Iri::parse(format!("urn:repo:{root}:findings")).unwrap(),
        )
        .with_arg("as", ArgRef::Inline(b"application/json".to_vec()))
        .with_arg("state", ArgRef::Inline(b"pending".to_vec()));
        let started = Instant::now();
        let (answer, _) = ikigai_resolve::Resolver::issue(&client, request).expect("findings");
        let took = started.elapsed();
        let rows = serde_json::from_slice::<serde_json::Value>(&answer.bytes)
            .ok()
            .and_then(|v| v.as_array().map(Vec::len))
            .unwrap_or(0);
        (took, rows, answer.bytes.len())
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n",
            self.port
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
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or(raw.len());
        let status = String::from_utf8_lossy(&raw[..split])
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, raw[(split + 4).min(raw.len())..].to_vec())
    }

    fn json(&self, path: &str, body: &str) -> String {
        let (status, body) = self.request(
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
        let body = String::from_utf8_lossy(&body).into_owned();
        assert_eq!(status, 200, "POST {path}: {body}");
        body
    }

    fn challenge(&self, op: &str) -> String {
        let v: serde_json::Value =
            serde_json::from_str(&self.json(&format!("/auth/{op}"), "{}")).unwrap();
        v["challenge"].as_str().unwrap().to_string()
    }

    /// A signed-in GET, as the page or htmx sends one. Wall time, status, bytes.
    fn get(&self, path: &str, session: &str, htmx: bool) -> (Duration, u16, usize) {
        let mut headers = vec![
            ("Accept", "text/html".to_string()),
            ("Cookie", format!("gonk_session={session}")),
            ("Sec-Fetch-Site", "same-origin".to_string()),
        ];
        if htmx {
            headers.push(("HX-Request", "true".to_string()));
        }
        let started = Instant::now();
        let (status, body) = self.request("GET", path, &headers, "");
        (started.elapsed(), status, body.len())
    }

    /// The `dur=` of the last access line naming `iri`.
    fn last_dur(&self, iri: &str) -> Option<String> {
        let lines = self.access.lock().unwrap();
        lines
            .iter()
            .rev()
            .find(|line| line.contains(iri) && line.contains("dur="))
            .and_then(|line| {
                line.split_whitespace()
                    .find(|word| word.starts_with("dur="))
                    .map(str::to_string)
            })
    }
}

fn start(gonk: &Path, store: &Path, config_toml: &str, home: &Path) -> Server {
    let config = home.join("config");
    let data = home.join("data");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(config.join("config.toml"), config_toml).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let socket = PathBuf::from(format!("/tmp/qb{}", std::process::id())).join("s");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let mut child = Command::new(gonk)
        .arg("--config-home")
        .arg(&config)
        .arg("--data-home")
        .arg(&data)
        .arg("--store")
        .arg(store)
        .arg("--socket")
        .arg(&socket)
        .args(["--port", &port.to_string()])
        .arg("--no-quic")
        .arg("--no-backup")
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ikigai-gonk");
    let access = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&access);
    let stderr = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !line.contains("dur=") {
                eprintln!("  gonk| {line}");
            }
            sink.lock().unwrap().push(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(300);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("gonk exited starting: {status}");
        }
        assert!(Instant::now() < deadline, "gonk never came up");
        std::thread::sleep(Duration::from_millis(100));
    }
    Server {
        child,
        port,
        socket,
        access,
    }
}

fn main() {
    let args = args();
    load(args.archive.as_deref(), &args.store);
    let home = tempfile::tempdir().expect("a scratch home");
    let config_toml = config_lines(&args.roots);
    eprintln!(
        "config: {} browse roots",
        config_toml.matches("gonk.browse.root").count()
    );

    // The grant and its invite, written the way `passkey invite` writes them.
    let layout = ikigai_gonk::quic::Layout::in_config_home(&home.path().join("config"));
    let scopes: Vec<String> = GRANT.iter().map(|s| s.to_string()).collect();
    let now = ikigai_gonk::identity::now_seconds();
    let invite = ikigai_gonk::identity::invite(&layout, "bench", &scopes, false, 60, now)
        .expect("an invite");

    let started = Instant::now();
    let server = start(&args.gonk, &args.store, &config_toml, home.path());
    eprintln!("server: up in {:.1}s", started.elapsed().as_secs_f64());

    let key = Authenticator::new();
    let c = server.challenge("register-options");
    server.json(
        "/auth/register",
        &key.register_body(&c, &invite, &server.origin()),
    );
    let c = server.challenge("login-options");
    let login: serde_json::Value =
        serde_json::from_str(&server.json("/auth/login", &key.login_body(&c, &server.origin(), 1)))
            .unwrap();
    let session = login["session"].as_str().expect("a session").to_string();

    let mut rows: BTreeMap<usize, String> = BTreeMap::new();
    let mut n = 0;
    let mut note = |what: String| {
        println!("{what}");
        rows.insert(n, what);
        n += 1;
    };
    for poll in 0..args.polls {
        let (took, status, bytes) = server.get("/queue/depth", &session, true);
        note(format!(
            "badge   poll {poll}: {status} {bytes:>7} B  wall {:>8.1} ms  {}",
            took.as_secs_f64() * 1000.0,
            server
                .last_dur(ikigai_gonk::queue::BADGE_IRI)
                .unwrap_or_default()
        ));
    }
    for page in &args.pages {
        for round in 0..2 {
            let (took, status, bytes) = server.get(page, &session, page != "/queue");
            note(format!(
                "page    {page} #{round}: {status} {bytes:>7} B  wall {:>8.1} ms",
                took.as_secs_f64() * 1000.0
            ));
        }
    }
    // ★ The breakdown's first column: each root's pending findings, read alone.
    let roots: Vec<String> = config_toml
        .lines()
        .filter_map(|line| {
            line.split_once('"')
                .and_then(|(_, rest)| rest.split_once('='))
        })
        .map(|(name, _)| name.to_string())
        .collect();
    let (mut total, mut total_rows, mut total_bytes) = (Duration::ZERO, 0, 0);
    for root in roots.iter().filter(|_| args.probe) {
        let (took, rows, bytes) = server.findings(root);
        let (again, _, _) = server.findings(root);
        total += took;
        total_rows += rows;
        total_bytes += bytes;
        note(format!(
            "findings {root:<20} {rows:>5} rows {bytes:>9} B  {:>8.1} ms  (again {:>7.1} ms)",
            took.as_secs_f64() * 1000.0,
            again.as_secs_f64() * 1000.0
        ));
    }
    if args.probe {
        note(format!(
            "findings TOTAL {total_rows} rows {total_bytes} B in {:.1} s over {} roots",
            total.as_secs_f64(),
            roots.len()
        ));
    }
    for poll in 0..args.polls {
        let (took, status, bytes) = server.get("/queue/depth", &session, true);
        note(format!(
            "badge   after-page poll {poll}: {status} {bytes:>7} B  wall {:>8.1} ms",
            took.as_secs_f64() * 1000.0
        ));
    }
    // ★ Steady state under whatever the machine is doing: a browser's poll every ten seconds
    // for `--hold` seconds, against the REAL roots other sessions are editing.
    let until = Instant::now() + Duration::from_secs(args.hold);
    let mut polls: Vec<f64> = Vec::new();
    while Instant::now() < until {
        let (took, _, _) = server.get("/queue/depth", &session, true);
        polls.push(took.as_secs_f64() * 1000.0);
        std::thread::sleep(Duration::from_secs(10).saturating_sub(took));
    }
    if !polls.is_empty() {
        let mut sorted = polls.clone();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize];
        note(format!(
            "held    {} polls: p50 {:.1} ms  p90 {:.1} ms  max {:.1} ms  (over 1 s: {})",
            polls.len(),
            at(0.5),
            at(0.9),
            at(1.0),
            polls.iter().filter(|ms| **ms > 1000.0).count()
        ));
        let (took, status, bytes) = server.get("/queue", &session, false);
        note(format!(
            "page    /queue after the hold: {status} {bytes:>7} B  wall {:>8.1} ms",
            took.as_secs_f64() * 1000.0
        ));
    }
}
