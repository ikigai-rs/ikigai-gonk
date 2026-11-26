//! Ledger #4369: at startup gonk says what an upgraded install still needs to match a fresh
//! one, one line per plan that found something with the exact command that fixes it, and
//! `urn:iki:gonk:upgrade:status` says the same. A fresh install prints nothing.
//!
//! A REAL gonk (the binary, in a child process) over a scratch home: started on current state,
//! then given state in the old shapes (a grant as ikigai-ledger 0.22 minted it, a finding filed
//! about an old-form file IRI) and restarted.

mod scratch_gonk;

use ikigai_core::{ArgRef, Iri, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::upgrade;
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;
use serde_json::Value;

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn issue(gonk: &Gonk, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Vec<u8> {
    let client = ikigai_ipc::connect(&gonk.socket).expect("the owner socket");
    let (answer, _) = client
        .issue(request(verb, iri, args))
        .unwrap_or_else(|e| panic!("{iri}: {e}\n{}", gonk.log()));
    answer.bytes.to_vec()
}

fn status(gonk: &Gonk) -> Value {
    serde_json::from_slice(&issue(
        gonk,
        Verb::Source,
        upgrade::STATUS,
        &[("as", "application/json")],
    ))
    .expect("the status document")
}

fn upgrade_lines(gonk: &Gonk) -> Vec<String> {
    gonk.log()
        .lines()
        .filter(|l| l.starts_with("ikigai-gonk: upgrade:"))
        .map(str::to_string)
        .collect()
}

#[test]
fn an_old_install_is_told_what_it_needs_and_a_fresh_one_nothing() {
    let mut gonk = Gonk::start("");

    // ── Current state: nothing printed, and the status says so.
    assert!(upgrade_lines(&gonk).is_empty(), "{}", gonk.log());
    let fresh = status(&gonk);
    assert_eq!(fresh["grants"]["grants"], serde_json::json!([]), "{fresh}");
    assert_eq!(fresh["file_iris"], serde_json::json!([]), "{fresh}");
    let text = String::from_utf8(issue(&gonk, Verb::Source, upgrade::STATUS, &[])).unwrap();
    assert!(text.contains("nothing to do"), "{text}");

    // ── The old shapes: a finding about browse's own file IRI (before #3657) ...
    issue(
        &gonk,
        Verb::Sink,
        "urn:iki:ledger:default:append",
        &[
            ("content", "an old finding"),
            (
                "about",
                "urn:repo:demo:file:src/lib.rs urn:iki:finding:0123456789abcdef",
            ),
        ],
    );
    // ... and a worker grant as ikigai-ledger 0.22 minted it: no session graph.
    let old_worker: Vec<String> = grants_for("default", Authority::Worker)
        .unwrap()
        .into_iter()
        .filter(|t| !t.ends_with(":session"))
        .collect();
    let file = gonk.layout.grants_json();
    let mut doc: serde_json::Map<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    doc.insert("old-worker".into(), serde_json::json!(old_worker));
    std::fs::write(&file, Value::Object(doc).to_string()).unwrap();

    gonk.child.kill().unwrap();
    gonk.child.wait().unwrap();
    gonk.restart();

    // ── One line per plan, each naming the exact command.
    let lines = upgrade_lines(&gonk);
    assert_eq!(lines.len(), 2, "{}", gonk.log());
    assert!(
        lines[0].contains("1 grant(s) lack 2 token(s)")
            && lines[0].contains("(old-worker)")
            && lines[0].contains("client refresh --config-home")
            && lines[0].contains("--write`"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("ledger `default` has 1 finding about(s)")
            && lines[1].contains("canonical-file-iris --ledger default --socket")
            && lines[1].contains("--apply`"),
        "{}",
        lines[1]
    );

    // ── The status says the same, and nothing was applied.
    let old = status(&gonk);
    assert_eq!(old["grants"]["grants"][0]["grant"], "old-worker", "{old}");
    assert_eq!(old["file_iris"][0]["ledger"], "default", "{old}");
    assert_eq!(old["file_iris"][0]["old_form"], 1, "{old}");
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(stored["old-worker"], serde_json::json!(old_worker));

    // ── Run the commands it names; the status reads empty again, with no restart.
    let config_home = gonk.layout.grants_json();
    let config_home = config_home.parent().unwrap().parent().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["client", "refresh", "--config-home"])
        .arg(config_home)
        .arg("--write")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args([
            "ledger",
            "canonical-file-iris",
            "--ledger",
            "default",
            "--socket",
        ])
        .arg(&gonk.socket)
        .arg("--apply")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = status(&gonk);
    assert_eq!(after["grants"]["grants"], serde_json::json!([]), "{after}");
    assert_eq!(after["file_iris"], serde_json::json!([]), "{after}");
}
