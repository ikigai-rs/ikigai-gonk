//! The git-event review trigger: the queue, the pass, the reviewer's grant, and the bound on
//! what an armed one spends. Ledger [#261](http://localhost:1060/l/default/item/261) built
//! it; [#466](http://localhost:1060/l/default/item/466) armed it.
//!
//! # What this file is actually asserting
//!
//! 1. **The trigger's call is the button's call.** `ikigai-browse` pinned the other half —
//!    `the_button_and_a_trigger_are_one_call_with_two_causes` derives the button's exact
//!    call and asserts the trigger's is an archive HIT on it. This file pins gonk's half: the
//!    Request `crate::trigger` builds is `Source urn:repo:{repo}:review:{path}` with
//!    `as=application/json` and nothing else, and the pass endpoint issues exactly that and
//!    returns the answer whole.
//! 2. **The queue is a queue.** Tuples are files, an identical request collapses to one
//!    tuple, and what is in the inbox is still there for a process that starts later —
//!    including, since [`an_armed_trigger_reviews_what_was_already_waiting`], for a process
//!    that then reviews it without anybody asking.
//! 3. ⚠ **The interlock is arithmetic, and nothing here may erode it.**
//!    [`no_provisioning_command_can_mint_a_reviewer`] holds that no command this server
//!    offers can produce the reviewer's authority, so arming stays something a person wrote
//!    into `grants.json`; [`a_reviewer_grant_that_could_publish_is_refused_before_anything_is_armed`]
//!    holds that a reviewer carrying `urn:cap:annotate` stops the server. Brian, 2026-09-19:
//!    *"Nothing gets published to Gonk except by the human."* Since `ikigai-browse` 0.5.0 a
//!    pass writes PENDING findings and **cannot** publish, which is what made arming safe —
//!    and `tests/browse.rs::the_pass_requires_exactly_what_the_real_review_requires` is the
//!    one that reads that claim off real browse rather than off the stub below.
//! 4. ⚠ **The spend bound is falsifiable.** [`a_second_concurrent_pass_is_refused_rather_than_paid_for`]
//!    turns "the reactor happens to be single-threaded" into a refusal this server issues,
//!    and [`the_depth_tells_a_slow_queue_from_a_stuck_one`] is the liveness readout that is
//!    the only symptom a dead watcher has.

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Fallback, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Result, Space, Verb,
};
use ikigai_gonk::grants::{self, Authority};
use ikigai_gonk::trigger::{self, Trigger, Tuple};

// ----------------------------------------------------------------- fixtures

fn queue(dir: &std::path::Path) -> Trigger {
    Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: dir.join("spaces"),
        arm: false,
    }
}

/// The exact scopes a review pass needs, **read off the kernel's own contract** — the same
/// function an operator's banner and refusal messages go through.
///
/// ⚠ Not a list: [`trigger::reviewer_grant_shape`] used to be one, and it went stale the day
/// browse 0.5.0 dropped `urn:cap:annotate` while still compiling. A test that spelled the
/// scopes here would have gone stale with it.
fn reviewer(kernel: &Kernel, review_iri: &str) -> Capability {
    Capability::scoped(
        trigger::reviewer_grant_shape(kernel, review_iri, "localhost").expect("a bound review"),
    )
}

/// One recorded call: the verb, the target, and the arguments in name order.
type Call = (Verb, String, Vec<(String, String)>);

/// What a [`Recorder`] has been asked, shared with the test that built it.
type Seen = Arc<std::sync::Mutex<Vec<Call>>>;

/// A stub bound where `ikigai-browse`'s review would be, recording what it was asked.
struct Recorder {
    seen: Seen,
}

#[async_trait::async_trait]
impl Endpoint for Recorder {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let mut args: Vec<(String, String)> = inv
            .request
            .args
            .iter()
            .map(|(k, v)| {
                let value = match v {
                    ArgRef::Inline(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                    other => format!("{other:?}"),
                };
                (k.clone(), value)
            })
            .collect();
        args.sort();
        self.seen.lock().expect("not poisoned").push((
            inv.request.verb,
            inv.request.target.to_string(),
            args,
        ));
        Ok(Representation::new(
            ReprType::new("application/json"),
            br#"{"derived":false,"minted":[]}"#.to_vec(),
        ))
    }

    fn name(&self) -> &str {
        "recorder"
    }

    fn describe(&self) -> Description {
        // The same capabilities the real review declares, so a capability that would be
        // refused by `ikigai-browse` is refused here too.
        //
        // ⚠⚠ **`urn:cap:annotate` is NOT among them, and this stub is a copy of a contract
        // that has already changed under this crate once.** browse 0.5.0 dropped it — a pass
        // writes pending findings and cannot publish — and that absence IS the interlock the
        // armed trigger rests on. A stub that still demanded it would make every test here
        // pass against a reviewer grant the real browse refuses, which is the exact shape of
        // the bug that shipped in `reviewer_grant_shape`. `tests/browse.rs` reads both real
        // contracts off a composed kernel; this one is the cheap fixture beside it.
        Description::new("recorder")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .requires(grants::CAP_NET_ANY)
            .output("application/json")
    }
}

/// A kernel holding the trigger's pair plus a recorder standing in for the review.
///
/// No store: the pass endpoint opens nothing and the queue is a directory, so this is the
/// whole composition the trigger needs. `compose_with`'s own wiring is walked by
/// `tests/conformance.rs`.
fn kernel_with_recorder(trigger_queue: &Trigger, review_iri: &str) -> (Kernel, Seen) {
    let seen: Seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = EndpointSpace::new().bind(
        Exact::new(review_iri),
        Recorder {
            seen: Arc::clone(&seen),
        },
    );
    let mut spaces = trigger::space(
        trigger_queue,
        Arc::new(trigger::Activity::default()),
        // Unarmed: these tests drive the pass directly, which is what a person piping a
        // tuple does. `armed` changes only what `urn:iki:gonk:review:depth` SAYS.
        false,
    );
    spaces.push(Arc::new(recorder) as Arc<dyn Space>);
    (Kernel::new(Arc::new(Fallback::new(spaces))), seen)
}

fn issue(kernel: &Kernel, request: Request, capability: &Capability) -> Result<Representation> {
    block_on(kernel.issue(request, capability))
}

fn tuple_arg(tuple: &Tuple) -> ArgRef {
    ArgRef::Inline(trigger::tuple_turtle(tuple).into_bytes())
}

// -------------------------------------------------- 1. one call, two causes

/// ★★ **THE INVARIANT, gonk's half.**
///
/// `ikigai-browse`'s `the_button_and_a_trigger_are_one_call_with_two_causes` derives the
/// button's exact call — `hx-get="/k/source urn:repo:demo:review:a.rs as=text/html"` — and
/// then asserts that the trigger's call (same IRI, no provider, `as=application/json`) is an
/// archive HIT on it: same version tag, same minted set, nothing paid.
///
/// This is the call it means, built here. If a later edit adds an argument, a rewrite or a
/// second route, this goes red before browse's does.
#[test]
fn the_trigger_builds_exactly_the_call_the_button_makes() {
    let request = trigger::review_request("demo", "a.rs").expect("a resolvable IRI");
    assert_eq!(request.verb, Verb::Source);
    assert_eq!(request.target.to_string(), "urn:repo:demo:review:a.rs");
    let args: Vec<(&str, String)> = request
        .args
        .iter()
        .map(|(k, v)| {
            let value = match v {
                ArgRef::Inline(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                other => format!("{other:?}"),
            };
            (k.as_str(), value)
        })
        .collect();
    assert_eq!(
        args,
        [("as", "application/json".to_string())],
        "one argument and no `provider`: a provider would fold a different model identity \
         into the archive key, so the trigger would stop sharing the button's entry"
    );
}

/// A path that carries a slash is the ordinary case and must survive into the IRI whole.
#[test]
fn a_nested_path_keeps_its_slashes() {
    let request = trigger::review_request("ikigai-gonk", "src/trigger.rs").expect("resolvable");
    assert_eq!(
        request.target.to_string(),
        "urn:repo:ikigai-gonk:review:src/trigger.rs"
    );
}

/// The pass issues that request and returns the review's answer, whole — no wrapper, no
/// re-serialization, nothing added.
#[test]
fn the_pass_issues_the_button_s_call_and_returns_the_answer_whole() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, seen) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let tuple = Tuple {
        repo: "demo".to_string(),
        path: "a.rs".to_string(),
    };
    let answer = issue(
        &kernel,
        Request::new(Verb::Source, Iri::parse(trigger::PASS).unwrap())
            .with_arg("content", tuple_arg(&tuple)),
        &reviewer(&kernel, "urn:repo:demo:review:a.rs"),
    )
    .expect("the pass runs");
    assert_eq!(
        String::from_utf8_lossy(&answer.bytes),
        r#"{"derived":false,"minted":[]}"#,
        "the review's answer is the answer"
    );
    let seen = seen.lock().expect("not poisoned");
    assert_eq!(
        *seen,
        vec![(
            Verb::Source,
            "urn:repo:demo:review:a.rs".to_string(),
            vec![("as".to_string(), "application/json".to_string())]
        )],
        "exactly one call, and it is the button's"
    );
}

// ------------------------------------------------ 2. capability, not convenience

/// The pass declares exactly what a review declares, and the kernel refuses short of it.
///
/// ⚠ Each is needed EVEN FOR A FREE ARCHIVE HIT: `review` declares them flatly on its
/// `Description`, so the kernel checks them before the endpoint runs and long before it
/// looks in the archive. There is no cheaper grant for the cheap case.
///
/// ★ The dropped set is DERIVED from the shape rather than listed, so the day another
/// capability joins or leaves the review's contract this test covers it without an edit —
/// which is the property `reviewer_grant_shape` itself lacked until 2026-09-20.
#[test]
fn a_pass_is_denied_short_of_any_one_of_them() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, seen) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let tuple = Tuple {
        repo: "demo".to_string(),
        path: "a.rs".to_string(),
    };
    let full = trigger::reviewer_grant_shape(&kernel, "urn:repo:demo:review:a.rs", "localhost")
        .expect("a bound review");
    // The scopes the PASS itself declares — the browse half. The store and exec tokens in
    // the shape are for hops past this endpoint, so dropping one of those is not refused
    // here, and asserting that it would be would be asserting an over-declaration.
    for dropped in [ikigai_browse::CAP_WILDCARD, "urn:cap:net:localhost"] {
        let scopes: Vec<String> = full.iter().filter(|s| *s != dropped).cloned().collect();
        let error = issue(
            &kernel,
            Request::new(Verb::Source, Iri::parse(trigger::PASS).unwrap())
                .with_arg("content", tuple_arg(&tuple)),
            &Capability::scoped(scopes),
        )
        .expect_err("a pass short of a capability must be refused");
        assert!(
            matches!(error, Error::Denied(_)),
            "dropping {dropped} gave {error:?}, not a typed Denied"
        );
    }
    assert!(
        seen.lock().expect("not poisoned").is_empty(),
        "a refusal must happen before anything is asked of the review"
    );
}

/// ⚠⚠ **ARMING IS AN OPERATOR'S ACT, AND NO PROVISIONING COMMAND CAN PERFORM IT.**
///
/// The trigger can be armed now (ledger
/// [#466](http://localhost:1060/l/default/item/466)) — browse 0.5.0 made a pass unable to
/// publish, so a headless reviewer no longer violates Brian's rule. What has NOT changed, and
/// is what this test holds, is that **nothing this server MINTS can carry the authority a
/// pass needs**. `client add --ledger`, `passkey invite --ledger` and `--browse-graph` hand
/// out ledger tokens and the browse graph's two store doors; the browse read and the net
/// grant have no flag at all, so a reviewer grant is something a person wrote into
/// `grants.json` with their hands. An arming path that could be reached by enrolling a client
/// would be a very different feature.
///
/// ★ And the second half: `urn:cap:annotate` — the publish token — must stay unmintable by
/// the browse-graph flag too, because the interlock is exactly that the reviewer lacks it.
#[test]
fn no_provisioning_command_can_mint_a_reviewer() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let needed = trigger::reviewer_grant_shape(&kernel, "urn:repo:demo:review:a.rs", "localhost")
        .expect("a bound review");
    let mut mintable: Vec<String> = Vec::new();
    for authority in [
        Authority::Read,
        Authority::Write,
        Authority::Delete,
        Authority::Purge,
    ] {
        mintable.extend(grants::grants_for("default", authority).expect("a ledger grant"));
        mintable.extend(grants::browse_graph_grants(authority).expect("the browse graph"));
    }
    for token in [
        ikigai_browse::CAP_WILDCARD,
        ikigai_browse::CAP_ANNOTATE,
        "urn:cap:net:localhost",
        trigger::CAP_EXEC_GH,
    ] {
        assert!(
            !mintable.contains(&token.to_string()),
            "`{token}` is now mintable by this server's own provisioning. Arming a headless \
             reviewer must stay a thing an operator writes into grants.json by hand — and if \
             the token is `urn:cap:annotate`, a mintable one would hand the reviewer the \
             publish authority the whole interlock rests on its lacking"
        );
    }
    // The two store tokens ARE mintable (`--browse-graph write`), which is the point: the
    // rest of what a pass needs has no flag at all. Ledger #435.
    for token in grants::browse_graph_grants(Authority::Write).expect("the browse graph") {
        assert!(
            needed.contains(&token),
            "the reviewer grant's shape must be the browse graph's own tokens, not a \
             transcription of them"
        );
    }
    // ⚠ And the shape itself never carries the publish token, whatever the contract says.
    assert!(
        !needed.contains(&ikigai_browse::CAP_ANNOTATE.to_string()),
        "the shape an operator is told to write must not include `{}`: a reviewer that can \
         publish is the interlock gone, and this helper shipped exactly that bug once",
        ikigai_browse::CAP_ANNOTATE
    );
}

/// ★★ **A grant that can publish is refused at STARTUP, not discovered at the first commit.**
///
/// This is the one check standing between an operator's copy-paste and unattended publishing,
/// so it is asserted in both directions: a reviewer carrying `urn:cap:annotate` stops the
/// server, and one short of what the review declares stops it too rather than dead-lettering
/// every tuple with a permission error nobody is watching.
#[test]
fn a_reviewer_grant_that_could_publish_is_refused_before_anything_is_armed() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let probe = "urn:repo:demo:review:a.rs";
    let good = trigger::reviewer_grant_shape(&kernel, probe, "localhost").expect("a bound review");
    trigger::check_reviewer(&kernel, probe, "localhost", &good).expect("the derived shape passes");

    let mut publishing = good.clone();
    publishing.push(ikigai_browse::CAP_ANNOTATE.to_string());
    let refusal = trigger::check_reviewer(&kernel, probe, "localhost", &publishing)
        .expect_err("a reviewer that can publish must be refused");
    assert!(refusal.contains(ikigai_browse::CAP_ANNOTATE), "{refusal}");
    assert!(refusal.contains("PUBLISHES"), "{refusal}");

    // Short of the browse read: every pass would be Denied, so this stops the server too —
    // and the message carries the stanza that fixes it.
    let short: Vec<String> = good
        .iter()
        .filter(|s| *s != ikigai_browse::CAP_WILDCARD)
        .cloned()
        .collect();
    let refusal = trigger::check_reviewer(&kernel, probe, "localhost", &short)
        .expect_err("a reviewer short of the browse read must be refused");
    assert!(refusal.contains(ikigai_browse::CAP_WILDCARD), "{refusal}");
    assert!(
        refusal.contains("\"reviewer\""),
        "the refusal hands over the stanza to paste: {refusal}"
    );

    // ⚠ The narrow net form satisfies the offering wildcard exactly as the kernel's own
    // check does — a grant naming the HOST must not read as a grant that is missing one.
    assert!(
        good.iter().any(|s| s == "urn:cap:net:localhost"),
        "{good:?}"
    );
    assert!(
        !good.iter().any(|s| s == grants::CAP_NET_ANY),
        "the offering wildcard is not a grant: {good:?}"
    );
}

/// The queue's own three tokens are minted by nobody either, so neither network door can
/// read, drop into, or take from it. It is reachable from the owner-only socket, which is
/// the door a person drains it through.
#[test]
fn no_grant_this_server_mints_can_reach_the_queue() {
    let mut mintable: Vec<String> = grants::grants_for("default", Authority::Purge).unwrap();
    mintable.extend(grants::browse_graph_grants(Authority::Write).unwrap());
    for token in [
        ikigai_intray::CAP_OUT,
        ikigai_intray::CAP_READ,
        ikigai_intray::CAP_TAKE,
    ] {
        assert!(
            !mintable.contains(&token.to_string()),
            "{token} is mintable"
        );
    }
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let error = issue(
        &kernel,
        Request::new(Verb::Source, Iri::parse("urn:space:reviews").unwrap()),
        &Capability::scoped(mintable),
    )
    .expect_err("reading the queue without `urn:cap:space:read` must be refused");
    assert!(matches!(error, Error::Denied(_)), "{error:?}");
}

// ------------------------------------------------------------- 3. the tuple

/// The tuple's bytes are the contract a git hook writes against, so they are pinned.
#[test]
fn a_tuple_round_trips_and_its_bytes_are_pinned() {
    let tuple = Tuple {
        repo: "ikigai-gonk".to_string(),
        path: "src/trigger.rs".to_string(),
    };
    let turtle = trigger::tuple_turtle(&tuple);
    assert_eq!(
        turtle,
        "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
         <urn:iki:gonk:review:request:ikigai-gonk:src/trigger.rs> ik:repo \"ikigai-gonk\" ; \
         ik:path \"src/trigger.rs\" .\n"
    );
    assert_eq!(trigger::parse_tuple(turtle.as_bytes()).unwrap(), tuple);
}

/// A tuple is ONE request. Two would break the bound the queue exists to hold — a pass
/// consumes one tuple — silently, by reviewing one of them.
#[test]
fn a_tuple_naming_two_requests_is_refused_rather_than_half_run() {
    let two = "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
               <urn:a> ik:repo \"demo\" ; ik:path \"a.rs\" .\n\
               <urn:b> ik:repo \"demo\" ; ik:path \"b.rs\" .\n";
    let error = trigger::parse_tuple(two.as_bytes()).expect_err("two requests, one tuple");
    assert!(
        matches!(&error, Error::InvalidArgument { detail, .. } if detail.contains("one tuple at a time")),
        "{error:?}"
    );
}

/// Not RDF at all, and something that is RDF but is not a review request, both explain
/// themselves rather than resolving to a wrong repository name.
#[test]
fn a_tuple_that_is_not_a_review_request_says_so() {
    assert!(matches!(
        trigger::parse_tuple(b"((name \"Ada\"))"),
        Err(Error::InvalidArgument { .. })
    ));
    let other = "@prefix foaf: <http://xmlns.com/foaf/0.1/> .\n<urn:p:a> a foaf:Person .\n";
    let error = trigger::parse_tuple(other.as_bytes()).expect_err("not a review request");
    assert!(
        matches!(&error, Error::InvalidArgument { detail, .. } if detail.contains("not a review request")),
        "{error:?}"
    );
}

// ------------------------------------------------------------- 4. the queue

/// ★ **The bound Brian asked for, as a property of the queue rather than of a policy.**
///
/// Forty changed files drop forty tuples; a person (and, when #444 arms one, a drainer)
/// takes them one at a time. Nothing decides a file was not worth reviewing.
#[test]
fn forty_files_drop_forty_tuples_and_come_back_one_at_a_time() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    for n in 0..40 {
        trigger::drop_tuple(
            &q,
            &Tuple {
                repo: "demo".to_string(),
                path: format!("src/f{n}.rs"),
            },
        )
        .expect("a drop");
    }
    assert_eq!(trigger::pending(&q), 40);

    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:src/f0.rs");
    let capability = Capability::scoped([ikigai_intray::CAP_READ, ikigai_intray::CAP_TAKE]);
    let listed = issue(
        &kernel,
        Request::new(Verb::Source, Iri::parse("urn:space:reviews").unwrap()),
        &capability,
    )
    .expect("rd");
    assert_eq!(
        String::from_utf8_lossy(&listed.bytes).lines().count(),
        40,
        "every request is queued, none merged away"
    );
    // One take is one tuple. The queue is the throttle; there is no batch face.
    for expected in (1..=40).rev() {
        assert_eq!(trigger::pending(&q), expected);
        issue(
            &kernel,
            Request::new(Verb::Delete, Iri::parse("urn:space:reviews").unwrap()),
            &capability,
        )
        .expect("take");
    }
    assert_eq!(trigger::pending(&q), 0);
}

/// ★ **The same request twice is ONE tuple**, because the drop is content-addressed and
/// [`trigger::tuple_turtle`] is byte-deterministic.
///
/// ⚠ This is the property that forbids a commit SHA or a timestamp in the tuple, and it is
/// why the commit->pass edge cannot come out of the queue (#261). What it buys: three
/// commits to one file while the queue waits collapse to one pass over the final content,
/// which is exactly what a person clicking `review` would get.
#[test]
fn the_same_request_twice_is_one_tuple() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    let tuple = Tuple {
        repo: "demo".to_string(),
        path: "src/lib.rs".to_string(),
    };
    let first = trigger::drop_tuple(&q, &tuple).expect("a drop");
    let second = trigger::drop_tuple(&q, &tuple).expect("the same drop");
    assert_eq!(first, second, "one content, one id");
    assert_eq!(trigger::pending(&q), 1);
    // A different path is a different tuple, so the collapse is by request and not by luck.
    trigger::drop_tuple(
        &q,
        &Tuple {
            repo: "demo".to_string(),
            path: "src/other.rs".to_string(),
        },
    )
    .expect("a second request");
    assert_eq!(trigger::pending(&q), 2);
}

/// ★ **The queue survives this process.** Tuples are files; a `Trigger` built fresh over the
/// same root sees what an earlier one dropped. That is the whole of "a commit that happens
/// while nothing is draining is not lost", and it is a property of the intray rather than
/// anything gonk keeps in memory.
#[test]
fn what_is_queued_is_still_queued_for_a_process_that_starts_later() {
    let dir = tempfile::tempdir().expect("a temp dir");
    {
        let q = queue(dir.path());
        trigger::drop_tuple(
            &q,
            &Tuple {
                repo: "demo".to_string(),
                path: "src/lib.rs".to_string(),
            },
        )
        .expect("a drop");
    }
    let later = queue(dir.path());
    assert_eq!(trigger::pending(&later), 1);
    let (kernel, _) = kernel_with_recorder(&later, "urn:repo:demo:review:src/lib.rs");
    let taken = issue(
        &kernel,
        Request::new(Verb::Delete, Iri::parse("urn:space:reviews").unwrap()),
        &Capability::scoped([ikigai_intray::CAP_TAKE]),
    )
    .expect("take");
    assert_eq!(
        trigger::parse_tuple(&taken.bytes).unwrap().path,
        "src/lib.rs"
    );
}

/// The end-to-end shape a person actually types: take one tuple, feed it to the pass.
#[test]
fn a_taken_tuple_feeds_the_pass() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::drop_tuple(
        &q,
        &Tuple {
            repo: "demo".to_string(),
            path: "a.rs".to_string(),
        },
    )
    .expect("a drop");
    let (kernel, seen) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let taken = issue(
        &kernel,
        Request::new(Verb::Delete, Iri::parse("urn:space:reviews").unwrap()),
        &Capability::scoped([ikigai_intray::CAP_TAKE]),
    )
    .expect("take");
    issue(
        &kernel,
        Request::new(Verb::Source, Iri::parse(trigger::PASS).unwrap())
            .with_arg("content", ArgRef::Inline(taken.bytes.clone())),
        &reviewer(&kernel, "urn:repo:demo:review:a.rs"),
    )
    .expect("the pass runs");
    assert_eq!(
        seen.lock().expect("not poisoned")[0].1,
        "urn:repo:demo:review:a.rs"
    );
    assert_eq!(trigger::pending(&q), 0, "the tuple is consumed by the take");
}

// ------------------------------------------------------ 5. configuration refusals

/// ⚠ A `cap` file beside the space is `ikigai-intray`'s reactor grant, and it MINTS rather
/// than narrows — from a directory anything that can drop a tuple can also write. gonk names
/// a grant instead, and refuses to start beside one rather than read it.
#[test]
fn a_reactor_cap_file_stops_this_server() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    assert!(trigger::refuse_cap_file(&q).is_ok(), "nothing is there yet");
    std::fs::write(q.dir().join("cap"), "urn:cap:store:write\n").expect("write");
    let refusal = trigger::refuse_cap_file(&q).expect_err("a cap file must stop this server");
    assert!(refusal.contains("MINTS a capability"), "{refusal}");
    assert!(refusal.contains("gonk.review.grant"), "{refusal}");
}

/// A space name is one segment. Checked at configuration time so a typo is a startup
/// refusal, not a tuple that fails where nobody is watching.
#[test]
fn a_space_name_is_one_segment() {
    assert!(trigger::check_space_name("reviews").is_ok());
    for bad in ["", "a/b", "a.b", "urn:x"] {
        assert!(
            trigger::check_space_name(bad).is_err(),
            "`{bad}` was accepted"
        );
    }
}

/// ★★ **The reviewer grant's shape is READ OFF THE CONTRACT of the review this kernel binds,
/// so it cannot go stale the way it did.**
///
/// It was a list of constants until 2026-09-20, and browse 0.5.0 made that list wrong while
/// it went on compiling: the `urn:cap:annotate` constant still existed, only the requirement
/// had gone. An operator following the helper would have written the reviewer the publish
/// token. This test drives the derivation against a stub whose contract is browse's, and then
/// CHANGES that contract to prove the shape follows it.
#[test]
fn the_reviewer_grant_shape_follows_the_review_it_reads() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let scopes = trigger::reviewer_grant_shape(&kernel, "urn:repo:demo:review:a.rs", "localhost")
        .expect("a bound review");
    assert!(scopes.contains(&ikigai_browse::CAP_WILDCARD.to_string()));
    assert!(scopes.contains(&"urn:cap:net:localhost".to_string()));
    assert!(scopes.contains(&trigger::CAP_EXEC_GH.to_string()));
    for token in grants::browse_graph_grants(Authority::Write).expect("the browse graph") {
        assert!(
            scopes.contains(&token),
            "{token} is missing from {scopes:?}"
        );
    }
    // ⚠ The narrow net form, never the offering wildcard: `quic::check_grants` refuses
    // `urn:cap:net:*` as a grant, so a shape carrying it could not be written down.
    assert!(
        !scopes.contains(&grants::CAP_NET_ANY.to_string()),
        "the offering wildcard is not a grant"
    );
    assert!(
        grants::unbounded_net_scopes(&scopes).is_empty(),
        "and this server's own check agrees"
    );
    assert!(
        grants::unbounded_exec_scopes(&scopes).is_empty(),
        "nor the exec one: `gh` is named, never `*`"
    );

    // ★ The derivation FOLLOWS: a review that declared one more capability would put it in
    // the operator's grant, with nobody editing this crate.
    struct Wider;
    #[async_trait::async_trait]
    impl Endpoint for Wider {
        async fn invoke(&self, _: &Invocation<'_>) -> Result<Representation> {
            Err(Error::Endpoint("never invoked".into()))
        }
        fn name(&self) -> &str {
            "wider"
        }
        fn describe(&self) -> Description {
            Description::new("wider")
                .verb(Verb::Source)
                .verb(Verb::Meta)
                .requires(ikigai_browse::CAP_WILDCARD)
                .requires(grants::CAP_NET_ANY)
                .requires("urn:cap:invented:later")
        }
    }
    let wider = Kernel::new(Arc::new(
        EndpointSpace::new().bind(Exact::new("urn:repo:demo:review:a.rs"), Wider),
    ));
    let scopes =
        trigger::reviewer_grant_shape(&wider, "urn:repo:demo:review:a.rs", "127.0.0.1").unwrap();
    assert!(
        scopes.contains(&"urn:cap:invented:later".to_string()),
        "a capability this crate has never heard of must reach the operator's grant: \
         {scopes:?}"
    );
    assert!(
        scopes.contains(&"urn:cap:net:127.0.0.1".to_string()),
        "and the host is the one asked for, not `localhost`: {scopes:?}"
    );

    // ⚠ A review that does not resolve is an ERROR, never a remembered list. A gonk with no
    // browse root or no mount has nothing to read, and a helpful guess is what shipped the
    // wrong grant in the first place.
    let bare = Kernel::new(Arc::new(EndpointSpace::new()));
    let refusal = trigger::reviewer_grant_shape(&bare, "urn:repo:demo:review:a.rs", "localhost")
        .expect_err("no review, no shape");
    assert!(refusal.contains("does not resolve"), "{refusal}");
}

// ---------------------------------------------------- 6. what the doors are offered

/// ★ **A configured trigger adds exactly three resources and no more.**
///
/// The sibling of `tests/conformance.rs`'s catalog pin, from the other side: that one holds
/// the catalog of a gonk with no trigger, this one holds the DELTA a `gonk.review.space`
/// line makes. `ikigai-intray` binds one template and this crate binds two endpoints; a
/// version that bound a fourth would be behind every door this binary opens, silently.
#[test]
fn a_configured_trigger_adds_exactly_the_queue_the_pass_and_the_depth() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let kernel = Kernel::new(Arc::new(Fallback::new(trigger::space(
        &q,
        Arc::new(trigger::Activity::default()),
        false,
    ))));
    let mut ids: Vec<String> = kernel
        .entries()
        .expect("an enumerable root")
        .iter()
        .filter(|entry| !entry.pattern.starts_with("urn:kernel:"))
        .map(|entry| {
            kernel
                .describe_pattern(&entry.pattern)
                .unwrap_or_else(|| panic!("`{}` describes itself", entry.pattern))
                .id
        })
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "gonk-review-depth".to_string(),
            "gonk-review-pass".to_string(),
            "space".to_string()
        ],
        "the trigger's whole served surface"
    );
}

/// ★★ **The pass declares EXACTLY what the review it issues declares — no more, no less.**
///
/// ⚠ Declaring NOTHING would be an over-offer — the manifold would say any caller may run a
/// pass, and the refusal would arrive one hop in, from a resource the caller never named.
/// ⚠⚠ Declaring MORE is the failure this arc found: `urn:cap:annotate` was declared here
/// after browse 0.5.0 stopped requiring it, so the reviewer grant this design hands out
/// would have been refused by gonk's own contract one hop before the review that accepts it.
/// The assertion is therefore against the REVIEW's contract on the same kernel, not a list.
#[test]
fn the_pass_declares_exactly_what_the_review_declares() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let described = kernel
        .describe(&Iri::parse(trigger::PASS).unwrap())
        .expect("the pass describes itself");
    let review = kernel
        .describe(&Iri::parse("urn:repo:demo:review:a.rs").unwrap())
        .expect("the review describes itself");
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v.dedup();
        v
    };
    assert_eq!(
        sorted(described.requires.clone()),
        sorted(review.requires.clone()),
        "the pass and the review must require the same set: more refuses callers the review \
         accepts, less is an over-offer that fails one hop in"
    );
    assert!(
        !described
            .requires
            .contains(&ikigai_browse::CAP_ANNOTATE.to_string()),
        "a pass writes PENDING findings and cannot publish — declaring the publish token \
         here would deny the reviewer grant before browse ever saw it"
    );
    assert!(described.verbs.contains(&Verb::Source));
    let names: Vec<&str> = described
        .inputs
        .iter()
        .map(|input| input.name.as_str())
        .collect();
    assert_eq!(names, ["content", "in", "space", "tuple"]);
    assert!(
        described.inputs[0].required,
        "`content` is the one required by-value input, so a piped tuple routes to it"
    );
}

/// ⚠ A request this module cannot emit is refused at the DROP, not queued as a tuple nothing
/// can read back.
///
/// [`trigger::tuple_turtle`] is a `format!` rather than a Turtle serializer — that is what
/// makes its bytes deterministic, and therefore what makes an identical request collapse to
/// one queue entry — so it escapes nothing. A git path may legally carry a `"`, a `\` or a
/// newline, and each would emit Turtle that `parse_tuple` then refuses. A drop that
/// succeeded and left a request failing forever is the worst outcome available here.
#[test]
fn a_request_this_server_cannot_emit_is_refused_at_the_drop() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    for path in [
        "src/a\"b.rs",
        "src/a\\b.rs",
        "src/a b.rs",
        "src/a\nb.rs",
        "src/a<b>.rs",
        "",
    ] {
        let tuple = Tuple {
            repo: "demo".to_string(),
            path: path.to_string(),
        };
        let refusal = trigger::drop_tuple(&q, &tuple)
            .expect_err(&format!("`{path}` must be refused, not queued"));
        assert!(
            refusal.contains("review request's path"),
            "`{path}` gave {refusal}"
        );
    }
    assert_eq!(trigger::pending(&q), 0, "nothing poisoned the queue");
    // And an ordinary path still drops, so the rule is a bound and not a wall.
    trigger::drop_tuple(
        &q,
        &Tuple {
            repo: "demo".to_string(),
            path: "src/a-b_c.2.rs".to_string(),
        },
    )
    .expect("an ordinary path");
    assert_eq!(trigger::pending(&q), 1);
}

/// Every request this server WILL emit round-trips — the check and the writer agree.
#[test]
fn everything_that_passes_the_check_parses_back() {
    for path in ["a.rs", "src/lib.rs", "a/b/c/d-e_f.2.rs", "README.md"] {
        let tuple = Tuple {
            repo: "ikigai-gonk".to_string(),
            path: path.to_string(),
        };
        trigger::check_request(&tuple).expect("accepted");
        assert_eq!(
            trigger::parse_tuple(trigger::tuple_turtle(&tuple).as_bytes()).unwrap(),
            tuple,
            "`{path}` did not round-trip"
        );
    }
}

// ------------------------------------------------------------ 7. armed, and bounded

/// ★★ **The arc's whole point, end to end: a tuple already waiting is reviewed without a
/// person.**
///
/// `SpaceReactor::watch()`'s contract is *drain what is pending, then watch*, and the
/// catch-up half is what makes a push design survive a restart of this server — a commit
/// while gonk was down is not lost, it is the first thing reviewed when gonk comes back.
/// This drives that half through gonk's own [`trigger::arm`], under the reviewer capability
/// [`trigger::reviewer_grant_shape`] derives, and asserts the review was actually issued.
#[test]
fn an_armed_trigger_reviews_what_was_already_waiting() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut q = queue(dir.path());
    q.arm = true;
    trigger::prepare(&q).expect("prepare");
    let tuple = Tuple {
        repo: "demo".to_string(),
        path: "a.rs".to_string(),
    };
    trigger::drop_tuple(&q, &tuple).expect("a drop with no server running");
    assert_eq!(trigger::pending(&q), 1);

    let (kernel, seen) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let hub = Arc::new(kernel);
    let scopes = trigger::reviewer_grant_shape(&hub, "urn:repo:demo:review:a.rs", "localhost")
        .expect("a bound review");
    trigger::arm(&q, Arc::clone(&hub), &scopes).expect("arming");

    // ⚠ Polled rather than slept: `arm` puts the catch-up on a thread of its own (the crate's
    // `watch()` runs it in the CALLING thread, which would hold a real server's doors shut
    // for one model call per waiting tuple).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && seen.lock().expect("not poisoned").is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let calls = seen.lock().expect("not poisoned").clone();
    assert_eq!(
        calls.len(),
        1,
        "the waiting tuple must reach the review exactly once: {calls:?}"
    );
    assert_eq!(calls[0].1, "urn:repo:demo:review:a.rs");
    assert_eq!(trigger::pending(&q), 0, "and leave the inbox");
    assert_eq!(
        trigger::depth(Some(&q)),
        trigger::Depth::Counted {
            inbox: 0,
            outbox: 1,
            error: 0
        },
        "a handled tuple lands in the outbox, which is what `handled` counts"
    );
}

/// ⚠ **A `cap` file stops ARMING too, and the reason is the opposite of the old one.**
///
/// `refuse_cap_file` stops this server because the file used to MINT authority. Under
/// `with_host_authority` the crate never reads it — so the file is INERT, and an operator who
/// wrote one believes they have bounded a reviewer they have not. `arm` asks the reactor's
/// own `ignored_cap_files()` and refuses rather than going live beside a lie.
#[test]
fn arming_beside_an_ignored_cap_file_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut q = queue(dir.path());
    q.arm = true;
    trigger::prepare(&q).expect("prepare");
    std::fs::write(q.dir().join("cap"), "urn:cap:store:write\n").expect("write");
    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let refusal = trigger::arm(&q, Arc::new(kernel), &["urn:cap:browse:read:*".to_string()])
        .expect_err("a cap file must stop arming");
    assert!(refusal.contains("INERT"), "{refusal}");
    assert!(refusal.contains("gonk.review.grant"), "{refusal}");
}

/// The `handler` file is gonk's, written when armed and REMOVED when not.
///
/// ⚠ It is a control surface in the tree a dropper writes into: `SpaceReactor` reads it per
/// tuple and fires whatever IRI it names, under the reviewer's authority. An unarmed gonk
/// that left one behind would be loading the gun for the next process that is armed.
#[test]
fn the_handler_file_is_this_servers_and_goes_away_when_it_is_not_armed() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let handler = q.dir().join("handler");

    trigger::set_handler(&q, true).expect("armed");
    assert_eq!(
        std::fs::read_to_string(&handler).expect("a handler"),
        format!("{}\n", trigger::PASS),
        "the handler names gonk's own pass and nothing else"
    );

    // Something else rewrote it: gonk's value is the value, re-asserted at startup.
    std::fs::write(&handler, "urn:system:exec\n").expect("a retarget");
    trigger::set_handler(&q, true).expect("armed again");
    assert_eq!(
        std::fs::read_to_string(&handler).expect("a handler"),
        format!("{}\n", trigger::PASS)
    );

    trigger::set_handler(&q, false).expect("unarmed");
    assert!(!handler.exists(), "an unarmed gonk leaves nothing to fire");
    trigger::set_handler(&q, false).expect("unarming twice is not an error");
}

/// ★★ **The spend bound, made falsifiable rather than inherited.**
///
/// Passes are serial because `SpaceReactor::watch` reads its channel on one thread and calls
/// `process` inline — a property of a dependency's thread shape, invisible from this crate
/// and free to change without a compile error. [#308](http://localhost:1060/l/default/item/308)
/// asked what bounds a forty-file push, and "the reactor happens to be single-threaded" is an
/// answer that stops being true silently. So a second concurrent pass is REFUSED, and this is
/// that refusal.
#[test]
fn a_second_concurrent_pass_is_refused_rather_than_paid_for() {
    let activity = Arc::new(trigger::Activity::default());
    let first = activity.begin(1_000).expect("the first pass");
    let refusal = activity
        .begin(1_100)
        .err()
        .expect("a second concurrent pass must be refused");
    assert!(
        matches!(refusal, Error::Unavailable(_)),
        "transient, so a re-dropped tuple can succeed: {refusal:?}"
    );
    assert!(format!("{refusal}").contains("ONE at a time"), "{refusal}");

    // ⚠ A pass that ends by `?` counts as a failure and RELEASES the slot: a leaked slot
    // would wedge the reviewer for the life of the process with no symptom but a queue that
    // stops draining — which is the exact failure the depth resource exists to show.
    drop(first);
    let counts = activity.snapshot();
    assert_eq!((counts.started, counts.succeeded, counts.failed), (1, 0, 1));
    assert!(counts.in_flight_since_ms.is_none(), "the slot is released");

    let second = activity.begin(2_000).expect("the slot is free again");
    second.succeeded();
    let counts = activity.snapshot();
    assert_eq!((counts.started, counts.succeeded, counts.failed), (2, 1, 1));
    assert!(counts.last_end_ms.is_some());
}

/// ★ **A queue that is not empty with nothing in flight is STUCK, and says so.**
///
/// The armed trigger's only liveness signal (`watch()` catches up at startup and then lives
/// on a thread nothing observes; gonk runs no log at all,
/// [#383](http://localhost:1060/l/default/item/383)). A page that printed only the count
/// would render a slow queue and a dead watcher identically, and exactly one of them needs a
/// person.
#[test]
fn the_depth_tells_a_slow_queue_from_a_stuck_one() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    for n in 0..3 {
        std::fs::write(q.inbox().join(format!("{n}.tuple")), "x").expect("a tuple");
    }
    let activity = Arc::new(trigger::Activity::default());
    let armed = trigger::DepthEndpoint {
        trigger: Some(Arc::new(q.clone())),
        activity: Arc::clone(&activity),
        armed: true,
    };

    // Armed, three waiting, nothing running: stuck.
    let status = armed.status();
    assert!(status.stuck(), "{status:?}");
    let sentence = status.sentence(10_000);
    assert!(sentence.contains("NONE IN FLIGHT"), "{sentence}");
    assert!(sentence.contains("STUCK reviewer"), "{sentence}");

    // A pass in flight: the same three waiting, and not stuck.
    let pass = activity.begin(5_000).expect("a pass");
    let status = armed.status();
    assert!(!status.stuck(), "{status:?}");
    let sentence = status.sentence(65_000);
    assert!(!sentence.contains("STUCK"), "{sentence}");
    assert!(sentence.contains("running for 60s"), "{sentence}");
    pass.succeeded();

    // ⚠ UNARMED is a third statement, and it is not "stuck": nothing is supposed to be
    // draining, so a number that does not fall is correct rather than alarming.
    let unarmed = trigger::DepthEndpoint {
        trigger: Some(Arc::new(q)),
        activity,
        armed: false,
    };
    let status = unarmed.status();
    assert!(!status.stuck(), "{status:?}");
    let sentence = status.sentence(10_000);
    assert!(sentence.contains("NOTHING IS DRAINING"), "{sentence}");
    assert!(sentence.contains("gonk.review.arm"), "{sentence}");

    // …and NOT CONFIGURED is a fourth, which is neither a number nor a failure (ledger #446).
    let absent = trigger::DepthEndpoint {
        trigger: None,
        activity: Arc::new(trigger::Activity::default()),
        armed: false,
    };
    let sentence = absent.status().sentence(0);
    assert!(
        sentence.contains("No review queue is configured"),
        "{sentence}"
    );
}

/// The depth is reachable through the kernel under the browse read it declares, and refused
/// without it — the floor ledger [#464](http://localhost:1060/l/default/item/464) asked for,
/// instead of the space's own token, which this server mints for nobody.
#[test]
fn the_depth_has_its_own_floor_and_answers_both_faces() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");

    let denied = issue(
        &kernel,
        Request::new(Verb::Source, Iri::parse(trigger::DEPTH).unwrap()),
        // A ledger grant, which is all an anonymous loopback caller holds.
        &Capability::scoped(grants::grants_for("default", Authority::Write).unwrap()),
    )
    .expect_err("the depth is process state, not public");
    assert!(matches!(denied, Error::Denied(_)), "{denied:?}");

    let reader = Capability::scoped([ikigai_browse::CAP_WILDCARD]);
    let plain = issue(
        &kernel,
        Request::new(Verb::Source, Iri::parse(trigger::DEPTH).unwrap()),
        &reader,
    )
    .expect("a browse reader may see the depth");
    assert!(
        String::from_utf8_lossy(&plain.bytes).contains("The review queue is empty"),
        "{}",
        String::from_utf8_lossy(&plain.bytes)
    );

    let json = issue(
        &kernel,
        Request::new(Verb::Source, Iri::parse(trigger::DEPTH).unwrap())
            .with_arg("as", ArgRef::Inline(b"application/json".to_vec())),
        &reader,
    )
    .expect("the machine face");
    let v: serde_json::Value = serde_json::from_slice(&json.bytes).expect("json");
    assert_eq!(v["configured"], true);
    assert_eq!(v["armed"], false);
    assert_eq!(v["waiting"], 0);
    assert_eq!(v["stuck"], false);
    // ★ The sentence rides WITH the numbers, so the badge, the page and the socket cannot
    // disagree about what the queue is doing.
    assert!(v["sentence"].as_str().is_some_and(|s| !s.is_empty()), "{v}");
}
