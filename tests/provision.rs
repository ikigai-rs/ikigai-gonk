//! The provisioning commands' `--browse` roles and their rewrite reporting, end to end: the
//! real binary against a scratch config home (ledger
//! [#435](http://localhost:1060/l/default/item/435)).
//!
//! Nothing here touches the operator's config home or `~/.ikigai`: every run gets its own
//! `HOME` and `XDG_CONFIG_HOME`, and no server is started — `grants`, `passkey invite` and
//! `client add` write files and exit.
//!
//! What is pinned, because each is what an operator relies on and none is visible from the
//! library:
//!
//! - `grants --browse derive` prints the net scope of THIS config's `gonk.mount` host;
//! - `derive` on a config with no mount is refused, with the sentence that says why;
//! - re-enrolling a hand-widened grant without `--force` is refused, NAMING the scopes it
//!   would drop, and writes nothing (no grant change, no invite);
//! - with `--force` it is written and the output names what it removed and added.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ikigai_gonk::grants::DERIVE_NEEDS_A_MOUNT;

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    /// A config home with one browse root and, when `mounted`, a QUIC mount at
    /// `127.0.0.1:4433` (its certificate directory exists; nothing is dialed).
    fn new(mounted: bool) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        let certs = root.join("certs");
        for dir in [&repo, &certs, &root.join("home"), &root.join("xdg/ikigai")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let mut config = format!("gonk.browse.root = \"demo={}\"\n", repo.display());
        if mounted {
            config.push_str(&format!(
                "gonk.mount = \"prefer urn:llm:=quic://127.0.0.1:4433 {}\"\n",
                certs.display()
            ));
        }
        std::fs::write(root.join("xdg/ikigai/config.toml"), config).unwrap();
        World { _tmp: tmp, root }
    }

    fn gonk_dir(&self) -> PathBuf {
        self.root.join("xdg/ikigai/gonk")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("xdg"))
            .output()
            .expect("run ikigai-gonk")
    }

    fn grants(&self) -> serde_json::Value {
        let text = std::fs::read_to_string(self.gonk_dir().join("grants.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn tokens(output: &Output) -> Vec<String> {
    assert!(output.status.success(), "{}", stderr(output));
    serde_json::from_slice(&output.stdout).expect("a JSON list of tokens")
}

#[test]
fn the_printer_shows_each_role_with_this_mounts_host() {
    let world = World::new(true);
    assert_eq!(
        tokens(&world.run(&["grants", "--browse", "read"])),
        [
            "urn:cap:browse:read:*",
            "urn:cap:store:read:graph:urn:iki:browse:graph:default",
        ]
    );
    assert_eq!(
        tokens(&world.run(&["grants", "--browse", "derive", "--root", "demo"])),
        [
            "urn:cap:browse:read:demo",
            "urn:cap:store:read:graph:urn:iki:browse:graph:default",
            "urn:cap:annotate",
            "urn:cap:net:127.0.0.1",
        ]
    );
    let typo = world.run(&["grants", "--browse", "read", "--root", "dem"]);
    assert!(!typo.status.success());
    assert!(
        stderr(&typo).contains("configured: demo"),
        "{}",
        stderr(&typo)
    );

    let unmounted = World::new(false);
    let refused = unmounted.run(&["grants", "--browse", "derive"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains(DERIVE_NEEDS_A_MOUNT),
        "{}",
        stderr(&refused)
    );
    assert_eq!(
        tokens(&unmounted.run(&["grants", "--browse", "read"])).len(),
        2,
        "read needs no mount"
    );
}

/// ★ The live case behind ledger #435: a grant widened by hand, then re-enrolled with flags.
#[test]
fn re_enrolling_a_hand_widened_grant_names_what_it_would_drop() {
    let world = World::new(true);
    std::fs::create_dir_all(world.gonk_dir()).unwrap();
    let hand_widened = serde_json::json!({
        "brian": [
            "urn:cap:ledger:read:default",
            "urn:cap:store:read:graph:urn:iki:ledger:graph:default",
            "urn:cap:ledger:write:default",
            "urn:cap:store:write:graph:urn:iki:ledger:graph:default",
            "urn:cap:ledger:delete:default",
            "urn:cap:store:write:graph:urn:iki:ledger:graph:default:deleted",
            "urn:cap:browse:read:*",
            "urn:cap:annotate",
            "urn:cap:net:localhost",
            "urn:cap:exec:gh",
        ]
    });
    std::fs::write(
        world.gonk_dir().join("grants.json"),
        serde_json::to_string_pretty(&hand_widened).unwrap(),
    )
    .unwrap();
    let invite = [
        "passkey",
        "invite",
        "brian",
        "--ledger",
        "default=delete",
        "--browse",
        "derive",
    ];

    let refused = world.run(&invite);
    assert!(!refused.status.success(), "{}", stdout(&refused));
    let why = stderr(&refused);
    for line in [
        "would NARROW it and widen it",
        "    removes  urn:cap:net:localhost",
        "    removes  urn:cap:exec:gh",
        "    adds     urn:cap:store:read:graph:urn:iki:browse:graph:default",
        "    adds     urn:cap:net:127.0.0.1",
        "--force",
    ] {
        assert!(why.contains(line), "`{line}` in: {why}");
    }
    assert_eq!(world.grants(), hand_widened, "nothing was written");
    assert!(
        !world.gonk_dir().join("invites.json").exists(),
        "a refused rewrite mints no invite"
    );

    let forced = world.run(&[&invite[..], &["--force"]].concat());
    assert!(forced.status.success(), "{}", stderr(&forced));
    let said = stdout(&forced);
    for line in [
        "REPLACED     grant `brian` (--force), NARROWING it",
        "    removes  urn:cap:net:localhost",
        "    removes  urn:cap:exec:gh",
        "    adds     urn:cap:net:127.0.0.1",
    ] {
        assert!(said.contains(line), "`{line}` in: {said}");
    }
    assert_eq!(
        world.grants()["brian"],
        serde_json::json!([
            "urn:cap:ledger:read:default",
            "urn:cap:store:read:graph:urn:iki:ledger:graph:default",
            "urn:cap:ledger:write:default",
            "urn:cap:store:write:graph:urn:iki:ledger:graph:default",
            "urn:cap:ledger:delete:default",
            "urn:cap:store:write:graph:urn:iki:ledger:graph:default:deleted",
            "urn:cap:browse:read:*",
            "urn:cap:store:read:graph:urn:iki:browse:graph:default",
            "urn:cap:annotate",
            "urn:cap:net:127.0.0.1",
        ])
    );

    // The same command again is no change: written without --force, nothing reported.
    let again = world.run(&invite);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(!stdout(&again).contains("REPLACED"), "{}", stdout(&again));
}

/// `client add` takes the role too, and reads the config only when it needs to.
#[test]
fn a_certificate_can_be_enrolled_under_a_role() {
    let world = World::new(true);
    let added = world.run(&["client", "add", "box", "--browse", "read", "--root", "demo"]);
    assert!(added.status.success(), "{}", stderr(&added));
    assert_eq!(
        world.grants()["box"],
        serde_json::json!([
            "urn:cap:browse:read:demo",
            "urn:cap:store:read:graph:urn:iki:browse:graph:default",
        ])
    );
    // A ledger-only enrolment needs no config at all, exactly as before.
    let bare = World::new(false);
    std::fs::remove_file(bare.root.join("xdg/ikigai/config.toml")).unwrap();
    let ledger_only = bare.run(&["client", "add", "laptop", "--ledger", "default=read"]);
    assert!(ledger_only.status.success(), "{}", stderr(&ledger_only));
    let refused = bare.run(&["client", "add", "other", "--browse", "derive"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains(DERIVE_NEEDS_A_MOUNT),
        "{}",
        stderr(&refused)
    );
    assert!(
        !Path::new(&bare.gonk_dir().join("quic/clients/other")).exists(),
        "a refused role mints no certificate bundle"
    );
}
