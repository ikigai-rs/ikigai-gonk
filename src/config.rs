//! Flags and the config home — never environment variables.
//!
//! Settings live in the machine's shared `config.toml` (`$XDG_CONFIG_HOME/ikigai`, else
//! `~/.config/ikigai`), under keys this server owns by prefix, in the same `key = "value"`
//! line grammar every ikigai process reads. The store's directory is NOT a key here: it is
//! `ikigai-store`'s own layered `store.toml` (with `gonk.store.toml` beside it), so one
//! setting has one spelling.
//!
//! ```toml
//! gonk.bind = "127.0.0.1:1060"          # the HTTP door (loopback only; this IS the default)
//! # gonk.port = 1060                    # shorthand for gonk.bind = "127.0.0.1:<port>"
//! gonk.socket = "~/.ikigai/gonk.sock"   # the owner-only socket (this IS the default)
//! gonk.quic.bind = "0.0.0.0:1060"       # the QUIC door, when a client is enrolled (default)
//! gonk.http.ledger = "default"          # ledgers the HTTP door may read and write; repeatable
//! ```
//!
//! **Flags override config wholesale**, the rule standalone `ikigai-web` set: `--port 9000`
//! against `gonk.bind = "127.0.0.1:1070"` serves loopback 9000, and a repeatable flag
//! replaces every line of its key rather than adding to them. A key under `gonk.` that this
//! server does not know is an error, not a no-op — a misspelled setting that silently did
//! nothing would look exactly like one that is in effect.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::grants::Authority;

/// The HTTP door's default port — and the QUIC door's, on UDP. See the README for 1060.
pub const DEFAULT_PORT: u16 = 1060;

/// The ledger the HTTP door serves when no `gonk.http.ledger` line names one.
pub const DEFAULT_LEDGER: &str = "default";

/// The socket's file name under the data home.
pub const SOCKET_NAME: &str = "gonk.sock";

/// The longest Unix socket path accepted, exclusive: `sun_path` on macOS (Linux allows 108;
/// the smaller bound is taken so a config that works on one works on both).
pub const SOCKET_PATH_LIMIT: usize = 104;

/// Every key this server reads.
const KEYS: [&str; 5] = [
    "gonk.bind",
    "gonk.port",
    "gonk.socket",
    "gonk.quic.bind",
    "gonk.http.ledger",
];

/// How to invoke the binary.
pub const USAGE: &str = "\
ikigai-gonk — a standalone ikigai work-ledger server

usage:
  ikigai-gonk [serve] [flags]      hold the durable store and serve the ledgers
  ikigai-gonk client add <name> [--ledger <ledger>=<read|write|delete|purge>]... [--cert <client.crt>] [--force]
                                   trust a QUIC client: mint its identity (or import the
                                   certificate it generated with --cert) into a bundle, and
                                   with --ledger enrol its fingerprint under a grant
  ikigai-gonk grants <ledger> [read|write|delete|purge]
                                   print the capability tokens for one ledger (JSON)

serve flags (each overrides its config key wholesale):
  --bind IP:PORT        the HTTP door (config `gonk.bind`); loopback only — default 127.0.0.1:1060
  --port N              shorthand for --bind 127.0.0.1:N (config `gonk.port`)
  --socket PATH         the owner-only socket (config `gonk.socket`); default ~/.ikigai/gonk.sock
  --quic-bind IP:PORT   the QUIC door (config `gonk.quic.bind`); default 0.0.0.0:1060 (UDP)
  --no-quic             do not open the QUIC door this run
  --http-ledger NAME    a ledger the HTTP door may read and write (repeatable; config
                        `gonk.http.ledger`); default `default`
  --config PATH         read this file instead of <config home>/config.toml

files (in the config home, ~/.config/ikigai unless XDG_CONFIG_HOME says otherwise):
  config.toml               the gonk.* keys above
  store.toml, gonk.store.toml   where the dataset lives (default ~/.ikigai/store)
  gonk/clients.json         certificate fingerprint -> grant name (the QUIC door opens when it exists)
  gonk/grants.json          grant name -> capability scopes
  gonk/quic/                server identity (generated on first use) and clients/<name>/ bundles
";

/// A parsed command line.
#[derive(Debug)]
pub enum Command {
    /// Serve, with these flags.
    Serve(Flags),
    /// Trust a QUIC client, and optionally enrol it.
    ClientAdd {
        /// The bundle's name.
        name: String,
        /// A certificate the client generated itself, to import instead of minting.
        cert: Option<PathBuf>,
        /// Ledgers and authorities to enrol the client under.
        ledgers: Vec<(String, Authority)>,
        /// Replace an existing bundle or enrolment.
        force: bool,
    },
    /// Print the tokens for one ledger.
    Grants {
        /// The ledger.
        ledger: String,
        /// The authority.
        authority: Authority,
    },
    /// Print the usage.
    Help,
}

/// The serve flags, unmerged.
#[derive(Debug, Default)]
pub struct Flags {
    /// `--bind`.
    pub bind: Option<String>,
    /// `--port`.
    pub port: Option<u16>,
    /// `--config`.
    pub config: Option<PathBuf>,
    /// `--socket`.
    pub socket: Option<String>,
    /// `--quic-bind`.
    pub quic_bind: Option<String>,
    /// `--no-quic`.
    pub no_quic: bool,
    /// `--http-ledger`, in order.
    pub http_ledgers: Vec<String>,
}

/// Everything `serve` needs, merged and validated.
#[derive(Debug)]
pub struct Settings {
    /// The HTTP door's address. Always loopback — see [`refuse_non_loopback`].
    pub http: SocketAddr,
    /// The socket path.
    pub socket: PathBuf,
    /// The QUIC door's address, or `None` under `--no-quic`. Whether it actually opens also
    /// depends on an enrolment existing; that is `main`'s decision.
    pub quic: Option<SocketAddr>,
    /// The ledgers the HTTP door may read and write.
    pub http_ledgers: Vec<String>,
}

/// The two ikigai homes, and `$HOME` for `~/` expansion.
#[derive(Debug, Clone)]
pub struct Homes {
    /// `$HOME`.
    pub home: PathBuf,
    /// The config home (`$XDG_CONFIG_HOME/ikigai`, else `~/.config/ikigai`).
    pub config: PathBuf,
    /// The data home (`~/.ikigai`).
    pub data: PathBuf,
}

impl Homes {
    /// The process's real homes, from `ikigai_core::config` — the one place the ecosystem
    /// spells them. Fails rather than guessing a working-directory-relative path.
    pub fn from_process() -> Result<Homes, String> {
        let config = ikigai_core::config::config_home()
            .ok_or("no ikigai config home: neither XDG_CONFIG_HOME nor HOME is set")?;
        let data =
            ikigai_core::config::data_home().ok_or("no ikigai data home: HOME is not set")?;
        let home = data
            .parent()
            .map(Path::to_path_buf)
            .ok_or("the data home has no parent directory")?;
        Ok(Homes { home, config, data })
    }
}

/// Parse the argument vector (without the program name).
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    match args.peek().map(String::as_str) {
        Some("client") => {
            args.next();
            return parse_client(args);
        }
        Some("grants") => {
            args.next();
            let ledger = args
                .next()
                .ok_or("grants: expected <ledger> [read|write|delete|purge]")?;
            let authority = match args.next() {
                Some(value) => value.parse()?,
                None => Authority::Write,
            };
            if let Some(extra) = args.next() {
                return Err(format!("grants: unexpected argument `{extra}`"));
            }
            return Ok(Command::Grants { ledger, authority });
        }
        Some("serve") => {
            args.next();
        }
        _ => {}
    }
    let mut flags = Flags::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" | "help" => return Ok(Command::Help),
            "--bind" => flags.bind = Some(value(&mut args, "--bind")?),
            "--port" => {
                let port = value(&mut args, "--port")?;
                flags.port = Some(
                    port.parse()
                        .map_err(|_| format!("--port: `{port}` is not a port number"))?,
                );
            }
            "--config" => flags.config = Some(PathBuf::from(value(&mut args, "--config")?)),
            "--socket" => flags.socket = Some(value(&mut args, "--socket")?),
            "--quic-bind" => flags.quic_bind = Some(value(&mut args, "--quic-bind")?),
            "--no-quic" => flags.no_quic = true,
            "--http-ledger" => flags.http_ledgers.push(value(&mut args, "--http-ledger")?),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Command::Serve(flags))
}

fn parse_client(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    match args.next().as_deref() {
        Some("add") => {}
        Some(other) => {
            return Err(format!(
                "client: unknown subcommand `{other}` (expected `add`)"
            ))
        }
        None => return Err("client: expected `add <name>`".to_string()),
    }
    let name = args
        .next()
        .filter(|name| !name.starts_with('-'))
        .ok_or("client add: expected <name>")?;
    let (mut cert, mut ledgers, mut force) = (None, Vec::new(), false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cert" => cert = Some(PathBuf::from(value(&mut args, "--cert")?)),
            "--ledger" => {
                let spec = value(&mut args, "--ledger")?;
                let (ledger, authority) = spec.split_once('=').ok_or_else(|| {
                    format!("--ledger: expected <ledger>=<read|write|delete|purge>, got `{spec}`")
                })?;
                ledgers.push((ledger.to_string(), authority.parse()?));
            }
            "--force" => force = true,
            other => return Err(format!("client add: unknown argument `{other}`")),
        }
    }
    Ok(Command::ClientAdd {
        name,
        cert,
        ledgers,
        force,
    })
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

/// The config file's path and text. An explicit `--config` that cannot be read is an
/// error; the default file is optional, and absent means unconfigured.
pub fn read_config(flags: &Flags, homes: &Homes) -> Result<(PathBuf, String), String> {
    match &flags.config {
        Some(path) => std::fs::read_to_string(path)
            .map(|text| (path.clone(), text))
            .map_err(|e| format!("--config {}: {e}", path.display())),
        None => {
            let path = homes.config.join("config.toml");
            match std::fs::read_to_string(&path) {
                Ok(text) => Ok((path, text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((path, String::new())),
                Err(e) => Err(format!("reading {}: {e}", path.display())),
            }
        }
    }
}

/// Merge flags over the config text into validated [`Settings`].
pub fn settings(flags: &Flags, text: &str, homes: &Homes) -> Result<Settings, String> {
    check_keys(text)?;
    let http = resolve_bind(flags.bind.as_deref(), flags.port, text)?;
    refuse_non_loopback(http)?;
    let socket = flags
        .socket
        .clone()
        .or_else(|| value_for(text, "gonk.socket"))
        .map(|spelled| expand_home(&spelled, &homes.home))
        .unwrap_or_else(|| homes.data.join(SOCKET_NAME));
    let quic = if flags.no_quic {
        None
    } else {
        Some(
            match flags
                .quic_bind
                .clone()
                .or_else(|| value_for(text, "gonk.quic.bind"))
            {
                Some(spelled) => parse_bind(&spelled).map_err(|e| format!("quic {e}"))?,
                None => SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
            },
        )
    };
    // A Unix socket path is bounded by `sun_path` — 104 bytes on macOS, 108 on Linux, the
    // terminator included. Refused here, before the store is opened, rather than as a door
    // that dies a moment after the banner says it is serving.
    let socket_bytes = socket.as_os_str().len();
    if socket_bytes >= SOCKET_PATH_LIMIT {
        return Err(format!(
            "socket path {} is {socket_bytes} bytes; a Unix socket path must be shorter than \
             {SOCKET_PATH_LIMIT} — set `gonk.socket` (or --socket) to a shorter path",
            socket.display()
        ));
    }
    let http_ledgers = if !flags.http_ledgers.is_empty() {
        flags.http_ledgers.clone()
    } else {
        let lines = values_for(text, "gonk.http.ledger");
        if lines.is_empty() {
            vec![DEFAULT_LEDGER.to_string()]
        } else {
            lines
        }
    };
    Ok(Settings {
        http,
        socket,
        quic,
        http_ledgers,
    })
}

/// Parse an `IP:PORT` socket address — an IP, not a hostname: a bind is a listening
/// posture, and a posture should not depend on what a resolver says today.
pub fn parse_bind(spelled: &str) -> Result<SocketAddr, String> {
    spelled.parse().map_err(|_| {
        format!("bind `{spelled}`: expected IP:PORT (e.g. 127.0.0.1:1060, [::1]:1060)")
    })
}

/// The HTTP door's address, from flags then config then the default — standalone
/// `ikigai-web`'s rule with `gonk.` keys. `bind` and `port` are one setting spelled two ways,
/// so both at one level is an error; a port alone always means loopback.
pub fn resolve_bind(
    bind_flag: Option<&str>,
    port_flag: Option<u16>,
    text: &str,
) -> Result<SocketAddr, String> {
    if bind_flag.is_some() && port_flag.is_some() {
        return Err("--bind and --port conflict — --bind names the full IP:PORT".to_string());
    }
    let bind_key = value_for(text, "gonk.bind");
    let port_key = value_for(text, "gonk.port");
    if bind_key.is_some() && port_key.is_some() {
        return Err(
            "config sets both `gonk.bind` and `gonk.port` — they are one setting spelled two \
             ways; keep `gonk.bind` and drop `gonk.port`"
                .to_string(),
        );
    }
    if let Some(spelled) = bind_flag {
        return parse_bind(spelled);
    }
    if let Some(port) = port_flag {
        return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
    }
    if let Some(spelled) = bind_key {
        return parse_bind(&spelled).map_err(|e| format!("gonk.{e}"));
    }
    if let Some(port) = port_key {
        let port: u16 = port
            .parse()
            .map_err(|_| format!("gonk.port: `{port}` is not a port number"))?;
        return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
    }
    Ok(SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)))
}

/// ★ **The HTTP door binds loopback, or the server does not start.**
///
/// It is unauthenticated: it grants read and write on its ledgers to whatever reaches it,
/// and on loopback that is this machine's own processes. On a LAN it would be everyone on
/// the LAN, with the same grant — and HTTP to a LAN cannot do WebAuthn either (no secure
/// context), so nothing could be layered on it to tell callers apart. A warning would scroll
/// past in a launchd log while the door stayed open; a refusal is legible and has a fix, the
/// QUIC door, which authenticates every connection.
pub fn refuse_non_loopback(addr: SocketAddr) -> Result<(), String> {
    if crate::doors::is_loopback(addr.ip()) {
        return Ok(());
    }
    Err(format!(
        "refusing to serve HTTP on {addr}: the HTTP door is unauthenticated — it grants read \
         and write on its ledgers to whoever reaches it — so it binds loopback only \
         (127.0.0.1 or [::1]). To reach gonk from another machine, use the QUIC door, which \
         admits a connection only by a client certificate enrolled with `ikigai-gonk client \
         add`."
    ))
}

/// Refuse any `gonk.*` key this server does not read.
fn check_keys(text: &str) -> Result<(), String> {
    let unknown: Vec<&str> = lines(text)
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("gonk.") && !KEYS.contains(name))
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unknown config key(s): {} — this server reads {}",
            unknown.join(", "),
            KEYS.join(", ")
        ))
    }
}

/// `~/`-expansion against `home`.
fn expand_home(spelled: &str, home: &Path) -> PathBuf {
    match spelled.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(spelled),
    }
}

/// The first `key = value` line for `key`, unquoted.
fn value_for(text: &str, key: &str) -> Option<String> {
    lines(text).find(|(name, _)| *name == key).map(|(_, v)| v)
}

/// Every `key = value` line for `key`, in file order.
fn values_for(text: &str, key: &str) -> Vec<String> {
    lines(text)
        .filter(|(name, _)| *name == key)
        .map(|(_, v)| v)
        .collect()
}

fn lines(text: &str) -> impl Iterator<Item = (&str, String)> + '_ {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| {
            (
                name.trim(),
                value.trim().trim_matches(['"', '\'']).trim().to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn homes() -> Homes {
        Homes {
            home: PathBuf::from("/home/u"),
            config: PathBuf::from("/home/u/.config/ikigai"),
            data: PathBuf::from("/home/u/.ikigai"),
        }
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn no_config_is_loopback_1060_with_the_default_ledger() {
        let settings = settings(&Flags::default(), "", &homes()).unwrap();
        assert_eq!(settings.http, addr("127.0.0.1:1060"));
        assert_eq!(settings.socket, PathBuf::from("/home/u/.ikigai/gonk.sock"));
        assert_eq!(settings.quic, Some(addr("0.0.0.0:1060")));
        assert_eq!(settings.http_ledgers, ["default"]);
    }

    #[test]
    fn bind_resolution_walks_flags_then_config_then_default() {
        assert_eq!(
            resolve_bind(Some("[::1]:9001"), None, "gonk.port = 7\n"),
            Ok(addr("[::1]:9001"))
        );
        // A port flag means loopback even against a config bind.
        assert_eq!(
            resolve_bind(None, Some(9002), "gonk.bind = \"127.0.0.2:1\"\n"),
            Ok(addr("127.0.0.1:9002"))
        );
        assert_eq!(
            resolve_bind(None, None, "gonk.port = '9999'\n"),
            Ok(addr("127.0.0.1:9999"))
        );
        assert!(resolve_bind(Some("127.0.0.1:1"), Some(2), "").is_err());
        assert!(resolve_bind(None, None, "gonk.bind = \"127.0.0.1:1\"\ngonk.port = 2\n").is_err());
        assert!(resolve_bind(None, None, "gonk.bind = \"localhost:1060\"\n").is_err());
    }

    /// ★ The decision: an unauthenticated door never faces a network.
    #[test]
    fn a_non_loopback_http_bind_is_refused() {
        for spelled in ["0.0.0.0:1060", "192.168.1.10:1060", "[::]:1060"] {
            let flags = Flags {
                bind: Some(spelled.to_string()),
                ..Flags::default()
            };
            let refused = settings(&flags, "", &homes()).unwrap_err();
            assert!(refused.contains("loopback only"), "{spelled}: {refused}");
            assert!(
                refused.contains("QUIC"),
                "the refusal names the door that works"
            );
        }
        let refused = settings(
            &Flags::default(),
            "gonk.bind = \"0.0.0.0:1060\"\n",
            &homes(),
        );
        assert!(refused.is_err(), "the config spelling is refused too");
        assert!(settings(
            &Flags {
                bind: Some("[::1]:1060".into()),
                ..Flags::default()
            },
            "",
            &homes()
        )
        .is_ok());
    }

    #[test]
    fn an_unknown_gonk_key_is_an_error_and_other_keys_are_not_ours() {
        let refused = settings(&Flags::default(), "gonk.prot = 1060\n", &homes()).unwrap_err();
        assert!(refused.contains("gonk.prot"), "{refused}");
        // The shared config.toml carries everyone's keys; only gonk.* is checked.
        assert!(settings(
            &Flags::default(),
            "mount = \"prefer urn:x:=y\"\nweb.port = 8642\n",
            &homes()
        )
        .is_ok());
    }

    /// A socket path the kernel cannot bind is refused before the store is opened.
    #[test]
    fn an_unbindable_socket_path_is_refused_up_front() {
        let long = format!("/{}/gonk.sock", "d".repeat(SOCKET_PATH_LIMIT));
        let flags = Flags {
            socket: Some(long),
            ..Flags::default()
        };
        let refused = settings(&flags, "", &homes()).unwrap_err();
        assert!(refused.contains("shorter than 104"), "{refused}");
    }

    #[test]
    fn repeatable_flags_replace_the_config_lines() {
        let text = "gonk.http.ledger = \"acme\"\ngonk.http.ledger = \"bosatsu\"\ngonk.socket = \"~/s/g.sock\"\n";
        let from_file = settings(&Flags::default(), text, &homes()).unwrap();
        assert_eq!(from_file.http_ledgers, ["acme", "bosatsu"]);
        assert_eq!(from_file.socket, PathBuf::from("/home/u/s/g.sock"));
        let flags = Flags {
            http_ledgers: vec!["default".into()],
            no_quic: true,
            ..Flags::default()
        };
        let overridden = settings(&flags, text, &homes()).unwrap();
        assert_eq!(overridden.http_ledgers, ["default"]);
        assert_eq!(overridden.quic, None);
    }

    #[test]
    fn subcommands_parse() {
        let args = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        match parse_args(args(
            "client add laptop --ledger default=write --ledger acme=read",
        ))
        .unwrap()
        {
            Command::ClientAdd {
                name,
                ledgers,
                cert,
                force,
            } => {
                assert_eq!(name, "laptop");
                assert_eq!(
                    ledgers,
                    [
                        ("default".to_string(), Authority::Write),
                        ("acme".to_string(), Authority::Read)
                    ]
                );
                assert!(cert.is_none() && !force);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_args(args("grants acme delete")).unwrap(),
            Command::Grants {
                authority: Authority::Delete,
                ..
            }
        ));
        assert!(matches!(
            parse_args(args("serve --port 1070")).unwrap(),
            Command::Serve(_)
        ));
        assert!(parse_args(args("client add laptop --ledger default=admin")).is_err());
        assert!(parse_args(args("--bogus")).is_err());
    }
}
