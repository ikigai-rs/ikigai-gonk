//! A scratch gonk in a CHILD process: the binary this crate builds, over a scratch config home,
//! data home, store and socket, shared by every test that runs one (ledger
//! [#1004](http://localhost:1060/l/default/item/1004),
//! [#1028](http://localhost:1060/l/default/item/1028)). Nothing here touches a live gonk.
//!
//! Two rules, each learned from a flake:
//!
//! - **No port is picked here and handed over, except QUIC's.** The child binds HTTP port 0 and
//!   the banner names the port it got. Picking a free port and releasing it raced every other
//!   socket on the machine for the time the child takes to open its store: `AddrInUse`, 43 of
//!   160 runs of `tests/sparql_depth_915.rs` at 8 concurrent copies.
//! - **Ready means every door ANSWERS**, never that a file exists: the banner for HTTP, a
//!   connection that completes the hello on the socket, and a QUIC handshake with this test's
//!   own client when that door is open. The socket's file appearing is not the socket
//!   listening, and a connect in between was refused (29 of 80 runs at 8 copies).
//!
//! Each test binary compiles this module WHOLE and uses part of it, so an item one binary does
//! not call is normal here rather than dead (the same reason `tests/common/mod.rs` gives).

use std::fs::File;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::quic;

/// A scratch gonk in a child process. Killed and reaped on drop.
pub struct Gonk {
    pub child: Child,
    /// Read from the banner: the child bound port 0.
    pub http: SocketAddr,
    /// `None` when the child was started with `--no-quic` ([`Gonk::start`]).
    pub quic: Option<SocketAddr>,
    pub socket: PathBuf,
    pub stderr: PathBuf,
    pub layout: quic::Layout,
    /// One QUIC client, `alpha`, enrolled to write the default ledger.
    pub client: quic::Bundle,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

/// What [`Gonk::launch`] says when the child did not come up.
enum Launch {
    /// The child died because its QUIC port was taken: worth another pick.
    PortTaken(String),
    /// Anything else, which a retry would only hide.
    Failed(String),
}

#[allow(dead_code)] // see the module docs: not every binary calls every constructor
impl Gonk {
    /// A gonk with its QUIC door OFF: HTTP and the socket only, so nothing picks a port.
    /// `config` is the scratch home's whole `config.toml` (empty for the defaults).
    pub fn start(config: &str) -> Gonk {
        Gonk::launch(config, false, None).unwrap_or_else(|launch| match launch {
            Launch::PortTaken(why) | Launch::Failed(why) => panic!("{why}"),
        })
    }

    /// A gonk whose HTTP door binds where `config` says (`gonk.bind = "<ip>:0"`), not where
    /// `--port 0` would put it: no port flag, so the config home's own key decides, and `ip`
    /// is the address the test then dials (ledger
    /// [#1045](http://localhost:1060/l/default/item/1045)). The QUIC door is off.
    pub fn start_bound(config: &str, ip: IpAddr) -> Gonk {
        Gonk::launch(config, false, Some(ip)).unwrap_or_else(|launch| match launch {
            Launch::PortTaken(why) | Launch::Failed(why) => panic!("{why}"),
        })
    }

    /// A gonk with its QUIC door open on loopback, for the tests that send over it.
    ///
    /// ⚠ **The one port still picked, and why it is retried rather than avoided** (ledger
    /// #1004). `ikigai_quic::serve` binds inside the transport crate and never reports its
    /// address (ledger [#1027](http://localhost:1060/l/default/item/1027)), so a child told
    /// `127.0.0.1:0` would listen where nobody can find it. So the port is picked here, and a
    /// pick another process takes before the child binds it (`AddrInUse`) is detected by the
    /// child's death and its log, and picked again. Every other failure still fails.
    pub fn with_quic(config: &str) -> Gonk {
        let mut taken = Vec::new();
        for _ in 0..5 {
            match Gonk::launch(config, true, None) {
                Ok(gonk) => return gonk,
                Err(Launch::PortTaken(why)) => taken.push(why),
                Err(Launch::Failed(why)) => panic!("{why}"),
            }
        }
        panic!(
            "five QUIC ports were taken before the child bound them:\n{}",
            taken.join("\n")
        )
    }

    fn launch(config_toml: &str, with_quic: bool, bound: Option<IpAddr>) -> Result<Gonk, Launch> {
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

        let quic: Option<SocketAddr> = with_quic.then(|| {
            UdpSocket::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
        });
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
        match quic {
            Some(addr) => command.args(["--quic-bind", &addr.to_string()]),
            None => command.arg("--no-quic"),
        };
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
            quic,
            socket,
            stderr,
            layout,
            client,
            _dirs: (home, short),
        };
        gonk.until_ready()?;
        Ok(gonk)
    }

    /// Wait until every door this child opened ANSWERS — not until its files exist.
    fn until_ready(&mut self) -> Result<(), Launch> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                let log = self.log();
                let why = format!("gonk exited while starting ({status}):\n{log}");
                return Err(
                    if log.contains("QUIC door") && log.contains("Address already in use") {
                        Launch::PortTaken(why)
                    } else {
                        Launch::Failed(why)
                    },
                );
            }
            if Instant::now() > deadline {
                return Err(Launch::Failed(format!(
                    "gonk never came up:\n{}",
                    self.log()
                )));
            }
            if self.http.port() == 0 {
                if let Some(port) = banner_port(&self.log()) {
                    self.http.set_port(port);
                }
            }
            if self.http.port() != 0
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
        let identity = ikigai_quic::Identity {
            cert_pem: std::fs::read_to_string(self.client.dir.join("client.crt")).unwrap(),
            key_pem: std::fs::read_to_string(self.client.dir.join("client.key")).unwrap(),
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

impl Drop for Gonk {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
