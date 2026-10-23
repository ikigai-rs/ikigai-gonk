//! `ikigai-gonk onboard init` sets the controller up with no file to copy or edit (ledger
//! #2123): the kinds file, the `gonk.onboard` and `gonk.onboard.door` lines, and the layer
//! host's `[onboard]` table, asking nothing with no terminal on stdin.
//!
//! ⚠ The binary mints into the REAL keystore (the Keychain on a Mac), so every run of the binary
//! here has the controller's public half in place already, and mints nothing: the fresh mint
//! is driven through the library with a stand-in.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use ikigai_gonk::config::{Homes, QuicBind};
use ikigai_gonk::onboard::{self, GonkDoor};
use ikigai_gonk::onboard_init::{self as init, InitFlags};

const PEM: &str = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAstandin0000000000000000000000000000000=\n-----END PUBLIC KEY-----\n";

struct Scratch {
    _dir: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

impl Scratch {
    fn new() -> Scratch {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let config = dir.path().join("config");
        let data = dir.path().join("data");
        for d in [&home, &config, &data] {
            std::fs::create_dir_all(d).unwrap();
        }
        Scratch {
            _dir: dir,
            home,
            config,
            data,
        }
    }

    /// A layers-client certificate set for `name`, as the layers client lays one out.
    fn cert_set(&self, name: &str) -> PathBuf {
        let dir = self.home.join(".ikigai").join("layers-client").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["client.crt", "client.key", "server.crt"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        dir
    }

    fn key_path(&self) -> PathBuf {
        self.config
            .join("gonk")
            .join("onboard")
            .join("controller.pub.pem")
    }

    fn kinds_path(&self) -> PathBuf {
        self.config.join("gonk").join("onboard").join("kinds.ttl")
    }

    fn config_toml(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    fn backups(&self) -> usize {
        std::fs::read_dir(&self.config)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".bak")
            })
            .count()
    }

    /// `ikigai-gonk onboard init <args>`, stdin not a terminal.
    fn init(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(["onboard", "init", "--config-home"])
            .arg(&self.config)
            .arg("--data-home")
            .arg(&self.data)
            .args(args)
            .env("HOME", &self.home)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn with_no_terminal_init_takes_the_defaults_writes_what_is_missing_and_reruns_clean() {
    let s = Scratch::new();
    let certs = s.cert_set("brian");
    std::fs::create_dir_all(s.key_path().parent().unwrap()).unwrap();
    std::fs::write(s.key_path(), PEM).unwrap();
    std::fs::write(
        s.config_toml(),
        format!(
            "gonk.quic.bind = \"0.0.0.0:1062\"\ngonk.layers.view = \"quic://edge.example:4434 {}\"",
            certs.display()
        ),
    )
    .unwrap();

    let first = s.init(&[]);
    let out = text(&first);
    assert!(first.status.success(), "{out}");
    assert!(out.contains("kept      the controller key"), "{out}");
    assert!(out.contains("wrote     the kinds file"), "{out}");
    assert_eq!(
        std::fs::read_to_string(s.kinds_path()).unwrap(),
        onboard::DEFAULT_KINDS
    );
    let config = std::fs::read_to_string(s.config_toml()).unwrap();
    // The layer door and the cert set come from the Layers page's line; the door from the
    // host name and the bound QUIC port.
    let onboard_line = format!(
        "gonk.onboard = \"quic://edge.example:4434 {}\"",
        certs.display()
    );
    assert!(config.contains(&onboard_line), "{config}");
    let door = format!("{}.local:1062", onboard::short_host_name());
    assert!(
        config.contains(&format!("gonk.onboard.door = \"{door}\"")),
        "{config}"
    );
    assert!(
        config.starts_with("gonk.quic.bind"),
        "nothing rewritten: {config}"
    );
    assert_eq!(s.backups(), 1, "a backup beside config.toml");
    // The edge's table, with the participant the cert set names and the PEM.
    assert!(out.contains("[onboard]"), "{out}");
    assert!(out.contains("grants         = \"grants\""), "{out}");
    assert!(out.contains("controller     = \"brian\""), "{out}");
    assert!(
        out.contains("controller_key = \"controller.pub.pem\""),
        "{out}"
    );
    assert!(
        out.contains("# trust_dir"),
        "a placeholder, commented out: {out}"
    );
    assert!(out.contains("trust_reload   = \"live\""), "{out}");
    assert!(out.contains(PEM), "{out}");
    assert!(out.contains("the QUIC door is configured"), "{out}");

    // The written config is one serve accepts: the door is the setting.
    let homes = Homes {
        home: s.home.clone(),
        config: s.config.clone(),
        data: s.data.clone(),
    };
    let settings =
        ikigai_gonk::config::settings(&Default::default(), &config, &homes).expect("settings");
    assert_eq!(settings.onboard.unwrap().door, GonkDoor::Set(door));

    // A rerun mints nothing, writes nothing, and prints the table again.
    let second = s.init(&["--door", "other.example:1"]);
    let out = text(&second);
    assert!(second.status.success(), "{out}");
    assert!(out.contains("kept      the kinds file"), "{out}");
    assert!(out.contains("kept      gonk.onboard = "), "{out}");
    assert!(out.contains("kept      gonk.onboard.door = "), "{out}");
    assert!(out.contains("--door is not applied"), "{out}");
    assert!(!out.contains("added "), "{out}");
    assert_eq!(std::fs::read_to_string(s.config_toml()).unwrap(), config);
    assert_eq!(s.backups(), 1, "nothing written, no new backup");
    assert!(out.contains(PEM), "{out}");
}

#[test]
fn flags_answer_every_question_and_a_missing_cert_set_writes_no_line() {
    let s = Scratch::new();
    std::fs::create_dir_all(s.key_path().parent().unwrap()).unwrap();
    std::fs::write(s.key_path(), PEM).unwrap();
    // A kinds file that is there is kept, whatever it says.
    std::fs::write(s.kinds_path(), "# mine\n").unwrap();

    // No cert set anywhere: nothing can be written, and the exit says so.
    let none = s.init(&["--yes"]);
    let out = text(&none);
    assert_eq!(none.status.code(), Some(1), "{out}");
    assert!(out.contains("NOT added gonk.onboard"), "{out}");
    assert!(out.contains("NOT added gonk.onboard.door"), "{out}");
    assert!(!s.config_toml().exists(), "no file made for nothing");
    assert_eq!(std::fs::read_to_string(s.kinds_path()).unwrap(), "# mine\n");
    assert!(
        out.contains("# controller"),
        "the controller is a placeholder: {out}"
    );

    // Flags: a layer door, a cert set that is not a layers-client one, the socket.
    let certs = s.home.join("door-certs");
    std::fs::create_dir_all(&certs).unwrap();
    for f in ["client.crt", "client.key", "server.crt"] {
        std::fs::write(certs.join(f), "x").unwrap();
    }
    let ok = s.init(&[
        "--layer-door",
        "quic://layers.example:4434",
        "--cert-dir",
        "~/door-certs",
        "--door",
        "socket",
    ]);
    let out = text(&ok);
    assert!(ok.status.success(), "{out}");
    let config = std::fs::read_to_string(s.config_toml()).unwrap();
    assert_eq!(
        config,
        "gonk.onboard = \"quic://layers.example:4434 ~/door-certs\"\ngonk.onboard.door = \"socket\"\n"
    );
    assert_eq!(s.backups(), 0, "there was no file to back up");
    assert!(out.contains("tools/gonk.py init --socket"), "{out}");
    assert!(
        out.contains("# controller"),
        "not a layers-client set: {out}"
    );
}

/// The library run, with a stand-in for the Keychain mint and a terminal's answers.
fn env(s: &Scratch, text: &str, quic: QuicBind) -> init::Env {
    init::Env {
        homes: Homes {
            home: s.home.clone(),
            config: s.config.clone(),
            data: s.data.clone(),
        },
        config: s.config_toml(),
        text: text.to_string(),
        onboard: None,
        quic,
        socket: s.data.join("gonk.sock"),
        host: "mini".to_string(),
    }
}

fn run(env: &init::Env, flags: &InitFlags, answers: &str) -> (bool, String, Vec<String>) {
    let mut minted = Vec::new();
    let mut prompts = Vec::new();
    let mut ask = init::Prompt {
        input: std::io::Cursor::new(answers.as_bytes().to_vec()),
        out: &mut prompts,
    };
    let mut mint = |key: &str| {
        minted.push(key.to_string());
        Ok(PEM.to_string())
    };
    let mut out = Vec::new();
    let ok = init::run(flags, env, &mut ask, &mut mint, &mut out).unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8(prompts).unwrap(),
        String::from_utf8(out).unwrap()
    );
    (ok, all, minted)
}

#[test]
fn a_fresh_operator_answers_the_menu_and_the_key_is_minted_once() {
    let s = Scratch::new();
    let certs = s.cert_set("brian");
    let quic = QuicBind::Default("0.0.0.0:1062".parse().unwrap());
    let e = env(&s, "", quic);
    // Enter for the layer door, the cert set typed, 2 (from elsewhere), Enter for the address.
    let answers = format!("\n{}\n2\n\n", certs.display());
    let (ok, all, minted) = run(&e, &InitFlags::default(), &answers);
    assert!(ok, "{all}");
    assert_eq!(minted, [onboard::DEFAULT_KEY]);
    assert_eq!(std::fs::read_to_string(s.key_path()).unwrap(), PEM);
    assert!(
        all.contains("the layer host's door (HOST:PORT) [ikigai-rs.dev:4434]: "),
        "{all}"
    );
    assert!(all.contains("1  locally"), "{all}");
    assert!(all.contains("2  from elsewhere"), "{all}");
    assert!(all.contains("[mini.local:1062]"), "{all}");
    assert!(all.contains("the QUIC door is not open yet"), "{all}");
    let config = std::fs::read_to_string(s.config_toml()).unwrap();
    assert!(
        config.contains("gonk.onboard.door = \"mini.local:1062\""),
        "{config}"
    );

    // Present now: a second run mints nothing and asks nothing.
    let text = std::fs::read_to_string(s.config_toml()).unwrap();
    let (ok, all, minted) = run(&env(&s, &text, quic), &InitFlags::default(), "");
    assert!(ok, "{all}");
    assert!(minted.is_empty(), "{all}");
    assert!(all.contains("kept      the controller key"), "{all}");
}

#[test]
fn answering_one_chooses_the_socket() {
    let s = Scratch::new();
    let certs = s.cert_set("brian");
    let e = env(
        &s,
        "",
        QuicBind::Explicit("127.0.0.1:1062".parse().unwrap()),
    );
    let flags = InitFlags {
        cert_dir: Some(certs.display().to_string()),
        layer_door: Some("edge.example:4434".into()),
        ..InitFlags::default()
    };
    let (ok, all, _) = run(&e, &flags, "1\n");
    assert!(ok, "{all}");
    let config = std::fs::read_to_string(s.config_toml()).unwrap();
    assert!(
        config.contains("gonk.onboard.door = \"socket\""),
        "{config}"
    );
    assert!(all.contains("over its owner-only socket"), "{all}");

    // From elsewhere on a loopback bind is warned about.
    let s = Scratch::new();
    let certs = s.cert_set("brian");
    let e = env(
        &s,
        "",
        QuicBind::Explicit("127.0.0.1:1062".parse().unwrap()),
    );
    let flags = InitFlags {
        cert_dir: Some(certs.display().to_string()),
        layer_door: Some("edge.example:4434".into()),
        door: Some("mini.local:1062".into()),
        ..InitFlags::default()
    };
    let (_, all, _) = run(&e, &flags, "");
    assert!(all.contains("a loopback address"), "{all}");
}

#[test]
fn a_door_line_needs_the_onboard_line_and_is_parsed() {
    let s = Scratch::new();
    let homes = Homes {
        home: s.home.clone(),
        config: s.config.clone(),
        data: s.data.clone(),
    };
    let flags = Default::default();
    let alone = ikigai_gonk::config::settings(&flags, "gonk.onboard.door = \"socket\"", &homes);
    assert!(alone.unwrap_err().contains("no gonk.onboard line"));
    let certs = s.cert_set("brian");
    let line = format!(
        "gonk.onboard = \"quic://e.example:4434 {}\"\n",
        certs.display()
    );
    let bad = ikigai_gonk::config::settings(
        &flags,
        &format!("{line}gonk.onboard.door = \"quic://x:1\""),
        &homes,
    );
    assert!(bad.is_err());
    let none = ikigai_gonk::config::settings(&flags, &line, &homes).unwrap();
    assert_eq!(
        none.onboard.unwrap().door,
        GonkDoor::Default { port: Some(1062) }
    );
}
