//! Ledger [#1138](http://localhost:1060/l/default/item/1138): gonk mounts `ikigai-nl`, so a person
//! drafts a query from a sentence, the mounted model drafting under the person's own grant, and
//! the draft is saved as a `state=draft` script that only its author sees until they publish it.
//!
//! Every model here is a STUB `urn:llm:` peer: a kernel served over `ikigai_ipc` on a short socket
//! under `/tmp`, mounted exactly as `gonk.mount` mounts the real one (so the mount's own net-host
//! check is in the path), answering every ask deterministically from the ask's own words. It binds
//! what `ikigai-llm` binds that matters here: `urn:llm:ask` (declaring `urn:cap:net:*`, as
//! `ikigai-llm` does) and `urn:llm:config` (declaring nothing, as `ikigai-llm` does).
//!
//! The first half runs against the hub in process (the grounding's size on a ledger the size of
//! the live one, and what each caller is offered); the second against a REAL gonk, the binary in
//! a child process over a scratch config home, data home, store and socket, on a port the child
//! picks, through its HTTP, QUIC and socket doors. Nothing here touches a live gonk.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Verb,
};
use ikigai_gonk::grants::{
    anonymous_http_grants, grants_for, nl_draft_grants, script_grants, Authority, ScriptAuthority,
};
use ikigai_gonk::nl::Nl;
use ikigai_gonk::script::Scripts;
use ikigai_gonk::{compose_with, mount};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use tempfile::TempDir;

mod common;
mod ready;
mod scratch_gonk;

// ------------------------------------------------------------------ the stub model

/// What the stub drafts when the ask is answerable: the titles of the default ledger's items.
const GOOD: &str = "\
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>
SELECT ?number ?title
FROM <urn:iki:ledger:graph:default>
WHERE { ?item ledger:number ?number ; dcterms:title ?title . }
ORDER BY ?number";

/// The ask (between the prompt's `The ask:` line and the grounding that follows it).
fn ask_of(prompt: &str) -> &str {
    prompt
        .split_once("The ask:\n")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split("\n\nWhat this person may read").next())
        .unwrap_or("")
}

/// Answers by the ask's own words, deterministically:
///
/// - `TITLES`: [`GOOD`], first time.
/// - `FIXME`: a query naming a predicate no graph uses, until the prompt carries the repair
///   section (the checker's refusal of it), then [`GOOD`].
/// - `HOPELESS`: a different unparsable text on every call, so the drafter never converges and
///   never sees the same draft twice: only the bound stops it.
struct StubAsk {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Endpoint for StubAsk {
    async fn invoke(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let prompt = match inv.request.args.get("prompt") {
            Some(ArgRef::Inline(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
            _ => String::new(),
        };
        let ask = ask_of(&prompt);
        let repairing = prompt.contains("Your previous draft was:");
        let query = if ask.contains("HOPELESS") {
            format!("SELECT ?x{n} WHERE {{ ?x{n} ?p }}")
        } else if ask.contains("FIXME") && !repairing {
            "SELECT ?item FROM <urn:iki:ledger:graph:default> \
             WHERE { ?item <https://ikigai-rs.dev/ns/ledger#invented> ?v }"
                .to_string()
        } else {
            GOOD.to_string()
        };
        let body = serde_json::json!({
            "text": format!("Here it is:\n```sparql\n{query}\n```\n"),
            "model": "stub-sparql",
        });
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(&body).unwrap(),
        ))
    }
    fn name(&self) -> &str {
        "stub-ask"
    }
    fn describe(&self) -> Description {
        let optional = |name: &str| ArgSpec::new(name).optional();
        Description::new("stub-ask")
            .verb(Verb::Source)
            .requires("urn:cap:net:*")
            .input(optional("prompt"))
            .input(optional("as"))
            .input(optional("temperature"))
            .input(optional("max_tokens"))
    }
}

/// `urn:llm:config`: capability-free, as `ikigai-llm`'s is.
struct StubConfig;

#[async_trait]
impl Endpoint for StubConfig {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            br#"{"default":"stub","providers":{}}"#.to_vec(),
        )
        .cacheable())
    }
    fn name(&self) -> &str {
        "stub-config"
    }
    fn describe(&self) -> Description {
        Description::new("stub-config").verb(Verb::Source)
    }
}

/// The stub peer, served on a socket under a short `/tmp` directory (a scratchpad path is too
/// long for `sun_path`).
struct Peer {
    calls: Arc<AtomicUsize>,
    socket: PathBuf,
    _dir: TempDir,
}

impl Peer {
    fn start() -> Peer {
        let dir = tempfile::Builder::new()
            .prefix("n1")
            .tempdir_in("/tmp")
            .expect("a short socket directory");
        let socket = dir.path().join("l.sock");
        let calls = Arc::new(AtomicUsize::new(0));
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:llm:ask"),
                StubAsk {
                    calls: Arc::clone(&calls),
                },
            )
            .bind(Exact::new("urn:llm:config"), StubConfig);
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
        let path = socket.clone();
        std::thread::spawn(move || ikigai_ipc::serve(kernel, &path));
        ready::socket(&socket);
        Peer {
            calls,
            socket,
            _dir: dir,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The `gonk.mount` line that reaches it.
    fn line(&self) -> String {
        format!("prefer urn:llm:={}", self.socket.display())
    }
}

// ------------------------------------------------------------------ in process: the hub

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn issue(
    kernel: &Kernel,
    capability: &Capability,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
) -> Result<Representation, Error> {
    block_on(kernel.issue(request(verb, iri, args), capability))
}

fn text(repr: &Representation) -> String {
    String::from_utf8_lossy(&repr.bytes).into_owned()
}

/// A passkey's principal, as the HTTP door mints it.
const PERSON: &str = "urn:iki:gonk:passkey:QlJJQU4";

/// The hub as `main` composes it for this arc: the store, the ledger, the scripts, the stub
/// mount in front, and the natural-language family.
fn hub(peer: &Peer) -> Arc<Kernel> {
    let scripts = Scripts::in_memory();
    let mount = mount::parse(&peer.line(), std::path::Path::new("/home/nobody")).unwrap();
    let host = mount.target.net_host();
    let nl = Nl::new(
        ikigai_gonk::nl::DEFAULT_ASK,
        Some(&host),
        Default::default(),
    );
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        vec![mount::space(
            &mount,
            &ikigai_gonk::budget::model_calls(ikigai_gonk::budget::DEFAULT_MODEL_CALLS_PER_HOUR),
        )],
        Vec::new(),
        None,
        Some(&scripts),
        Some(&nl),
    ));
    scripts.attach(&hub);
    hub
}

/// A person who writes the default ledger, may draft, and may publish `titles`.
fn person() -> Capability {
    let mut scopes = grants_for("default", Authority::Write).unwrap();
    scopes.extend(nl_draft_grants(Some("localhost")).unwrap());
    scopes.extend(script_grants("titles", ScriptAuthority::Publish).unwrap());
    Capability::scoped(scopes).with_principal(PERSON).unwrap()
}

/// The anonymous loopback caller of the HTTP door.
fn anonymous() -> Capability {
    Capability::scoped(anonymous_http_grants(&["default".to_string()]).unwrap())
}

fn json(repr: &Representation) -> serde_json::Value {
    serde_json::from_slice(&repr.bytes).unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(repr)))
}

/// ★ Who is offered what. The drafter is offered to exactly the callers holding the mount's
/// net grant, and refused before any model is asked to everyone else; the grounding and the
/// check, which spend nothing, are offered to every caller and composed under each one's own
/// grant.
#[test]
fn the_drafter_needs_the_mounts_net_grant_and_the_rest_needs_nothing_new() {
    let peer = Peer::start();
    let hub = hub(&peer);
    let offered = |who: &Capability, verb: &str| {
        text(
            &issue(
                &hub,
                who,
                Verb::Source,
                "urn:kernel:actions",
                &[("verb", verb)],
            )
            .unwrap(),
        )
    };
    for (who, name) in [(anonymous(), "anonymous"), (person(), "person")] {
        let sources = offered(&who, "Source");
        assert!(sources.contains("urn:nl:grounding"), "{name}: {sources}");
        assert!(sources.contains("urn:nl:sparql:check"), "{name}: {sources}");
    }
    assert!(offered(&person(), "Sink").contains("urn:nl:sparql\n"));
    assert!(
        !offered(&anonymous(), "Sink").contains("urn:nl:sparql\n"),
        "the manifold offers the drafter to nobody it would refuse"
    );

    let before = peer.calls();
    match issue(
        &hub,
        &anonymous(),
        Verb::Sink,
        "urn:nl:sparql",
        &[("ask", "TITLES of the items")],
    ) {
        Err(Error::Denied(why)) => assert!(why.contains("urn:cap:net:localhost"), "{why}"),
        other => panic!("anonymous drafting: {other:?}"),
    }
    assert_eq!(peer.calls(), before, "refused before any model was asked");
}

/// ★ The grounding composes for every caller, the stub mount in front included, and describes
/// every offered action. (A mounted resource that declares nothing, `urn:llm:config`, used to be
/// offered to a caller the mount then refused; since ledger #1204 the mount declares its net
/// token on every resource, `tests/mount_offer_1204.rs`.)
#[test]
fn the_grounding_composes_for_every_caller_with_a_mount_in_front() {
    let peer = Peer::start();
    let hub = hub(&peer);
    issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "first")],
    )
    .unwrap();
    for (who, name) in [
        (anonymous(), "anonymous"),
        (person(), "person"),
        (Capability::root(), "root"),
    ] {
        let grounding = issue(
            &hub,
            &who,
            Verb::Source,
            "urn:nl:grounding",
            &[("as", "application/json")],
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        let g = json(&grounding);
        let graphs: Vec<&str> = g["graphs"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["graph"].as_str().unwrap())
            .collect();
        assert!(
            graphs.contains(&"urn:iki:ledger:graph:default"),
            "{name}: {graphs:?}"
        );
        let endpoints: Vec<&str> = g["actions"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|a| a["endpoint"].as_str())
            .collect();
        // The model door is in a grounding exactly when the caller may spend it.
        assert_eq!(
            endpoints.contains(&"urn:llm:ask"),
            name != "anonymous",
            "{name}: {endpoints:?}"
        );
        assert_eq!(
            endpoints.contains(&"urn:nl:sparql"),
            name != "anonymous",
            "{name}: {endpoints:?}"
        );
    }
}

/// ★ No mount, no drafter: a gonk with no `gonk.mount` binds the grounding, the check, the
/// prompts and the vocabulary, and not `urn:nl:sparql`, which would have no model to ask. And
/// the family adds exactly these names to the hub.
#[test]
fn without_a_mount_the_drafter_is_not_bound_and_the_rest_is() {
    let scripts = Scripts::in_memory();
    let hub = |nl: Option<&Nl>| {
        Arc::new(compose_with(
            DurableStore::in_memory().expect("an in-memory store"),
            None,
            Vec::new(),
            Vec::new(),
            None,
            Some(&scripts),
            nl,
        ))
    };
    let patterns = |kernel: &Kernel| -> std::collections::BTreeSet<String> {
        kernel
            .entries()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.pattern)
            .collect()
    };
    let without = patterns(&hub(None));
    let mounted = Nl::new(
        ikigai_gonk::nl::DEFAULT_ASK,
        Some("localhost"),
        Default::default(),
    );
    let unmounted = Nl::new(ikigai_gonk::nl::DEFAULT_ASK, None, Default::default());
    let added = |nl: &Nl| -> Vec<String> {
        patterns(&hub(Some(nl)))
            .difference(&without)
            .cloned()
            .collect()
    };
    assert_eq!(
        added(&mounted),
        [
            "urn:ikigai:vocab",
            "urn:nl:grounding",
            "urn:nl:prompt:{name}",
            "urn:nl:sparql",
            "urn:nl:sparql:check",
        ]
    );
    assert_eq!(
        added(&unmounted),
        [
            "urn:ikigai:vocab",
            "urn:nl:grounding",
            "urn:nl:prompt:{name}",
            "urn:nl:sparql:check",
        ]
    );
    let bare = hub(Some(&unmounted));
    match issue(
        &bare,
        &Capability::root(),
        Verb::Sink,
        "urn:nl:sparql",
        &[("ask", "anything")],
    ) {
        Err(Error::Unresolved(_)) => {}
        other => panic!("a drafter with no model: {other:?}"),
    }
    issue(
        &bare,
        &Capability::root(),
        Verb::Source,
        "urn:nl:grounding",
        &[("as", "application/json")],
    )
    .expect("the grounding needs no model");
}

/// The live ledger's size, near enough (ledger #1138 said ~730; ~750 by dispatch).
const LIVE_ITEMS: usize = 750;

/// ★ **The grounding's size bound, measured on a ledger the size of the live one, with browse
/// composed as the live gonk composes it.** `ikigai-nl`'s default bound is 1 MiB, refused (never
/// truncated) above it. The ledger graph's SHAPE is bounded by partitions and samples, not by its
/// item count, so the items move only the totals; what grows a grounding is the caller's
/// MANIFOLD (one action per door it may use) and the vocabulary. Every caller below fits, with
/// and without a focus, in both faces; the sizes are printed for the README.
#[test]
fn a_grounding_over_a_ledger_the_size_of_the_live_one_fits_the_bound() {
    let peer = Peer::start();
    let root_dir = tempfile::tempdir().expect("a browse root");
    std::fs::write(root_dir.path().join("README.md"), "# demo\n").unwrap();
    let graph = ikigai_gonk::browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let tiers = ikigai_gonk::config::ExplainTiers::default();
    let wired = ikigai_gonk::browse::wire(
        vec![("demo".to_string(), root_dir.path().to_path_buf())],
        store
            .shared_writer()
            .expect("a shared store hands out its writer"),
        None,
        Some(&tiers),
        &graph,
        None,
    );
    let scripts = Scripts::in_memory();
    let mount = mount::parse(&peer.line(), std::path::Path::new("/home/nobody")).unwrap();
    let nl = Nl::new(
        ikigai_gonk::nl::DEFAULT_ASK,
        Some("localhost"),
        Default::default(),
    );
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        vec![mount::space(
            &mount,
            &ikigai_gonk::budget::model_calls(ikigai_gonk::budget::DEFAULT_MODEL_CALLS_PER_HOUR),
        )],
        Vec::new(),
        None,
        Some(&scripts),
        Some(&nl),
    ));
    scripts.attach(&hub);

    let started = Instant::now();
    let root = Capability::root();
    for n in 0..LIVE_ITEMS {
        let content = format!("item {n}: something to do about the ledger, at some length");
        let labels = ["gonk", "p2", "design"][n % 3];
        issue(
            &hub,
            &root,
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", &content), ("labels", labels)],
        )
        .expect("append");
    }
    println!("seeded {LIVE_ITEMS} items in {:?}", started.elapsed());

    let mut browsing = person();
    let browse =
        ikigai_gonk::grants::browse_role_grants(ikigai_gonk::grants::BrowseRole::Read, &[], None)
            .unwrap();
    browsing = Capability::scoped(
        browsing
            .scopes()
            .unwrap()
            .iter()
            .cloned()
            .chain(browse)
            .filter(|s| !s.starts_with("urn:cap:principal:")),
    )
    .with_principal(PERSON)
    .unwrap();
    let bound = ikigai_nl::SpaceConfig::new().max_bytes;
    for (who, name) in [
        (anonymous(), "anonymous"),
        (person(), "person"),
        (browsing, "person+browse"),
        (root.clone(), "root"),
    ] {
        for focus in [None, Some("urn:iki:ledger:graph:default")] {
            for face in ["application/json", "text/turtle"] {
                let mut args = vec![("as", face)];
                if let Some(focus) = focus {
                    args.push(("focus", focus));
                }
                let cold = Instant::now();
                let answer = issue(&hub, &who, Verb::Source, "urn:nl:grounding", &args)
                    .unwrap_or_else(|e| panic!("{name} {focus:?} {face}: {e}"));
                let cold = cold.elapsed();
                let warm = Instant::now();
                issue(&hub, &who, Verb::Source, "urn:nl:grounding", &args).unwrap();
                let warm = warm.elapsed();
                println!(
                    "{name:14} focus={:32} {face:17} {:>7} bytes  cold {cold:?}  warm {warm:?}",
                    focus.unwrap_or("-"),
                    answer.bytes.len()
                );
                assert!(answer.bytes.len() <= bound, "{name} fits {bound}");
            }
        }
    }
    // The totals are the live ledger's: every item is counted, though the shape lists few.
    let g = json(
        &issue(
            &hub,
            &person(),
            Verb::Source,
            "urn:nl:grounding",
            &[
                ("as", "application/json"),
                ("focus", "urn:iki:ledger:graph:default"),
            ],
        )
        .unwrap(),
    );
    let ledger = g["graphs"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["graph"] == "urn:iki:ledger:graph:default")
        .expect("the ledger graph's shape");
    let subjects = ledger["subjects"].as_u64().unwrap();
    assert!(subjects >= LIVE_ITEMS as u64, "{ledger}");
    println!(
        "ledger graph: {} triples, {subjects} subjects, {} classes",
        ledger["triples"], ledger["classes"]
    );
}

/// The live gonk's browse root count on 2026-10-12 (`gonk.browse.root` lines in its config home).
const LIVE_ROOTS: usize = 51;

/// ★ Ledger [#1243](http://localhost:1060/l/default/item/1243): an UNFOCUSED draft as the owner,
/// with browse composed over as many roots as the live gonk serves, FITS. Through `ikigai-nl`
/// 0.2.x the drafter's grounding carried the caller's whole manifold, about 100 KB of browse
/// actions per root, so the live gonk refused every unfocused draft before any model call
/// ("the grounding with no focus is 4966451 bytes … over this host bound of 1048576"). 0.3.0's
/// drafter reads only the vocabulary, the graphs and the examples, so the root count no longer
/// moves it. Measured here before the move: on 0.2.1 this draft is that same refusal.
#[test]
fn an_unfocused_draft_over_the_live_root_count_fits_the_bound() {
    let peer = Peer::start();
    let roots_dir = tempfile::tempdir().expect("the browse roots");
    let roots: Vec<(String, std::path::PathBuf)> = (0..LIVE_ROOTS)
        .map(|n| {
            let dir = roots_dir.path().join(format!("r{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("README.md"), format!("# root {n}\n")).unwrap();
            (format!("repo{n}"), dir)
        })
        .collect();
    let graph = ikigai_gonk::browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let tiers = ikigai_gonk::config::ExplainTiers::default();
    let wired = ikigai_gonk::browse::wire(
        roots,
        store
            .shared_writer()
            .expect("a shared store hands out its writer"),
        None,
        Some(&tiers),
        &graph,
        None,
    );
    let scripts = Scripts::in_memory();
    let mount = mount::parse(&peer.line(), std::path::Path::new("/home/nobody")).unwrap();
    let nl = Nl::new(
        ikigai_gonk::nl::DEFAULT_ASK,
        Some("localhost"),
        Default::default(),
    );
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        vec![mount::space(
            &mount,
            &ikigai_gonk::budget::model_calls(ikigai_gonk::budget::DEFAULT_MODEL_CALLS_PER_HOUR),
        )],
        Vec::new(),
        None,
        Some(&scripts),
        Some(&nl),
    ));
    scripts.attach(&hub);
    issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "first item")],
    )
    .expect("append");

    let before = peer.calls();
    let drafted = issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:nl:sparql",
        &[
            ("content", "the TITLES of the ledger's items"),
            ("save", "titles"),
            ("as", "application/json"),
        ],
    )
    .unwrap_or_else(|e| panic!("an unfocused draft over {LIVE_ROOTS} roots: {e}"));
    let d = json(&drafted);
    assert_eq!(d["status"], "saved", "{d}");
    assert_eq!(peer.calls(), before + 1, "the model was asked once: {d}");

    // What the README says of the WHOLE grounding at this root count: a direct unfocused read
    // is still over the bound (the manifold part is what grows), and the drafter's own parts fit.
    let whole = issue(
        &hub,
        &Capability::root(),
        Verb::Source,
        "urn:nl:grounding",
        &[("as", "application/json")],
    );
    assert!(
        whole
            .as_ref()
            .is_err_and(|e| e.to_string().contains("over this host")),
        "the whole unfocused grounding over {LIVE_ROOTS} roots is refused at the bound: {:?}",
        whole.map(|r| r.bytes.len())
    );
    let drafting = issue(
        &hub,
        &Capability::root(),
        Verb::Source,
        "urn:nl:grounding",
        &[
            ("as", "application/json"),
            ("parts", ikigai_nl::DRAFTING_PARTS),
        ],
    )
    .expect("the drafter's parts of the grounding fit");
    println!(
        "{LIVE_ROOTS} roots: the drafter's grounding is {} bytes",
        drafting.bytes.len()
    );
}

// ------------------------------------------------------------- a real gonk: every door

use std::io::{Read, Write};
use std::net::TcpStream;

use common::Authenticator;
use ikigai_gonk::{identity, quic};
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

impl Gonk {
    fn origin(&self) -> String {
        format!("http://localhost:{}", self.http.port())
    }

    /// One HTTP exchange: `(status, body)`.
    fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(self.http).expect("connect");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n",
            self.http.port()
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ));
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let response = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    fn post_json(&self, path: &str, body: &str) -> (u16, String) {
        self.raw(
            "POST",
            path,
            &[
                ("Accept", "application/json".to_string()),
                ("Content-Type", "application/json".to_string()),
                ("Origin", self.origin()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
            ],
            body,
        )
    }

    /// Enroll a passkey from `authenticator` under the grant `grant` holding `scopes`, sign in,
    /// and return the session token.
    fn sign_in(&self, authenticator: &Authenticator, grant: &str, scopes: &[String]) -> String {
        let invite = identity::invite(
            &self.layout,
            grant,
            scopes,
            false,
            30,
            identity::now_seconds(),
        )
        .unwrap();
        let challenge = |op: &str| {
            let (status, body) = self.post_json(&format!("/auth/{op}"), "{}");
            assert_eq!(status, 200, "{body}");
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            v["challenge"].as_str().unwrap().to_string()
        };
        let c = challenge("register-options");
        let (status, body) = self.post_json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(status, 200, "{body}");
        let c = challenge("login-options");
        let (status, body) = self.post_json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        );
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["session"].as_str().unwrap().to_string()
    }

    /// The mechanical route over HTTP: `method path`, `body` as the content, signed in when
    /// `session` is given.
    fn http(&self, method: &str, path: &str, session: Option<&str>, body: &str) -> (u16, String) {
        let mut headers = vec![("Content-Type", "text/plain".to_string())];
        if let Some(token) = session {
            headers.push(("Cookie", format!("{}={token}", identity::SESSION_COOKIE)));
        }
        self.raw(method, path, &headers, body)
    }

    fn over_socket(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }

    fn over_quic(&self, request: Request) -> Result<Representation, Error> {
        let client = self
            .dial_quic()
            .unwrap_or_else(|e| panic!("QUIC connect: {e}"));
        client.issue(request).map(|(repr, _)| repr)
    }
}

/// A drafting over HTTP as `session`: `(status, the draft's JSON or the refusal)`.
fn draft(gonk: &Gonk, session: Option<&str>, ask: &str, save: &str) -> (u16, serde_json::Value) {
    let (status, body) = gonk.http(
        "POST",
        &format!("/nl/sparql?save={save}&as=application%2Fjson"),
        session,
        ask,
    );
    let value = serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body));
    (status, value)
}

/// ★ The arc's whole story through a REAL gonk with a stub model: a person drafts a query from a
/// sentence and the check passes; a bad first draft is repaired within the bound; a draft that
/// never validates is refused at the bound; the draft is its author's and nobody else's; publishing
/// it makes `:result` runnable; anonymous is refused before any model is asked; a QUIC machine
/// with the grant drafts too.
#[test]
fn a_person_drafts_a_query_from_a_sentence_and_publishes_it() {
    let peer = Peer::start();
    let gonk = Gonk::with_quic(&format!("gonk.mount = \"{}\"\n", peer.line()));
    assert!(
        gonk.log()
            .contains("urn:nl:sparql drafts through urn:llm:ask"),
        "the banner says who may draft: {}",
        gonk.log()
    );
    for (content, label) in [("first item", "triage"), ("second item", "other")] {
        gonk.over_socket(request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", content), ("labels", label)],
        ))
        .expect("append over the socket");
    }

    // A person who writes the default ledger, may draft, and may publish three names.
    let mut scopes = grants_for("default", Authority::Write).unwrap();
    scopes.extend(nl_draft_grants(Some("localhost")).unwrap());
    for name in ["titles", "fixed", "hopeless"] {
        scopes.extend(script_grants(name, ScriptAuthority::Publish).unwrap());
    }
    let session = gonk.sign_in(&Authenticator::named("drafter"), "drafter", &scopes);

    // ── 1. A sentence to a saved draft, first time: the check passes and previews the rows.
    let before = peer.calls();
    let (status, d) = draft(
        &gonk,
        Some(&session),
        "the TITLES of the ledger's items",
        "titles",
    );
    assert_eq!(status, 200, "{d}");
    assert_eq!(d["status"], "saved", "{d}");
    assert_eq!(d["attempts"].as_array().unwrap().len(), 1, "{d}");
    assert_eq!(d["check"]["valid"], true, "{d}");
    let preview = d["check"]["preview"].to_string();
    assert!(preview.contains("first item"), "{d}");
    assert!(
        d["version"]
            .as_str()
            .unwrap()
            .starts_with("urn:script:titles:version:sha256:"),
        "{d}"
    );
    assert_eq!(peer.calls(), before + 1, "one model call");
    let saved = d["query"].as_str().unwrap().to_string();
    assert!(
        saved.contains(ikigai_nl::drafter::PROV_MARKER),
        "the saved text carries its provenance: {saved}"
    );

    // ── 2. A deliberately bad first draft is REPAIRED within the bound: the checker names the
    // invented predicate, the repair prompt carries it, and the second draft is saved.
    let (status, d) = draft(&gonk, Some(&session), "FIXME: the item titles", "fixed");
    assert_eq!(status, 200, "{d}");
    assert_eq!(d["status"], "saved", "{d}");
    let attempts = d["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "{d}");
    assert_eq!(attempts[0]["valid"], false, "{d}");
    assert!(
        attempts[0]["errors"].to_string().contains("invented"),
        "the checker named what was wrong: {d}"
    );
    assert_eq!(attempts[1]["valid"], true, "{d}");

    // ── 3. A draft that never validates is REFUSED at the bound (3 attempts), with every
    // attempt and no query, and nothing is saved.
    let before = peer.calls();
    let (status, d) = draft(&gonk, Some(&session), "HOPELESS", "hopeless");
    assert_eq!(
        status, 200,
        "a failed draft is an answer, not an error: {d}"
    );
    assert_eq!(d["status"], "failed", "{d}");
    assert_eq!(d["attempts"].as_array().unwrap().len(), 3, "{d}");
    assert!(d.get("query").is_none(), "{d}");
    assert_eq!(peer.calls(), before + 3, "the bound is the spend");
    let (status, _) = gonk.http("GET", "/script/hopeless", Some(&session), "");
    assert_eq!(status, 404, "nothing was saved");

    // ── 4. The draft is its author's: they read it; another person holding the read grant
    // finds nothing there; root (the socket) sees it.
    let (status, body) = gonk.http(
        "GET",
        "/script/titles?as=application%2Fjson",
        Some(&session),
        "",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"draft\""), "{body}");
    let mut reader = grants_for("default", Authority::Read).unwrap();
    reader.extend(script_grants("titles", ScriptAuthority::Run).unwrap());
    reader.extend(script_grants("ghost", ScriptAuthority::Run).unwrap());
    let other = gonk.sign_in(&Authenticator::named("reader"), "reader", &reader);
    let (status, body) = gonk.http("GET", "/script/titles", Some(&other), "");
    assert_eq!(status, 404, "a draft is its author's alone: {body}");
    // Nor does it run for them, refused exactly as a name nobody wrote is, under the same grant
    // (`ghost`: held, never written). Since ikigai-script 0.3.3 (ledger #1261) a draft wears
    // the run family's contract, not Lisp's, so both are NotFound, word for word but the name;
    // through 0.3.2 a draft's run was refused for want of `urn:cap:lisp`, which hid the reason.
    let run = gonk.http("GET", "/script/titles/result", Some(&other), "");
    let nobody = gonk.http("GET", "/script/ghost/result", Some(&other), "");
    assert_ne!(
        run.0, 200,
        "a draft does not run for another person: {}",
        run.1
    );
    assert_eq!(run.0, nobody.0, "{} / {}", run.1, nobody.1);
    assert_eq!(
        run.1.replace("titles", "ghost"),
        nobody.1,
        "a draft is indistinguishable from an absent name"
    );
    gonk.over_socket(request(Verb::Source, "urn:script:titles", &[]))
        .expect("root sees every draft");

    // ── 5. The author PUBLISHES the draft as it was saved, and `:result` runs, for them and
    // for the other person.
    let (status, body) = gonk.http(
        "POST",
        "/script/titles?language=sparql",
        Some(&session),
        &saved,
    );
    assert_eq!(status, 200, "the author publishes: {body}");
    for (who, s) in [("author", &session), ("other", &other)] {
        let (status, body) = gonk.http("GET", "/script/titles/result?as=text%2Fcsv", Some(s), "");
        assert_eq!(status, 200, "{who}: {body}");
        assert!(
            body.contains("first item") && body.contains("second item"),
            "{who}: {body}"
        );
    }
    // A published script is never replaced by a draft: refused before any model is asked.
    let before = peer.calls();
    let (status, d) = draft(&gonk, Some(&session), "the TITLES again", "titles");
    assert_eq!(status, 400, "{d}");
    assert_eq!(peer.calls(), before);

    // ── 6. Anonymous may not draft: refused at the kernel's floor, before the grounding and
    // before any model call.
    let before = peer.calls();
    let (status, d) = draft(&gonk, None, "the TITLES of the items", "titles");
    assert_eq!(status, 403, "{d}");
    assert!(d.to_string().contains("urn:cap:net:localhost"), "{d}");
    assert_eq!(peer.calls(), before);
    // …but the grounding and the check spend nothing, and answer it under its own grant.
    let (status, body) = gonk.http(
        "GET",
        "/nl/grounding?focus=urn%3Aiki%3Aledger%3Agraph%3Adefault&as=application%2Fjson",
        None,
        "",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("urn:iki:ledger:graph:default"), "{body}");

    // ── 7. A QUIC machine granted drafting drafts too, saved as ITS draft (a machine drafts, a
    // person publishes); without the grant it is refused like anonymous.
    let machine_draft = request(
        Verb::Sink,
        "urn:nl:sparql",
        &[
            ("ask", "the TITLES, for a machine"),
            ("save", "machine-titles"),
            ("as", "application/json"),
        ],
    );
    match gonk.over_quic(machine_draft.clone()) {
        Err(Error::Denied(why)) => assert!(why.contains("urn:cap:net:localhost"), "{why}"),
        other => panic!("a machine without the grant: {other:?}"),
    }
    let mut machine = grants_for("default", Authority::Read).unwrap();
    machine.extend(nl_draft_grants(Some("localhost")).unwrap());
    machine.extend(script_grants("machine-titles", ScriptAuthority::Publish).unwrap());
    quic::enrol(
        &gonk.layout,
        "alpha",
        &gonk.client.fingerprint,
        &machine,
        true,
    )
    .expect("re-enroll alpha");
    let answer = gonk.over_quic(machine_draft).expect("a machine drafts");
    let d = json(&answer);
    assert_eq!(d["status"], "saved", "{d}");
    gonk.over_quic(request(Verb::Source, "urn:script:machine-titles", &[]))
        .expect("the machine reads its own draft");
}
