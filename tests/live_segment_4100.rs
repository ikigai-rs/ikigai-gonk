//! Ledger #4100: the live (open, unsealed) log segment reads as a graph through the owner's
//! socket. gonk writes a request's door line when the answer is returned, before a streamed
//! body is read, so that read appended to the very file it was reading, and `ikigai-log`'s
//! version pin refused it: `conflict: log segment changed while reading its body`, every time.

mod scratch_gonk;

use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).unwrap());
    for (k, v) in args {
        request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
    }
    request
}

fn over_socket(gonk: &Gonk, request: Request) -> Result<Representation, Error> {
    let client =
        ikigai_ipc::connect(&gonk.socket).map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
    client.issue(request).map(|(repr, _)| repr)
}

/// The IRI of the segment gonk is writing now: the one in the listing with no seal yet.
fn live_segment(gonk: &Gonk) -> String {
    let listing = over_socket(gonk, request(Verb::Source, "urn:log:segments", &[]))
        .expect("the owner lists the segments");
    let listing = String::from_utf8_lossy(&listing.bytes).into_owned();
    let mut iris: Vec<String> = listing
        .split(|c: char| c.is_whitespace() || c == '<' || c == '>')
        .filter(|w| w.starts_with("urn:log:gonk:"))
        .map(str::to_string)
        .collect();
    iris.sort();
    iris.dedup();
    iris.pop()
        .unwrap_or_else(|| panic!("no gonk segment: {listing}"))
}

#[test]
fn the_live_segment_reads_as_a_graph_over_the_socket() {
    let gonk = Gonk::start("");
    // Something in the segment to read.
    for _ in 0..5 {
        over_socket(&gonk, request(Verb::Source, "urn:iki:ledger:items", &[]))
            .expect("the owner reads the items");
    }
    let live = live_segment(&gonk);
    for attempt in 0..3 {
        let graph = over_socket(&gonk, request(Verb::Source, &live, &[]))
            .unwrap_or_else(|e| panic!("attempt {attempt}: reading <{live}> as a graph: {e}"));
        let turtle = String::from_utf8_lossy(&graph.bytes).into_owned();
        assert!(turtle.contains("urn:iki:ledger:items"), "{turtle}");
    }
    // The raw face is the same file and reads the same way.
    let raw = over_socket(
        &gonk,
        request(Verb::Source, &live, &[("as", "text/x-ikigai-log")]),
    )
    .unwrap_or_else(|e| panic!("reading <{live}> raw: {e}"));
    assert!(String::from_utf8_lossy(&raw.bytes).contains(" log:Access urn:iki:ledger:items "));
}

#[test]
fn a_sealed_segment_still_reads_as_a_graph_after_a_restart() {
    let mut gonk = Gonk::start("");
    over_socket(&gonk, request(Verb::Source, "urn:iki:ledger:items", &[]))
        .expect("the owner reads the items");
    let before = live_segment(&gonk);
    // An orderly stop seals the segment (`access::Record::close`).
    let killed = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(gonk.child.id().to_string())
        .status()
        .expect("run kill");
    assert!(killed.success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while gonk.child.try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "no exit:\n{}",
            gonk.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    gonk.restart();
    let after = live_segment(&gonk);
    assert_ne!(before, after, "a restart opens a new segment");
    // The sealed one streams under ikigai-log's version pin, as before #4100.
    let graph = over_socket(&gonk, request(Verb::Source, &before, &[]))
        .unwrap_or_else(|e| panic!("reading the sealed <{before}>: {e}"));
    assert!(String::from_utf8_lossy(&graph.bytes).contains("urn:iki:ledger:items"));
}
