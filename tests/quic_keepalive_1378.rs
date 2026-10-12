//! Ledger [#1378](http://localhost:1062/l/default/item/1378): gonk's QUIC door keeps answering a
//! client's keep-alive pings while a call resolves, so a call that stays silent past the
//! client's liveness bound still answers.
//!
//! From `ikigai-quic` 0.1.56 a client pings every third of its liveness bound
//! (`KEEP_ALIVE_INTERVAL`, 5 s) and declares the connection dead after the bound
//! (`LIVENESS_TIMEOUT`, 15 s) of silence. Through 0.1.55 the door resolved each call on the
//! runtime worker that also held the connection's driver, so while a call resolved no packet was
//! read and no ACK sent, and the client gave the connection up: `connection lost`, for any call
//! silent longer than the bound. A model explaining a file is such a call. 0.1.56 resolves off
//! the worker and the driver keeps answering.
//!
//! The door here is gonk's own ([`doors::quic_kernel_with`], served by
//! `ikigai_quic::serve_bounded_on` with the default idle timeout, as `main` serves it); only the
//! hub behind it is a test kernel whose one endpoint sleeps. Scaled down as `ikigai-quic`'s own
//! test is: a one-second liveness bound (`connect_with`'s `idle` caps it) and three seconds of
//! silent work, inside a ten-second call deadline. Everything is in process on `127.0.0.1:0`;
//! nothing touches a live service.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{
    Capability, EndpointSpace, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use ikigai_gonk::doors;
use ikigai_resolve::Resolver;
use ikigai_vocab::TurtleRenderer;

/// How long the endpoint behind the door stays silent: three times the liveness bound.
const SILENCE: Duration = Duration::from_secs(3);
/// The client's liveness bound, scaled down from the 15 s a host runs with.
const LIVENESS: Duration = Duration::from_secs(1);

/// gonk's QUIC door over a hub whose `urn:test:slow` sleeps [`SILENCE`], and the client identity
/// it trusts. Returns the door's address, the client identity and the server's certificate.
fn door_over_a_slow_hub() -> (std::net::SocketAddr, ikigai_quic::Identity, String) {
    let slow = FnEndpoint::new("slow", |_: &Invocation<'_>| {
        std::thread::sleep(SILENCE);
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"slow".to_vec(),
        ))
    });
    let hub = Arc::new(Kernel::with_meta_renderer(
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:test:slow"), slow)),
        Arc::new(TurtleRenderer),
    ));
    let door = doors::quic_kernel_with(hub, None);
    let (server, client) = (ikigai_quic::generate(), ikigai_quic::generate());
    let server_pem = server.cert_pem.clone();
    let trusted = vec![client.cert_pem.clone()];
    let minter: ikigai_quic::Minter = Arc::new(|_| {
        Some(ikigai_quic::Session {
            capability: Capability::root(),
            file_segment: "gonk".to_string(),
            principal: None,
        })
    });
    // Bound here and handed to the door, so the port is the one the kernel chose (field guide 9h).
    let udp = ikigai_quic::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = udp.local_addr().unwrap();
    std::thread::spawn(move || {
        ikigai_quic::serve_bounded_on(
            door,
            udp,
            &server,
            &trusted,
            minter,
            ikigai_quic::DEFAULT_IDLE_TIMEOUT,
            None,
        )
    });
    (addr, client, server_pem)
}

/// ★ The FLOOR TEST for `ikigai-quic = "0.1.56"`: a call silent for three liveness bounds
/// answers through gonk's door. On 0.1.55 it fails with `connection lost` after about a second.
#[test]
fn a_call_silent_past_the_liveness_bound_answers_through_the_door() {
    let (addr, client, server_pem) = door_over_a_slow_hub();
    let resolver = ikigai_quic::connect_with(addr, &client, &server_pem, LIVENESS)
        .expect("the door accepts a trusted client")
        .with_call_timeout(Some(Duration::from_secs(10)));
    let start = Instant::now();
    let (answer, _) = resolver
        .issue(Request::new(
            Verb::Source,
            Iri::parse("urn:test:slow").unwrap(),
        ))
        .expect("the door answered the client's pings while the call resolved");
    assert_eq!(answer.bytes, b"slow");
    assert!(start.elapsed() >= SILENCE, "{:?}", start.elapsed());
}

/// The numbers the test above is scaled from, so a change to them is read here first: a client
/// at the default `quic.timeout` declares a peer dead after 15 s and pings every 5 s.
#[test]
fn the_client_liveness_bound_is_fifteen_seconds_with_a_ping_every_five() {
    assert_eq!(ikigai_quic::LIVENESS_TIMEOUT, Duration::from_secs(15));
    assert_eq!(ikigai_quic::KEEP_ALIVE_INTERVAL, Duration::from_secs(5));
}
