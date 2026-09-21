//! ★ **A CHARACTERIZATION TEST OF A DEFECT THAT IS NOT THIS CRATE'S.** It is green
//! because the defect is present, and the day it goes RED is the day someone fixed
//! `ikigai-ipc` — at which point invert these assertions and move the file there.
//!
//! # What it reproduces: ledger #479
//!
//! `source urn:repo:ikigai-gonk:findings state=published as=application/json` came back
//! with NINE `ikigai-core` findings — a complete, well-formed, internally consistent
//! payload for a resource nobody asked for, with no error anywhere. It happened once and
//! resisted ~15 attempts. The saved payload is `tests/fixtures/leaked-findings-479.json`.
//!
//! Neither this crate nor `ikigai-browse` can produce it. The read is scoped by the `repo`
//! binding, which `ikigai-browse`'s `RootRow` grammar inserts as a CONSTANT per bound row,
//! and `annotate::list_findings` then keeps only records whose own `repo` field equals it —
//! so a row labelled `ikigai-core` can only come back from an invocation whose target WAS
//! `urn:repo:ikigai-core:findings`. `EndpointSpace::resolve` is a linear scan over an
//! immutable table, and the listing is `Expiry::Always`, so neither resolution nor the
//! kernel cache can substitute one root for another. The substitution is on the WIRE.
//!
//! # The mechanism, in one line
//!
//! `ikigai-ipc` is a strict request/reply protocol over ONE stream with **no correlation
//! id** — replies are matched to calls positionally. On a read deadline
//! (`is_dead_connection` deliberately excludes `TimedOut`/`WouldBlock`, so the connection
//! is KEPT) the client abandons a reply that the server is still going to write. The next
//! call on that resolver reads it. From then on every answer is the answer to the previous
//! question, silently and permanently.
//!
//! ⚠ `ikigai-quic` is immune: its `Wire::attempt` opens a fresh bidirectional stream per
//! call, so an abandoned reply dies with its stream.
//!
//! ⚠ A `Retry` overlay over an `IpcResolver` makes this WORSE, not better: the retry
//! consumes the abandoned reply, looks like a clean success, and leaves the shift in place.
//!
//! # What this does NOT prove
//!
//! That this is what happened on 2026-09-20. It needs a resolver that is SHARED across
//! calls — a `--connect` REPL session, a daemon's or `ikigai mcp`'s standing mount — that
//! had previously read `urn:repo:ikigai-core:findings`. A one-shot `ikigai -c` makes only
//! an engine `Meta` probe before its `Source`, and a shift there delivers a CONTRACT (the
//! second test below shows exactly that), which is not what #479 saw.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    Description, Endpoint, EndpointSpace, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Result as KResult, Verb,
};
use ikigai_resolve::Resolver;
use ikigai_vocab::TurtleRenderer;

/// The client's I/O deadline. Generous enough that an instant round trip on a loaded
/// runner is never mistaken for a miss.
const DEADLINE: Duration = Duration::from_millis(500);
/// How long the slow endpoint takes — well past [`DEADLINE`], so the miss is not a guess.
const SLOW: Duration = Duration::from_millis(2500);

/// `urn:test:{name}` answers its own name, after `delay`.
fn space(rows: &[(&str, u64)]) -> EndpointSpace {
    let mut space = EndpointSpace::new();
    for (name, delay) in rows {
        let label = (*name).to_string();
        let delay = *delay;
        space = space.bind(
            Exact::new(format!("urn:test:{name}")),
            FnEndpoint::new(format!("e-{name}"), move |_inv| {
                std::thread::sleep(Duration::from_millis(delay));
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    label.clone(),
                ))
            }),
        );
    }
    space
}

/// An endpoint whose SELF-DESCRIPTION is slow — what a peer that federates its own mounts
/// looks like from outside. That is not hypothetical here: this machine's `config.toml`
/// records gonk's manifold coming back degraded at the 30s describe bound because gonk
/// pays its own 30s against its LLM peer inside the same call.
struct SlowDescribe {
    name: String,
    delay: Duration,
}

#[async_trait]
impl Endpoint for SlowDescribe {
    async fn invoke(&self, _inv: &Invocation<'_>) -> KResult<Representation> {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            self.name.clone(),
        ))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn describe(&self) -> Description {
        std::thread::sleep(self.delay);
        Description::new(self.name.clone())
            .verb(Verb::Source)
            .verb(Verb::Meta)
    }
}

/// A socket path short enough for `sun_path` (104 bytes on macOS) — the session scratchpad
/// is not, and the failure surfaces at `bind` as a permission error rather than a length.
fn socket(tag: &str) -> PathBuf {
    let dir = PathBuf::from(std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string()));
    dir.join(format!("i479{tag}{}.s", std::process::id()))
}

fn client_for(path: &std::path::Path, deadline: Duration) -> ikigai_ipc::IpcResolver {
    for _ in 0..300 {
        if let Ok(c) = ikigai_ipc::connect_with_timeout(path, Some(deadline)) {
            return c;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the test server never came up at {}", path.display());
}

fn read(client: &ikigai_ipc::IpcResolver, name: &str) -> Result<String, String> {
    client
        .issue(Request::new(
            Verb::Source,
            Iri::parse(format!("urn:test:{name}")).expect("a valid test IRI"),
        ))
        .map(|(r, _)| String::from_utf8_lossy(&r.bytes).to_string())
        .map_err(|e| format!("{e:?}"))
}

/// ★ Ledger #479's exact shape: A is fine, B misses its deadline, and then the read of C
/// comes back with **B's** complete answer — and the shift never goes away.
///
/// The preconditions are checked rather than assumed: if a loaded runner made the first
/// call slow or the second call fast, this says so and stops instead of flaking.
#[test]
fn a_timed_out_ipc_call_poisons_the_connection() {
    let path = socket("a");
    let _ = std::fs::remove_file(&path);
    let serving = path.clone();
    std::thread::spawn(move || {
        let kernel = Kernel::new(Arc::new(space(&[
            ("a", 0),
            ("b", SLOW.as_millis() as u64),
            ("c", 0),
        ])));
        let _ = ikigai_ipc::serve(kernel, &serving);
    });
    let client = client_for(&path, DEADLINE);

    let first = read(&client, "a");
    let second = read(&client, "b");
    // The server finishes B and writes its reply into a socket nobody is reading.
    std::thread::sleep(SLOW + DEADLINE);
    let third = read(&client, "c");
    let fourth = read(&client, "a");
    let _ = std::fs::remove_file(&path);

    if first.as_deref() != Ok("a") || second.is_ok() {
        println!(
            "preconditions not met on this runner (a -> {first:?}, b -> {second:?}); \
             nothing was measured"
        );
        return;
    }
    assert_eq!(
        third.as_deref(),
        Ok("b"),
        "a Source of urn:test:c should have come back with the PREVIOUS call's answer"
    );
    assert_eq!(
        fourth.as_deref(),
        Ok("c"),
        "and the shift should still be there one call later"
    );
}

/// The same defect reached through the SELF-DESCRIPTION deadline, which is the bound that
/// actually fires in this fleet (30s by default, against a peer that federates). Here the
/// next Source comes back with the endpoint's Turtle CONTRACT as if it were the resource's
/// content — the one-shot `ikigai -c` version of the failure, and the reason a one-shot
/// cannot be how #479 happened.
#[test]
fn a_timed_out_describe_poisons_the_connection_too() {
    let path = socket("m");
    let _ = std::fs::remove_file(&path);
    let serving = path.clone();
    std::thread::spawn(move || {
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:test:a"),
                FnEndpoint::new("e-a", |_inv| {
                    Ok(Representation::new(ReprType::new("text/plain"), "a"))
                }),
            )
            .bind(
                Exact::new("urn:test:b"),
                SlowDescribe {
                    name: "e-b".to_string(),
                    delay: SLOW,
                },
            );
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
        let _ = ikigai_ipc::serve(kernel, &serving);
    });
    // The connection's own deadline is left long, so only the DESCRIBE bound can fire.
    let client = client_for(&path, Duration::from_secs(60)).with_describe_timeout(Some(DEADLINE));

    let meta = client.issue(Request::new(
        Verb::Meta,
        Iri::parse("urn:test:b").expect("a valid test IRI"),
    ));
    std::thread::sleep(SLOW + DEADLINE);
    let after = read(&client, "a");
    let _ = std::fs::remove_file(&path);

    if meta.is_ok() {
        println!("the describe did not miss its deadline on this runner; nothing was measured");
        return;
    }
    let after = after.expect("the poisoned read still answers — that is the whole problem");
    assert!(
        after.contains("urn:ikigai:endpoint:e-b"),
        "a Source of urn:test:a should have come back with the abandoned CONTRACT, got: {after}"
    );
}

/// The incident's own payload, kept because it is the only copy: it was saved to a session
/// scratchpad that temp cleanup will take. What it pins is the part that makes this a
/// read-confinement question rather than a mix-up — every row is a *valid* `ikigai-core`
/// finding, correctly labelled, in the state that was asked for. Nothing in the answer is
/// malformed, so nothing downstream can notice.
#[test]
fn the_leaked_payload_is_entirely_another_repos_published_findings() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/leaked-findings-479.json"
    ))
    .expect("the preserved payload");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("a JSON array");

    assert_eq!(rows.len(), 9, "the incident's nine rows");
    for row in &rows {
        assert_eq!(
            row["repo"].as_str(),
            Some("ikigai-core"),
            "asked for ikigai-gonk, answered about"
        );
        assert!(
            row["annotates"]
                .as_str()
                .is_some_and(|iri| iri.starts_with("urn:repo:ikigai-core:file:")),
            "and its anchor names the same other repo: {}",
            row["annotates"]
        );
        assert_eq!(
            row["state"].as_str(),
            Some("published"),
            "in exactly the state the caller asked for, which is why it looks right"
        );
    }
}
