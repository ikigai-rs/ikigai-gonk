//! A scratch gonk in a CHILD process: the binary this crate builds, over a scratch config home,
//! data home, store and socket, shared by every test that runs one (ledger
//! [#1004](http://localhost:1060/l/default/item/1004),
//! [#1028](http://localhost:1060/l/default/item/1028)). Nothing here touches a live gonk.
//!
//! Two rules, each learned from a flake:
//!
//! - **No port is picked here and handed over.** The child binds HTTP port 0 and QUIC port 0,
//!   and the banner names the ports it got. Picking a free port and releasing it raced every
//!   other socket on the machine for the time the child takes to open its store: `AddrInUse`, 43
//!   of 160 runs of `tests/sparql_depth_915.rs` at 8 concurrent copies. QUIC was the one
//!   exception until ledger [#1300](http://localhost:1060/l/default/item/1300): the door bound
//!   inside `ikigai_quic::serve` and never said where, so its port was picked here and a taken
//!   pick retried. The door now binds before the banner, and nothing is retried.
//! - **Ready means every door ANSWERS**, never that a file exists: the banner for HTTP, a
//!   connection that completes the hello on the socket, and a QUIC handshake with this test's
//!   own client when that door is open. The socket's file appearing is not the socket
//!   listening, and a connect in between was refused (29 of 80 runs at 8 copies).
//!
//! Each test binary compiles this module WHOLE and uses part of it, so an item one binary does
//! not call is normal here rather than dead (the same reason `tests/common/mod.rs` gives).

use std::fs::File;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::quic;

/// A scratch gonk in a child process. Killed and reaped on drop.
pub struct Gonk {
    pub child: Child,
    /// Read from the banner: the child bound port 0.
    pub http: SocketAddr,
    /// Read from the banner: the child bound QUIC port 0. `None` when it was started with
    /// `--no-quic` ([`Gonk::start`]).
    pub quic: Option<SocketAddr>,
    pub socket: PathBuf,
    pub stderr: PathBuf,
    pub layout: quic::Layout,
    /// One QUIC client, `alpha`, enrolled to write the default ledger.
    pub client: quic::Bundle,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

#[allow(dead_code)] // see the module docs: not every binary calls every constructor
impl Gonk {
    /// A gonk with its QUIC door OFF: HTTP and the socket only, so nothing picks a port.
    /// `config` is the scratch home's whole `config.toml` (empty for the defaults).
    pub fn start(config: &str) -> Gonk {
        Gonk::launch(config, false, None, &|_, _| {})
    }

    /// A gonk whose HTTP door binds where `config` says (`gonk.bind = "<ip>:0"`), not where
    /// `--port 0` would put it: no port flag, so the config home's own key decides, and `ip`
    /// is the address the test then dials (ledger
    /// [#1045](http://localhost:1060/l/default/item/1045)). The QUIC door is off.
    pub fn start_bound(config: &str, ip: IpAddr) -> Gonk {
        Gonk::launch(config, false, Some(ip), &|_, _| {})
    }

    /// A gonk with its QUIC door open on loopback port 0, for the tests that send over it; the
    /// banner's `quic` line names the port it bound.
    pub fn with_quic(config: &str) -> Gonk {
        Gonk::with_quic_prepared(config, &|_, _| {})
    }

    /// [`Gonk::with_quic`], with `prepare(config_home, home)` run after `alpha` is enrolled and
    /// BEFORE the child starts — for a test that enrolls more clients (with the binary's own
    /// `client add`, say): the set of TLS-trusted certificates is read at startup, so a client
    /// added after it would never complete a handshake.
    pub fn with_quic_prepared(config: &str, prepare: &dyn Fn(&Path, &Path)) -> Gonk {
        Gonk::launch(config, true, None, prepare)
    }

    fn launch(
        config_toml: &str,
        with_quic: bool,
        bound: Option<IpAddr>,
        prepare: &dyn Fn(&Path, &Path),
    ) -> Gonk {
        let home = tempfile::tempdir().unwrap();
        // A Unix socket path must fit `sun_path` (104 bytes on macOS); a scratch dir does not.
        let short = tempfile::Builder::new()
            .prefix("gk")
            .tempdir_in("/tmp")
            .unwrap();
        let config = home.path().join("config");
        let data = home.path().join("data");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        if !config_toml.is_empty() {
            std::fs::write(config.join("config.toml"), config_toml).unwrap();
        }

        // One QUIC client, enrolled for the default ledger — what `client add` writes.
        let layout = quic::Layout::in_config_home(&config);
        quic::server_identity(&layout).unwrap();
        let client = quic::add_client(&layout, "alpha", None, false).unwrap();
        quic::enrol(
            &layout,
            "alpha",
            &client.fingerprint,
            &grants_for("default", Authority::Write).unwrap(),
            false,
        )
        .unwrap();
        prepare(&config, home.path());

        let socket = short.path().join("s");
        let stderr = home.path().join("stderr.log");
        let mut command = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"));
        command
            .arg("--config-home")
            .arg(&config)
            .arg("--data-home")
            .arg(&data)
            .arg("--socket")
            .arg(&socket)
            .arg("--no-backup");
        // ★ Port 0: the child binds whatever is free and the banner names it — by flag, unless
        // the config's own `gonk.bind` is what is under test (a flag would override it).
        if bound.is_none() {
            command.args(["--port", "0"]);
        }
        if with_quic {
            command.args(["--quic-bind", "127.0.0.1:0"]);
        } else {
            command.arg("--no-quic");
        }
        let child = command
            .env("HOME", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .expect("spawn ikigai-gonk");
        let mut gonk = Gonk {
            child,
            // The port is unknown until the banner says; nothing reads it before then.
            http: SocketAddr::new(bound.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)), 0),
            // Likewise until the banner's `quic` line names it.
            quic: with_quic.then(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)),
            socket,
            stderr,
            layout,
            client,
            _dirs: (home, short),
        };
        if let Err(why) = gonk.until_ready() {
            panic!("{why}");
        }
        gonk
    }

    /// Wait until every door this child opened ANSWERS — not until its files exist.
    fn until_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Err(format!(
                    "gonk exited while starting ({status}):\n{}",
                    self.log()
                ));
            }
            if Instant::now() > deadline {
                return Err(format!("gonk never came up:\n{}", self.log()));
            }
            if self.http.port() == 0 {
                if let Some(port) = banner_port(&self.log()) {
                    self.http.set_port(port);
                }
            }
            if self.quic.is_some_and(|quic| quic.port() == 0) {
                if let (Some(port), Some(quic)) = (banner_quic_port(&self.log()), &mut self.quic) {
                    quic.set_port(port);
                }
            }
            if self.http.port() != 0
                && self.quic.is_none_or(|quic| quic.port() != 0)
                && ikigai_ipc::connect(&self.socket).is_ok()
                && std::net::TcpStream::connect(self.http).is_ok()
                && (self.quic.is_none() || self.dial_quic().is_ok())
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The child's stderr so far: the banner, and how it died if it did.
    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// One QUIC connection as this test's enrolled client. Only for a child with the door open.
    pub fn dial_quic(&self) -> std::io::Result<ikigai_quic::QuicResolver> {
        self.dial_quic_as(&self.client.dir)
    }

    /// One QUIC connection as the client whose bundle is `bundle` (`gonk/quic/clients/<name>`).
    pub fn dial_quic_as(&self, bundle: &Path) -> std::io::Result<ikigai_quic::QuicResolver> {
        let identity = ikigai_quic::Identity {
            cert_pem: std::fs::read_to_string(bundle.join("client.crt")).unwrap(),
            key_pem: std::fs::read_to_string(bundle.join("client.key")).unwrap(),
        };
        let (server, _) = quic::server_identity(&self.layout).unwrap();
        let addr = self.quic.expect("a gonk started with_quic");
        ikigai_quic::connect(addr, &identity, &server.cert_pem)
    }
}

/// The port the banner's `http` line names: `  http    http://{host}:{port}/ — …`, where the
/// host is `localhost` for a `127.0.0.1` bind and the bound IP otherwise (`doors::http_url`).
fn banner_port(log: &str) -> Option<u16> {
    let line = log.lines().find(|l| l.trim_start().starts_with("http "))?;
    let at = line.find("http://")? + "http://".len();
    let authority = line[at..].split('/').next()?;
    authority.rsplit_once(':')?.1.parse().ok()
}

/// The port the banner's `quic` line names: `  quic    udp 127.0.0.1:{port} — …`, the port the
/// door BOUND (ledger [#1300](http://localhost:1060/l/default/item/1300)).
fn banner_quic_port(log: &str) -> Option<u16> {
    let line = log.lines().find(|l| l.trim_start().starts_with("quic "))?;
    let at = line.find("udp ")? + "udp ".len();
    let addr: SocketAddr = line[at..].split_whitespace().next()?.parse().ok()?;
    Some(addr.port())
}

impl Drop for Gonk {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
