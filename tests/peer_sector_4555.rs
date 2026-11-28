//! Ledger #4555, task 3: every configured peer (a `gonk.mount` line) has a stable sector IRI,
//! `urn:iki:gonk:sector:name:{name}`, named by the line's `sector=` word or defaulted from the
//! peer's configured name, and every call the mount sends that peer writes a
//! `urn:iki:gonk:class:PeerCall` entry naming it.
//!
//! The peer is a stub kernel served over `ikigai_ipc` on a short socket under `/tmp`, mounted
//! exactly as `gonk.mount` mounts a real one. Nothing here touches a live gonk.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    Capability, Description, Endpoint, EndpointSpace, Exact, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use ikigai_gonk::{access, compose_with, config, mount};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;

mod ready;

/// A scratch `$HOME` holding one peer's certificate directory under `quic/peers/{name}`, the
/// layout the README and `docs/fresh-machine.md` set up. `parse` checks only that it is a
/// directory; the files are written so a rotation has something to replace.
fn peer_dir(home: &Path, name: &str) -> PathBuf {
    let dir = home.join(".config/ikigai/gonk/quic/peers").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    rotate(&dir, "first");
    dir
}

/// A certificate rotation, as far as the configuration can see one: every file in the
/// directory replaced, the directory and the mount line unchanged.
fn rotate(dir: &Path, generation: &str) {
    for file in ["client.crt", "client.key", "server.crt"] {
        std::fs::write(dir.join(file), format!("{generation} {file}\n")).unwrap();
    }
}

fn homes(home: &Path) -> config::Homes {
    config::Homes {
        home: home.to_path_buf(),
        config: home.join(".config/ikigai"),
        data: home.join(".ikigai"),
    }
}

/// The sectors of every mount `text` configures, as a start of gonk reads them.
fn sectors(text: &str, home: &Path) -> Vec<(String, String)> {
    config::settings(&config::Flags::default(), text, &homes(home))
        .unwrap()
        .mounts
        .into_iter()
        .map(|mount| (mount.prefix, mount.sector))
        .collect()
}

#[test]
fn a_configured_peer_has_a_stable_sector_iri() {
    let home = tempfile::tempdir().unwrap();
    peer_dir(home.path(), "plasma");
    let text = "gonk.mount = \"prefer urn:llm:=quic://127.0.0.1:4433 \
                ~/.config/ikigai/gonk/quic/peers/plasma\"\n";
    let first = sectors(text, home.path());
    assert_eq!(
        first,
        [(
            "urn:llm:".to_string(),
            "urn:iki:gonk:sector:name:plasma".to_string()
        )]
    );
    // Every start reads the same configuration, so every start gets the same sector.
    for _ in 0..3 {
        assert_eq!(sectors(text, home.path()), first);
    }
    // A `sector=` word names it instead, as a name or as a whole IRI.
    let named = format!(
        "{}{}",
        text.trim_end().trim_end_matches('"'),
        " sector=model\"\n"
    );
    assert_eq!(
        sectors(&named, home.path())[0].1,
        "urn:iki:gonk:sector:name:model"
    );
    let iri = format!(
        "{}{}",
        text.trim_end().trim_end_matches('"'),
        " sector=urn:x-example:sector:name:lab\"\n"
    );
    assert_eq!(
        sectors(&iri, home.path())[0].1,
        "urn:x-example:sector:name:lab"
    );
}

#[test]
fn the_sector_survives_a_certificate_rotation() {
    let home = tempfile::tempdir().unwrap();
    let dir = peer_dir(home.path(), "plasma");
    let line = format!("prefer urn:llm:=quic://127.0.0.1:4433 {}", dir.display());
    let before = mount::parse(&line, home.path()).unwrap();
    let fingerprint_before = std::fs::read(dir.join("client.crt")).unwrap();

    rotate(&dir, "rotated");
    assert_ne!(
        std::fs::read(dir.join("client.crt")).unwrap(),
        fingerprint_before,
        "the rotation replaced the certificate"
    );
    let after = mount::parse(&line, home.path()).unwrap();
    assert_eq!(after.sector, before.sector);
    assert_eq!(after.sector, "urn:iki:gonk:sector:name:plasma");
}

#[test]
fn two_peers_get_distinct_sectors() {
    let home = tempfile::tempdir().unwrap();
    peer_dir(home.path(), "plasma");
    peer_dir(home.path(), "build");
    let text = "gonk.mount = \"prefer urn:llm:=quic://127.0.0.1:4433 \
                ~/.config/ikigai/gonk/quic/peers/plasma\"\n\
                gonk.mount = \"prefer urn:x-proj:classify:=quic://127.0.0.1:4434 \
                ~/.config/ikigai/gonk/quic/peers/build\"\n\
                gonk.mount = \"prefer urn:dashboard:=~/.ikigai/dashboard.sock\"\n";
    let got = sectors(text, home.path());
    let mut by_prefix: Vec<(&str, &str)> = got
        .iter()
        .map(|(prefix, sector)| (prefix.as_str(), sector.as_str()))
        .collect();
    by_prefix.sort();
    assert_eq!(
        by_prefix,
        [
            ("urn:dashboard:", "urn:iki:gonk:sector:name:dashboard"),
            ("urn:llm:", "urn:iki:gonk:sector:name:plasma"),
            ("urn:x-proj:classify:", "urn:iki:gonk:sector:name:build"),
        ]
    );
}

/// An older configuration: the README's own `gonk.mount` lines, with no `sector=` word, load
/// unchanged and get the default. Its refusals are unchanged too: a stray fourth word is still
/// refused, and so is a `sector=` that names nothing.
#[test]
fn an_older_configuration_with_no_sector_loads_and_gets_the_default() {
    let home = tempfile::tempdir().unwrap();
    peer_dir(home.path(), "plasma");
    let older = "gonk.mount = \"prefer urn:llm:=quic://127.0.0.1:4433 \
                 ~/.config/ikigai/gonk/quic/peers/plasma\"\n\
                 gonk.mount = \"prefer urn:dashboard:=~/.ikigai/dashboard.sock\"\n";
    let settings = config::settings(&config::Flags::default(), older, &homes(home.path()))
        .expect("an older configuration still loads");
    for m in &settings.mounts {
        assert_eq!(m.sector, mount::default_sector(&m.target), "{}", m.prefix);
    }

    let refused = |line: &str| mount::parse(line, home.path()).unwrap_err();
    let socket = "prefer urn:llm:=/tmp/llm.sock";
    assert!(refused(&format!("{socket} extra")).contains("a socket target takes no"));
    assert!(refused(&format!("{socket} sector=")).contains("must be a name"));
    assert!(refused(&format!("{socket} sector=a sector=b")).contains("twice"));
}

/// A stub `urn:llm:ask` that answers `ok`.
struct Stub;

#[async_trait]
impl Endpoint for Stub {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"ok".to_vec(),
        ))
    }
    fn name(&self) -> &str {
        "ask"
    }
    fn describe(&self) -> Description {
        Description::new("ask").verb(Verb::Source)
    }
}

/// Every call to the peer writes one `PeerCall` entry naming its sector beside the peer, the
/// outcome and the caller; a call to a peer that is down writes one too.
#[test]
fn every_call_to_the_peer_writes_its_sector_on_the_audit_line() {
    let dir = tempfile::Builder::new()
        .prefix("s4555")
        .tempdir_in("/tmp")
        .expect("a short socket directory");
    let socket = dir.path().join("model.sock");
    let space = EndpointSpace::new().bind(Exact::new("urn:llm:ask"), Stub);
    let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
    let path = socket.clone();
    std::thread::spawn(move || ikigai_ipc::serve(kernel, &path));
    ready::socket(&socket);

    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&lines);
    let record = access::Record::to(Arc::new(move |line: &str| {
        sink.lock().unwrap().push(line.to_string())
    }));
    let home = Path::new("/home/nobody");
    let up = mount::parse(&format!("prefer urn:llm:={}", socket.display()), home).unwrap();
    let down = mount::parse(
        &format!(
            "prefer urn:x-down:={} sector=gone",
            dir.path().join("none.sock").display()
        ),
        home,
    )
    .unwrap();
    let calls = ikigai_gonk::budget::model_calls(10);
    let hub = compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        vec![
            mount::space_recorded(&up, &calls, Some(Arc::clone(&record))),
            mount::space_recorded(&down, &calls, Some(Arc::clone(&record))),
        ],
        Vec::new(),
        None,
        None,
        None,
    );
    let caller = Capability::scoped(["urn:cap:net:localhost".to_string()])
        .with_principal("urn:iki:host:gonk:client:ab12")
        .unwrap();
    let ask = |iri: &str| {
        block_on(hub.issue(
            Request::new(Verb::Source, Iri::parse(iri).unwrap()),
            &caller,
        ))
    };
    assert_eq!(ask("urn:llm:ask").unwrap().bytes, b"ok");
    assert_eq!(ask("urn:llm:ask").unwrap().bytes, b"ok");
    assert!(ask("urn:x-down:thing").is_err(), "nothing listens there");

    let lines = lines.lock().unwrap().clone();
    let calls: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("PeerCall"))
        .collect();
    // The mount asks the peer for the name's contract (a `Meta`, under the mount's own
    // authority) before the first call: a call to the peer like any other, and on the record.
    let to_up = |verb: &str| -> Vec<&&String> {
        calls
            .iter()
            .filter(|line| line.contains(" urn:llm:ask ") && line.contains(verb))
            .collect()
    };
    assert!(
        to_up("verb=meta")
            .iter()
            .all(|line| line.contains("sector=urn:iki:gonk:sector:name:model")),
        "{lines:#?}"
    );
    let to_up = to_up("verb=source");
    assert_eq!(to_up.len(), 2, "one line per call: {lines:#?}");
    for line in &to_up {
        for column in [
            "sector=urn:iki:gonk:sector:name:model",
            &format!("peer={}", socket.display()),
            "verb=source",
            "outcome=ok",
            "principal=urn:iki:host:gonk:client:ab12",
        ] {
            assert!(line.contains(column), "`{column}` missing from {line}");
        }
    }
    let to_down: Vec<&&String> = calls
        .iter()
        .filter(|line| line.contains(" urn:x-down:thing ") && line.contains("verb=source"))
        .collect();
    assert_eq!(to_down.len(), 1, "{lines:#?}");
    assert!(
        to_down[0].contains("sector=urn:iki:gonk:sector:name:gone")
            && to_down[0].contains("outcome=unavailable"),
        "{}",
        to_down[0]
    );
}
