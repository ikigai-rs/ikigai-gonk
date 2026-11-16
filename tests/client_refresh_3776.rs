//! Ledger [#3776](http://localhost:1062/l/default/item/3776): `ikigai-gonk client refresh` adds to
//! every stored grant what its ledger roles mint now, after `ikigai-ledger` 0.23.0 gave every role
//! its ledger's session graph (gonk PR 385). A grant stored before then lacks those tokens: a
//! worker's session summary is refused, a reader sees no sessions, a delete is refused before
//! anything moves. `client add … --force` rebuilt a grant whole and dropped hand-added scopes, so
//! it was the wrong tool for a grant that had grown by hand.
//!
//! Every grant here is shaped as gonk minted it under `ikigai-ledger` 0.22.x: today's
//! `grants_for` minus the session graph's store tokens.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod scratch_gonk;

use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_gonk::grants::{grants_for, ledger_graph_grants, Authority};
use ikigai_gonk::quic;
use ikigai_gonk::refresh::{self, plan_grant, ROLES};
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

/// `grants_for(ledger, role)` as gonk minted it before `ikigai-ledger` 0.23.0: no session graph.
fn as_0_22(ledger: &str, role: Authority) -> Vec<String> {
    grants_for(ledger, role)
        .unwrap()
        .into_iter()
        .filter(|t| !t.ends_with(":session"))
        .collect()
}

fn set(tokens: &[String]) -> BTreeSet<String> {
    tokens.iter().cloned().collect()
}

/// A scratch config home, holding `gonk/grants.json` and nothing else.
struct Home {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Home {
    fn with_grants(grants: &serde_json::Value) -> Home {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for dir in ["home", "cfg/gonk", "data"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let home = Home { _tmp: tmp, root };
        std::fs::write(
            home.grants(),
            serde_json::to_string_pretty(grants).unwrap() + "\n",
        )
        .unwrap();
        home
    }

    fn gonk_dir(&self) -> PathBuf {
        self.root.join("cfg/gonk")
    }

    fn grants(&self) -> PathBuf {
        self.gonk_dir().join("grants.json")
    }

    fn layout(&self) -> quic::Layout {
        quic::Layout::in_config_home(&self.root.join("cfg"))
    }

    /// The real binary, as an operator types it.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .arg("--config-home")
            .arg(self.root.join("cfg"))
            .arg("--data-home")
            .arg(self.root.join("data"))
            .env("HOME", self.root.join("home"))
            .output()
            .expect("run ikigai-gonk")
    }

    fn refresh(&self, write: bool) -> String {
        let mut args = vec!["client", "refresh"];
        if write {
            args.push("--write");
        }
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "client refresh: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// The file names in `gonk/`, sorted.
    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.gonk_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn stored(&self, grant: &str) -> Vec<String> {
        quic::read_grants(&self.grants()).unwrap()[grant].clone()
    }
}

/// ★ One stored grant per role, shaped as 0.22.x minted it: refresh adds exactly the
/// session-graph tokens that role now mints, and the result equals `grants_for` for that role.
#[test]
fn each_role_minted_under_0_22_gains_exactly_its_session_tokens() {
    for role in ROLES {
        let old = as_0_22("acme", role);
        let now = grants_for("acme", role).unwrap();
        let missing: BTreeSet<String> = set(&now).difference(&set(&old)).cloned().collect();
        assert!(
            !missing.is_empty() && missing.iter().all(|t| t.ends_with(":session")),
            "{role:?}: the 0.22 shape lacks only the session graph: {missing:?}"
        );
        let plan = plan_grant("g", &old);
        assert_eq!(plan.ledgers.len(), 1, "{role:?}: {plan:?}");
        assert_eq!(plan.ledgers[0].roles, [role], "{role:?} read back");
        assert_eq!(set(&plan.adds()), missing, "{role:?} adds");
        let mut after = old.clone();
        after.extend(plan.adds());
        assert_eq!(set(&after), set(&now), "{role:?}: refreshed == grants_for");
        assert!(plan.unmatched.is_empty(), "{role:?}: {plan:?}");
    }
}

/// The same, through the binary over a file holding all six, and a grant already current is
/// left alone.
#[test]
fn the_command_refreshes_a_store_of_every_role() {
    let mut doc = serde_json::Map::new();
    for role in ROLES {
        doc.insert(
            refresh::role_name(role).to_string(),
            serde_json::json!(as_0_22("default", role)),
        );
    }
    doc.insert(
        "current".to_string(),
        serde_json::json!(grants_for("default", Authority::Write).unwrap()),
    );
    let home = Home::with_grants(&serde_json::Value::Object(doc));
    let report = home.refresh(true);
    for role in ROLES {
        assert_eq!(
            set(&home.stored(refresh::role_name(role))),
            set(&grants_for("default", role).unwrap()),
            "{role:?}\n{report}"
        );
    }
    assert!(
        report.contains("grant `current`: nothing to add"),
        "{report}"
    );
    assert!(
        report.contains("printf plan | ikigai -c 'sink urn:iki:ledger:default:sessions'"),
        "{report}"
    );
    assert!(
        report.contains("printf move | ikigai -c 'sink urn:iki:ledger:default:sessions'"),
        "{report}"
    );
    assert!(report.contains("needs no restart"), "{report}");
}

/// ★ A grant that has grown by hand keeps every scope it had, in its order, with the new
/// tokens after them; and a grant in the object form keeps its other fields.
#[test]
fn hand_added_scopes_are_all_kept() {
    let mut brian = as_0_22("default", Authority::Purge);
    brian.extend(as_0_22("acme", Authority::Write));
    brian.extend(ikigai_gonk::grants::alias_grants("acme", "gh").unwrap());
    brian.extend(ledger_graph_grants("raw").unwrap());
    brian.extend([
        "urn:cap:net:localhost".to_string(),
        "urn:cap:script:read:triage".to_string(),
        "urn:cap:browse:read:*".to_string(),
        "urn:cap:ledger:link-in:other".to_string(),
    ]);
    let mut seen = BTreeSet::new();
    brian.retain(|t| seen.insert(t.clone()));
    let grants = serde_json::json!({
        "brian": brian,
        "obj": { "scopes": as_0_22("default", Authority::Worker), "note": "kept" },
    });
    let home = Home::with_grants(&grants);
    home.refresh(true);

    let after = home.stored("brian");
    assert_eq!(
        &after[..brian.len()],
        &brian[..],
        "every original, in order"
    );
    let added = &after[brian.len()..];
    let expected: BTreeSet<String> = [
        "urn:cap:store:read:graph:urn:iki:ledger:graph:default:session",
        "urn:cap:store:write:graph:urn:iki:ledger:graph:default:session",
        "urn:cap:store:read:graph:urn:iki:ledger:graph:acme:session",
        "urn:cap:store:write:graph:urn:iki:ledger:graph:acme:session",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(set(added), expected);
    assert_eq!(
        added.len(),
        expected.len(),
        "nothing added twice: {added:?}"
    );

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.grants()).unwrap()).unwrap();
    assert_eq!(doc["obj"]["note"], "kept");
    assert_eq!(
        set(&home.stored("obj")),
        set(&grants_for("default", Authority::Worker).unwrap())
    );
}

/// ★ A raw `--ledger-graph` grant and a lone family token match no role: untouched, and listed.
#[test]
fn a_raw_graph_grant_and_a_lone_token_are_left_alone_and_listed() {
    let raw = ledger_graph_grants("acme").unwrap();
    let lone = vec!["urn:cap:ledger:link-in:acme".to_string()];
    let mirror = ikigai_gonk::grants::mirror_grants("acme").unwrap();
    let home = Home::with_grants(&serde_json::json!({
        "raw": raw, "lone": lone, "mirror": mirror,
    }));
    let before = std::fs::read(home.grants()).unwrap();
    let report = home.refresh(true);
    assert_eq!(std::fs::read(home.grants()).unwrap(), before, "{report}");
    for (grant, tokens) in [("raw", &raw), ("lone", &lone), ("mirror", &mirror)] {
        let plan = plan_grant(grant, tokens);
        assert!(plan.adds().is_empty(), "{plan:?}");
        assert_eq!(plan.unmatched, [("acme".to_string(), tokens.clone())]);
        assert!(
            report.contains(&format!(
                "grant `{grant}`: nothing to add\n  ledger acme  no role: left alone"
            )),
            "{report}"
        );
    }
    assert!(report.contains("nothing written"), "{report}");
}

/// ★ A dry run writes nothing; `--write` backs the file up and writes it; a second `--write`
/// adds nothing and makes no second backup; the backup is the original, byte for byte.
#[test]
fn dry_run_writes_nothing_and_write_is_idempotent_with_a_backup() {
    let home = Home::with_grants(&serde_json::json!({
        "w": as_0_22("default", Authority::Worker),
        "r": as_0_22("default", Authority::Read),
    }));
    let original = std::fs::read(home.grants()).unwrap();
    let files = home.files();

    let dry = home.refresh(false);
    assert_eq!(std::fs::read(home.grants()).unwrap(), original, "{dry}");
    assert_eq!(home.files(), files, "a dry run leaves gonk/ as it found it");
    assert!(dry.contains("Dry run: nothing written"), "{dry}");
    assert!(
        dry.contains("3 token(s) to add to 2 of 2 grant(s)"),
        "{dry}"
    );
    assert!(
        dry.contains(
            "  ledger default  worker\n    adds     urn:cap:store:read:graph:urn:iki:ledger:graph:default:session"
        ),
        "{dry}"
    );

    let wrote = home.refresh(true);
    assert!(
        wrote.contains("added 3 token(s) to 2 of 2 grant(s)"),
        "{wrote}"
    );
    let backups: Vec<String> = home
        .files()
        .into_iter()
        .filter(|n| n.starts_with("grants.json.bak-") && n.ends_with("-refresh"))
        .collect();
    assert_eq!(backups.len(), 1, "{:?}", home.files());
    let backup = home.gonk_dir().join(&backups[0]);
    assert!(
        wrote.contains(&format!("backup {}", backup.display())),
        "{wrote}"
    );
    assert_eq!(std::fs::read(&backup).unwrap(), original);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&backup).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the backup is readable by this user only");
    }
    let refreshed = std::fs::read(home.grants()).unwrap();
    assert_ne!(refreshed, original);

    let again = home.refresh(true);
    assert!(again.contains("nothing to add; nothing written"), "{again}");
    assert_eq!(std::fs::read(home.grants()).unwrap(), refreshed);
    assert_eq!(
        home.files()
            .iter()
            .filter(|n| n.starts_with("grants.json.bak-"))
            .count(),
        1
    );
}

/// With no `grants.json` there is nothing to refresh, and nothing is created.
#[test]
fn no_grants_file_is_nothing_to_do() {
    let home = Home::with_grants(&serde_json::json!({}));
    std::fs::remove_file(home.grants()).unwrap();
    let report = refresh::refresh(&home.layout(), true, 0).unwrap();
    assert!(!report.found);
    assert!(refresh::render(&report).contains("does not exist"));
    assert!(!home.grants().exists());
}

// ---------------------------------------------------------------- end to end, through a gonk

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn gonk_cli(config: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
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
    String::from_utf8(out.stdout).unwrap()
}

impl Gonk {
    fn as_client(&self, name: &str, request: Request) -> Result<Representation, Error> {
        let bundle = self.layout.quic_dir().join("clients").join(name);
        let client = self
            .dial_quic_as(&bundle)
            .unwrap_or_else(|e| panic!("QUIC connect as {name}: {e}"));
        client.issue(request).map(|(repr, _)| repr)
    }

    fn over_socket(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }
}

/// A session summary the ledger's closed `ledger:SessionSummaryShape` accepts.
const SUMMARY: &str = r#"{
  "session": "5e55105e",
  "machineName": "lawnboy",
  "harness": "claude",
  "model": "claude-opus-5-5",
  "exitStatus": 0,
  "wallClockMs": 60000,
  "turns": 3,
  "toolCalls": 7,
  "inputTokens": 100,
  "cachedInputTokens": 1000,
  "cacheCreationTokens": 10,
  "outputTokens": 50
}"#;

/// ★ End to end: a worker and an owner's delete-level grant, stored as 0.22.x minted them, are
/// refused a session summary and a delete by a running gonk; `client refresh --write` on the
/// live config home lets both through on their next connection, with no restart.
#[test]
fn a_refreshed_worker_records_a_summary_and_a_refreshed_owner_deletes() {
    let gonk = Gonk::with_quic_prepared("", &|config, home| {
        gonk_cli(
            config,
            home,
            &["client", "add", "w1", "--worker", "default"],
        );
        gonk_cli(
            config,
            home,
            &["client", "add", "owner", "--ledger", "default=delete"],
        );
        // Store both as gonk minted them under ikigai-ledger 0.22.x.
        let layout = quic::Layout::in_config_home(config);
        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(layout.grants_json()).unwrap()).unwrap();
        doc["w1"] = serde_json::json!(as_0_22("default", Authority::Worker));
        doc["owner"] = serde_json::json!(as_0_22("default", Authority::Delete));
        std::fs::write(layout.grants_json(), doc.to_string()).unwrap();
    });
    for title in ["Fix the widget", "Misfiled"] {
        gonk.over_socket(request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", title)],
        ))
        .expect("append over the socket");
    }
    gonk.as_client(
        "w1",
        request(
            Verb::Sink,
            "urn:iki:ledger:claim",
            &[("item", "1"), ("content", "w1"), ("lease", "1h")],
        ),
    )
    .expect("a worker claims");
    gonk.as_client(
        "w1",
        request(
            Verb::Sink,
            "urn:iki:ledger:comment",
            &[("item", "1"), ("content", "done"), ("handback", "true")],
        ),
    )
    .expect("a worker hands the item back");

    let summary = || {
        gonk.as_client(
            "w1",
            request(
                Verb::Sink,
                "urn:iki:ledger:item:1:sessions",
                &[("content", SUMMARY)],
            ),
        )
    };
    let delete = || {
        gonk.as_client(
            "owner",
            request(
                Verb::Delete,
                "urn:iki:ledger:item:2",
                &[("reason", "wrong place")],
            ),
        )
    };
    // ── Before: both refused.
    match summary() {
        Err(Error::Denied(_)) => {}
        other => panic!("a 0.22-shaped worker's summary: expected Denied, got {other:?}"),
    }
    match delete() {
        Err(Error::Denied(_)) => {}
        other => panic!("a 0.22-shaped delete: expected Denied, got {other:?}"),
    }

    // ── The refresh, against the running server's config home.
    let config = gonk.layout.dir().parent().unwrap().to_path_buf();
    let home = tempfile::tempdir().unwrap();
    let report = gonk_cli(&config, home.path(), &["client", "refresh", "--write"]);
    assert!(report.contains("grant `w1`: 2 tokens to add"), "{report}");
    assert!(
        report.contains("grant `owner`: 2 tokens to add"),
        "{report}"
    );
    assert!(report.contains("grant `alpha`: nothing to add"), "{report}");

    // ── After: both through, on a new connection, with no restart.
    let recorded = summary().expect("a refreshed worker records its session's summary");
    assert!(
        String::from_utf8_lossy(&recorded.bytes).contains("5e55105e"),
        "{recorded:?}"
    );
    delete().expect("a refreshed delete-level grant deletes");

    // ── And the move step the report prints answers over the owner's socket.
    let plan = gonk
        .over_socket(request(
            Verb::Sink,
            "urn:iki:ledger:default:sessions",
            &[("content", "plan")],
        ))
        .expect("the sessions plan answers");
    assert!(!plan.bytes.is_empty());
}
