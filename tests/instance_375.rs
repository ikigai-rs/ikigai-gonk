//! Ledger #375: a page can tell that the server behind it has moved.
//!
//! The version in the banner did not change across three merged arcs, so a page comparing
//! versions would have said "same server" throughout the episode this is for. What changes is
//! the PROCESS: each start draws a fresh instance id, every full page carries it in
//! `<meta name="gonk-instance">`, and `GET /instance` answers it, so `web/gonk.js` can compare
//! the two and say the page is out of date.
//!
//! These start the real binary twice, because "a restart is a new id" is a fact about
//! processes, which no in-process test can show.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    host: String,
    banner: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start gonk on a fresh home, and wait for its banner's `mount` line.
fn start(home: &std::path::Path) -> Server {
    let log = home.join("gonk.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .unwrap();
    let start_len = std::fs::read_to_string(&log).unwrap_or_default().len();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("c"))
        .args(["--port", "0", "--socket", "g.sock"])
        .stdin(Stdio::null())
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let banner = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default()[start_len..].to_string();
        if text.lines().any(|l| l.trim_start().starts_with("mount")) {
            break text;
        }
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            panic!("the server printed no banner:\n{text}");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let host = banner
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("http "))
        .and_then(|rest| rest.trim_start().strip_prefix("http://"))
        .and_then(|rest| rest.split(['/', ' ']).next())
        .unwrap_or_else(|| panic!("no http line in the banner:\n{banner}"))
        .to_string();
    Server {
        child,
        host,
        banner,
    }
}

/// A GET with the `Host` the banner names: `(status, body)`.
fn get(server: &Server, path: &str) -> (u16, String) {
    let port = server.host.rsplit(':').next().unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        server.host
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status = raw
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_string()
    };
    (status, body)
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&rest[..n]);
        body = &rest[n + 2..];
    }
    out
}

/// The `content` of the page's `<meta name='gonk-instance'>`.
fn meta_instance(page: &str) -> Option<String> {
    let at = page.find("name='gonk-instance'")?;
    let tag = &page[page[..at].rfind('<')?..];
    let tag = &tag[..tag.find('>')?];
    let (_, rest) = tag.split_once("content='")?;
    Some(rest.split('\'').next()?.to_string())
}

/// What one run of the server says its instance is, three ways that must agree.
fn instance_of(server: &Server) -> String {
    let (status, id) = get(server, "/instance");
    assert_eq!(status, 200, "GET /instance answers: {id}");
    let id = id.trim().to_string();
    assert!(
        id.len() >= 16 && id.chars().all(|c| c.is_ascii_hexdigit()),
        "an instance id is random hex, not a version: {id:?}"
    );
    assert_ne!(id, env!("CARGO_PKG_VERSION"));
    let (status, page) = get(server, "/");
    assert_eq!(status, 200, "the home page: {page}");
    assert_eq!(
        meta_instance(&page).as_deref(),
        Some(id.as_str()),
        "the page carries the id /instance answers"
    );
    assert!(
        server.banner.contains(&format!("instance {id}")),
        "the banner names the instance:\n{}",
        server.banner
    );
    id
}

/// The notice is on every page, hidden until `web/gonk.js` finds the server moved, and the script
/// asks the route the page names.
#[test]
fn the_page_holds_the_hidden_notice_and_the_script_asks_for_the_instance() {
    let home = tempfile::tempdir().unwrap();
    let server = start(home.path());
    let (_, page) = get(&server, "/sparql");
    let notice = page
        .find("id='moved'")
        .map(|at| &page[page[..at].rfind('<').unwrap()..])
        .map(|tag| &tag[..tag.find('>').unwrap()])
        .expect("the page has the notice");
    assert!(notice.contains("hidden"), "hidden with no script: {notice}");
    assert!(page.contains("gonk has restarted since this page loaded"));
    assert!(
        meta_instance(&page).is_some(),
        "every full page carries the id"
    );
    let (status, script) = get(&server, "/static/gonk.js");
    assert_eq!(status, 200);
    assert!(
        script.contains("fetch(\"/instance\""),
        "gonk.js polls /instance"
    );
    assert!(script.contains("meta[name='gonk-instance']"));
}

#[test]
fn a_restarted_server_answers_a_different_instance_with_the_same_version() {
    let home = tempfile::tempdir().unwrap();
    let first = start(home.path());
    let before = instance_of(&first);
    assert_eq!(
        instance_of(&first),
        before,
        "one process keeps one id between reads"
    );
    drop(first);

    // The same binary, the same version, the same store: only the process is new.
    let second = start(home.path());
    let after = instance_of(&second);
    assert_ne!(
        before, after,
        "a restart is a new instance even though nothing else changed"
    );
}
