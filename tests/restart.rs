//! The process, restarted: what one run leaves in a config home, and whether the next run
//! still serves.
//!
//! ★ These run the real binary. gonk #2's tests drove the HTTP face in-process and never
//! started a second server after enrolling a passkey — which is how a passkey-only
//! `clients.json` shipped making the next start refuse (gonk PENDING §3).
//!
//! Every run is hermetic: `HOME` and `XDG_CONFIG_HOME` point into a scratch directory (so the
//! store, the config home and the socket are all scratch), HTTP binds an ephemeral loopback
//! port, the socket is a short RELATIVE path (a Unix socket path is capped at 104 bytes and a
//! temp directory eats most of that), and any QUIC bind is `127.0.0.1:0`.
//!
//! - [`a_passkey_enrolled_over_http_survives_a_restart`]
//! - [`the_refusals_that_are_right_still_stop_the_server`]
//! - [`a_certificate_client_beside_a_passkey_opens_both_doors`]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use common::Authenticator;

const PATIENCE: Duration = Duration::from_secs(60);

struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Scratch {
        Scratch {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// `<config home>/gonk`.
    fn gonk_dir(&self) -> PathBuf {
        self.dir.path().join("c").join("ikigai").join("gonk")
    }

    fn server_cert(&self) -> PathBuf {
        self.gonk_dir().join("quic").join("server.crt")
    }

    fn write(&self, name: &str, contents: &str) {
        std::fs::create_dir_all(self.gonk_dir()).unwrap();
        std::fs::write(self.gonk_dir().join(name), contents).unwrap();
    }

    fn command(&self) -> Command {
        let home: &Path = self.dir.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"));
        command
            .current_dir(home)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("c"))
            .stdin(Stdio::null());
        command
    }

    /// Run a subcommand to completion; its stdout.
    fn run(&self, args: &[&str]) -> String {
        let out = self.command().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "ikigai-gonk {args:?}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// `passkey invite brian`; the invite code.
    fn invite(&self) -> String {
        let printed = self.run(&[
            "passkey",
            "invite",
            "brian",
            "--ledger",
            "default=delete",
            "--port",
            "1",
        ]);
        let at = printed
            .find("#invite=")
            .unwrap_or_else(|| panic!("no invite link in: {printed}"));
        printed[at + "#invite=".len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect()
    }

    fn serve_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec!["--port", "0", "--socket", "g.sock"];
        args.extend_from_slice(extra);
        args
    }

    /// Start the server and wait for its banner.
    fn serve(&self, extra: &[&str]) -> Running {
        let mut child = self
            .command()
            .args(Self::serve_args(extra))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = lines_of(child.stderr.take().unwrap());
        let mut banner = String::new();
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match lines.recv_timeout(left) {
                Ok(line) => {
                    banner.push_str(&line);
                    banner.push('\n');
                    if line.trim_start().starts_with("mount") {
                        break;
                    }
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("the server printed no banner:\n{banner}");
                }
            }
        }
        let at =
            banner.find("http://localhost:").expect("an http line") + "http://localhost:".len();
        let port = banner[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap();
        Running {
            child,
            port,
            banner,
            _lines: lines,
        }
    }

    /// Start the server expecting it to REFUSE; its stderr.
    fn refused(&self, extra: &[&str]) -> String {
        let mut child = self
            .command()
            .args(Self::serve_args(extra))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = lines_of(child.stderr.take().unwrap());
        let deadline = Instant::now() + PATIENCE;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                let said: Vec<String> = lines.try_iter().collect();
                panic!(
                    "expected a refusal, but it kept running:\n{}",
                    said.join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let said: Vec<String> = lines.iter().collect();
        let said = said.join("\n");
        assert!(
            !status.success(),
            "expected a refusal, got {status}:\n{said}"
        );
        said
    }
}

fn lines_of(stream: impl Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// A server process, killed on drop.
struct Running {
    child: Child,
    port: u16,
    banner: String,
    /// Held so the server's later stderr writes have somewhere to go.
    _lines: Receiver<String>,
}

impl Running {
    fn origin(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String) {
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
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        (status, body.to_string())
    }

    /// A same-origin JSON post, as gonk.js makes one.
    fn json(&self, path: &str, body: &str) -> (u16, String) {
        self.request(
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

    fn challenge(&self, op: &str) -> String {
        let (status, body) = self.json(&format!("/auth/{op}"), "{}");
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["challenge"].as_str().unwrap().to_string()
    }

    /// It serves: a browser gets the front page, and the process is still alive after.
    fn assert_serving(&mut self) {
        let (status, body) = self.request(
            "GET",
            "/",
            &[
                (
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string(),
                ),
                ("Sec-Fetch-Site", "none".to_string()),
            ],
            "",
        );
        assert_eq!(status, 200, "{body}");
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "the server exited after its banner:\n{}",
            self.banner
        );
    }

    fn quic_line(&self) -> &str {
        self.banner
            .lines()
            .find(|line| line.trim_start().starts_with("quic"))
            .expect("a quic line")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Invite, start, enrol the software passkey through `POST /auth/register`, stop.
fn enrol_a_passkey(scratch: &Scratch) {
    let invite = scratch.invite();
    let mut first = scratch.serve(&[]);
    first.assert_serving();
    let c = first.challenge("register-options");
    let (status, body) = first.json(
        "/auth/register",
        &Authenticator::new().register_body(&c, &invite, &first.origin()),
    );
    assert_eq!(status, 200, "{body}");
}

#[test]
fn a_passkey_enrolled_over_http_survives_a_restart() {
    let scratch = Scratch::new();
    enrol_a_passkey(&scratch);
    let clients = std::fs::read_to_string(scratch.gonk_dir().join("clients.json")).unwrap();
    assert!(clients.contains("\"passkeys\""), "{clients}");
    assert!(!clients.contains("\"clients\""), "{clients}");

    // ★ The restart that used to exit 1.
    let mut second = scratch.serve(&[]);
    second.assert_serving();
    assert!(second.banner.contains("1 passkey(s)"), "{}", second.banner);
    let quic = second.quic_line();
    assert!(
        quic.contains("off") && quic.contains("enrols no client certificate"),
        "the banner says why QUIC is off: {quic}"
    );
    assert!(
        quic.contains("ikigai-gonk client add"),
        "and what opens it: {quic}"
    );
    assert!(
        !scratch.server_cert().exists(),
        "no server identity is generated for a door that does not open"
    );

    // And the passkey still signs in, at the new port's origin.
    let c = second.challenge("login-options");
    let (status, body) = second.json(
        "/auth/login",
        &Authenticator::new().login_body(&c, &second.origin(), 1),
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let (status, who) = second.json("/auth/session", v["session"].as_str().unwrap());
    assert_eq!(status, 200, "{who}");
    assert!(who.contains("\"grant\":\"brian\""), "{who}");
}

#[test]
fn the_refusals_that_are_right_still_stop_the_server() {
    // A named QUIC bind with no certificate enrolled: the operator expects a door.
    let scratch = Scratch::new();
    scratch.write("clients.json", "{\"passkeys\": {}}");
    let said = scratch.refused(&["--quic-bind", "127.0.0.1:0"]);
    assert!(
        said.contains("QUIC door was asked for") && said.contains("ikigai-gonk client add"),
        "{said}"
    );
    assert!(
        !scratch.server_cert().exists(),
        "refused before generating anything"
    );

    // The same with no clients.json at all.
    let bare = Scratch::new();
    let said = bare.refused(&["--quic-bind", "127.0.0.1:0"]);
    assert!(said.contains("no client certificate is enrolled"), "{said}");

    // A broad store token in grants.json stops the server even with QUIC not opening.
    let broad = Scratch::new();
    broad.write("clients.json", "{\"passkeys\": {}}");
    broad.write("grants.json", "{\"everything\": [\"urn:cap:store:read\"]}");
    let said = broad.refused(&[]);
    assert!(said.contains("urn:cap:store:read"), "{said}");
}

#[test]
fn a_certificate_client_beside_a_passkey_opens_both_doors() {
    let scratch = Scratch::new();
    scratch.run(&["client", "add", "laptop", "--ledger", "default=write"]);
    enrol_a_passkey(&scratch);
    let clients = std::fs::read_to_string(scratch.gonk_dir().join("clients.json")).unwrap();
    assert!(
        clients.contains("\"clients\"") && clients.contains("\"passkeys\""),
        "{clients}"
    );

    let mut both = scratch.serve(&["--quic-bind", "127.0.0.1:0"]);
    both.assert_serving();
    assert!(both.banner.contains("1 passkey(s)"), "{}", both.banner);
    let quic = both.quic_line();
    assert!(
        quic.contains("udp 127.0.0.1:0") && quic.contains("1 trusted certificate(s), 1 enrolled"),
        "{quic}"
    );
}
