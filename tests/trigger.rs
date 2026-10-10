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
use ikigai_gonk::config::QueuePolicy;
use ikigai_gonk::grants::{self, Authority, BrowseRole};
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
        QueuePolicy::default(),
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
/// is what this test holds, is that **nothing this server MINTS can arm it**.
///
/// ★ Since ledger [#435](http://localhost:1060/l/default/item/435) the browse read, the net
/// grant and `urn:cap:annotate` ARE mintable — as the `--browse read|derive` roles — so the
/// interlock is no longer "has no flag". It is arithmetic over what the flags can combine:
/// **every mintable grant that may derive also carries `urn:cap:annotate`** (`derive` bundles
/// the two), and [`trigger::check_reviewer`] refuses any grant that can publish. So every
/// grant a provisioning command can write either cannot derive or can publish, and neither
/// arms. This walks every combination the flags can express and asks the real check.
///
/// ★ And `urn:cap:exec:gh` (the PR review tier's `gh`) stays unmintable by every flag.
#[test]
fn no_provisioning_command_can_mint_a_reviewer() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let probe = "urn:repo:demo:review:a.rs";
    let (kernel, _) = kernel_with_recorder(&q, probe);
    let needed =
        trigger::reviewer_grant_shape(&kernel, probe, "localhost").expect("a bound review");
    let authorities = [
        Authority::Read,
        Authority::Write,
        Authority::Delete,
        Authority::Purge,
    ];
    let mut every_token: Vec<String> = Vec::new();
    for ledger in authorities {
        for graph in [None, Some(Authority::Read), Some(Authority::Write)] {
            for role in [None, Some(BrowseRole::Read), Some(BrowseRole::Derive)] {
                let mut grant = grants::grants_for("default", ledger).expect("a ledger grant");
                if let Some(graph) = graph {
                    grant.extend(grants::browse_graph_grants(graph).expect("the browse graph"));
                }
                if let Some(role) = role {
                    grant.extend(
                        grants::browse_role_grants(role, &[], Some("localhost")).expect("a role"),
                    );
                }
                assert!(
                    trigger::check_reviewer(&kernel, probe, "localhost", &grant).is_err(),
                    "this MINTABLE grant arms a headless reviewer: {grant:?}. A role that \
                     spends inference without carrying `{}` would be exactly a mintable \
                     reviewer",
                    ikigai_browse::CAP_ANNOTATE
                );
                every_token.extend(grant);
            }
        }
    }
    assert!(
        !every_token.contains(&trigger::CAP_EXEC_GH.to_string()),
        "`{}` is now mintable by this server's own provisioning",
        trigger::CAP_EXEC_GH
    );
    // The two store tokens ARE mintable (`--browse-graph write`), and the reviewer's shape
    // must be computed from them rather than transcribed.
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

    // ★ And the HOST is checked exactly (ledger #805): `localhost` satisfies the kernel's
    // wildcard, but the mount at `127.0.0.1` refuses it, so a reviewer granted for the wrong
    // host is refused HERE rather than dead-lettering every pass.
    let refusal = trigger::check_reviewer(&kernel, probe, "127.0.0.1", &good)
        .expect_err("a net grant for another host must be refused");
    assert!(refusal.contains("urn:cap:net:127.0.0.1"), "{refusal}");
}

/// The queue's own three tokens are minted by nobody either, so neither network door can
/// read, drop into, or take from it. It is reachable from the owner-only socket, which is
/// the door a person drains it through.
#[test]
fn no_grant_this_server_mints_can_reach_the_queue() {
    let mut mintable: Vec<String> = grants::grants_for("default", Authority::Purge).unwrap();
    mintable.extend(grants::browse_graph_grants(Authority::Write).unwrap());
    mintable
        .extend(grants::browse_role_grants(BrowseRole::Derive, &[], Some("localhost")).unwrap());
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

/// Git paths an IRI cannot carry raw. Each passed [`trigger::check_request`] before ledger
/// #737 and then failed one hop later: `[`, `]` and a `%` that is not an escape made a tuple
/// `parse_tuple` refused (a POISON tuple — the drop succeeded and the request could never
/// run), and a `%` that IS an escape named a different file once browse decoded it.
const AWKWARD_PATHS: [&str; 9] = [
    "pages/[id].tsx",
    "docs/100%.md",
    "a%zz.rs",
    "a%.rs",
    "a%41.rs",
    "docs/a%20b.md",
    "notes/c#.md",
    "a?b.rs",
    "src/café.rs",
];

/// ★ Everything [`trigger::check_request`] accepts reads back as the SAME tuple and names a
/// review. Reproductions from the review-value experiment (ledger #723, both arms): on
/// 4c1caec `pages/[id].tsx` and `docs/100%.md` were accepted and did neither.
#[test]
fn every_path_the_check_accepts_reads_back_and_names_a_review() {
    for path in AWKWARD_PATHS {
        let tuple = Tuple {
            repo: "demo".to_string(),
            path: path.to_string(),
        };
        trigger::check_request(&tuple).expect("a git path the trigger can express");
        let back = trigger::parse_tuple(trigger::tuple_turtle(&tuple).as_bytes());
        assert_eq!(
            back.ok().as_ref(),
            Some(&tuple),
            "`{path}` did not read back"
        );
        trigger::review_request("demo", path)
            .unwrap_or_else(|e| panic!("`{path}` was accepted and names no review: {e}"));
    }
}

/// The same end to end at the drop: what lands in the inbox is runnable, never poison.
#[test]
fn a_dropped_tuple_is_never_poison() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    for path in AWKWARD_PATHS {
        let tuple = Tuple {
            repo: "demo".to_string(),
            path: path.to_string(),
        };
        trigger::drop_tuple(&q, &tuple).unwrap_or_else(|e| panic!("`{path}`: {e}"));
    }
    assert_eq!(trigger::pending(&q), AWKWARD_PATHS.len());
    let mut read: Vec<String> = std::fs::read_dir(q.inbox())
        .expect("the inbox")
        .flatten()
        .map(|entry| {
            let bytes = std::fs::read(entry.path()).expect("a tuple");
            trigger::parse_tuple(&bytes)
                .unwrap_or_else(|e| panic!("{} is poison: {e}", entry.path().display()))
                .path
        })
        .collect();
    read.sort();
    let mut want: Vec<String> = AWKWARD_PATHS.iter().map(|p| p.to_string()).collect();
    want.sort();
    assert_eq!(read, want);
}

/// The path is percent-encoded in both IRIs exactly as `ikigai-browse` encodes it for its
/// own Review button (its private `iri_encode`), and only there: the `ik:path` literal keeps
/// the path as git spells it, and a path with nothing to encode is byte-identical to what a
/// hook wrote before (see [`a_tuple_round_trips_and_its_bytes_are_pinned`]).
#[test]
fn the_path_is_encoded_in_the_iris_and_literal_in_the_tuple() {
    for (path, encoded) in [
        ("pages/[id].tsx", "pages/%5Bid%5D.tsx"),
        ("docs/100%.md", "docs/100%25.md"),
        ("a%41.rs", "a%2541.rs"),
        ("notes/c#.md", "notes/c%23.md"),
        ("a?b.rs", "a%3Fb.rs"),
        ("src/café.rs", "src/caf%C3%A9.rs"),
        ("a-b_c.2~/x!$&'()*+,;=:@.rs", "a-b_c.2~/x!$&'()*+,;=:@.rs"),
    ] {
        let review = trigger::review_request("demo", path).expect("a review");
        assert_eq!(
            review.target.as_str(),
            format!("urn:repo:demo:review:{encoded}")
        );
        let tuple = Tuple {
            repo: "demo".to_string(),
            path: path.to_string(),
        };
        assert_eq!(
            tuple.iri(),
            format!("urn:iki:gonk:review:request:demo:{encoded}")
        );
        assert!(
            trigger::tuple_turtle(&tuple).contains(&format!("ik:path \"{path}\"")),
            "the literal is the path as git spells it"
        );
    }
}

/// The repository is a configured root name and is NOT encoded — browse does not encode it
/// either — so a character that would change what the IRI means there is refused at the
/// drop rather than queued.
#[test]
fn a_repository_name_an_iri_cannot_carry_is_refused() {
    for repo in ["de[mo", "de]mo", "de%mo", "de#mo", "de?mo"] {
        let tuple = Tuple {
            repo: repo.to_string(),
            path: "a.rs".to_string(),
        };
        let refusal = trigger::check_request(&tuple).expect_err(repo);
        assert!(refusal.contains("review request's repository"), "{refusal}");
    }
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

/// ★ **A space name the config check passes is one a drop can use.** The check exists to
/// catch at startup what would otherwise fail at the first drop, where nobody is watching. On
/// 4c1caec `my reviews` passed it and every drop then failed (`urn:space:my reviews` is not an
/// IRI), and `a#b` passed and named a different space (`#` starts a fragment). Reproduction
/// from the review-value experiment (ledger #723, unled arm).
#[test]
fn a_space_name_the_check_passes_can_be_dropped_into() {
    for bad in [
        "my reviews",
        "tab\there",
        "a#b",
        "a?b",
        "a%41",
        "a[b]",
        "a<b>",
        "a\"b",
        "a{b}",
        "a|b",
        "a^b",
        "a`b",
    ] {
        let refusal = trigger::check_space_name(bad).expect_err(bad);
        assert!(refusal.contains("gonk.review.space"), "{refusal}");
    }
    for good in ["reviews", "my-reviews", "reviews_2", "révisions"] {
        trigger::check_space_name(good).expect(good);
        let dir = tempfile::tempdir().expect("a temp dir");
        let q = Trigger {
            space: good.to_string(),
            grant: None,
            root: dir.path().join("spaces"),
            arm: false,
        };
        trigger::drop_tuple(
            &q,
            &Tuple {
                repo: "demo".to_string(),
                path: "src/x.rs".to_string(),
            },
        )
        .unwrap_or_else(|e| panic!("`{good}` passed the check and a drop failed: {e}"));
        assert_eq!(
            trigger::pending(&q),
            1,
            "`{good}`: the drop landed in its inbox"
        );
    }
}

/// The spaces tree is created `0700` — the root as well as the space and its inbox, which
/// `SPACES_DIR` says gonk owns. On 4c1caec the root was left at the umask default. Checked on
/// a root that does not exist yet, because a `tempdir` is already `0700`.
#[cfg(unix)]
#[test]
fn the_spaces_tree_is_created_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    for created in [q.root.clone(), q.dir(), q.inbox()] {
        let mode = std::fs::metadata(&created)
            .expect("created")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{} is {mode:o}", created.display());
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
        QueuePolicy::default(),
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
    //
    // ⚠⚠ **Polled on the tuple LANDING, not on the review being CALLED** (ledger #1004). The
    // reactor claims the tuple into `.processing/`, runs the pass, and only THEN moves it to
    // `outbox/` — so the recorder has already seen the call while the tuple is still in
    // `.processing/`, and a test that stopped waiting at the call read `processing: 1,
    // outbox: 0` (2 runs in 400, under 24 concurrent copies of this test). The depth is the
    // condition every assertion below is about, so it is the one waited for.
    //
    // The bound is generous because it only matters on a failure: under that same load the
    // catch-up started anywhere from 0 to 12.5 s after `arm` (8 runs in 400 past the old 10 s),
    // the watch it waits behind being established one process at a time by the OS.
    let handled = trigger::Depth::Counted {
        inbox: 0,
        processing: 0,
        outbox: 1,
        error: 0,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline && trigger::depth(Some(&q)) != handled {
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
        handled,
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
/// Under this server's own reactor the file decides nothing any more
/// ([`a_retargeted_handler_file_is_dead_lettered_never_fired`]); it is the marker other
/// readers of the tree use to tell a reactive space from a plain one. Removed when unarmed
/// anyway, because a reactor some other host runs over the same tree would not have gonk's
/// seam, and would fire whatever the file names.
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
        policy: QueuePolicy::default(),
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
        policy: QueuePolicy::default(),
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
        policy: QueuePolicy::default(),
    };
    let sentence = absent.status().sentence(0);
    assert!(
        sentence.contains("No review queue is configured"),
        "{sentence}"
    );
}

/// ★ **A request a restart caught mid-pass is visible, not an empty queue.**
///
/// `SpaceReactor` claims a tuple by renaming `inbox/<id>.tuple` into `.processing/` and
/// settles it only after the pass returns — about a minute of model call — and gonk is
/// restarted on every reinstall. A restart in that minute leaves the tuple in `.processing/`,
/// the restarted reactor's catch-up reads only the inbox, and on 4c1caec the depth said
/// "The review queue is empty". Reproduction from the review-value experiment (ledger #723,
/// unled arm; item 4 of ledger #737), with the claim done by the reactor's own rename.
/// Retrying it is the next armed START's (ledger #742, and the test after this one); this one
/// is the half where it is SEEN, and where what the sentence promises depends on whether the
/// server reading it is armed.
#[test]
fn a_request_a_restart_caught_mid_pass_is_counted_and_called_lost() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut q = queue(dir.path());
    q.arm = true;
    trigger::prepare(&q).expect("prepare");
    let id = trigger::drop_tuple(
        &q,
        &Tuple {
            repo: "demo".to_string(),
            path: "src/x.rs".to_string(),
        },
    )
    .expect("dropped");
    // The reactor's claim, and then the process dies before it settles.
    let processing = q.dir().join(".processing");
    std::fs::create_dir_all(&processing).expect("staging");
    std::fs::rename(
        q.inbox().join(format!("{id}.tuple")),
        processing.join(format!("{id}.tuple")),
    )
    .expect("claimed");

    assert_eq!(
        trigger::depth(Some(&q)),
        trigger::Depth::Counted {
            inbox: 0,
            processing: 1,
            outbox: 0,
            error: 0
        }
    );
    // The restarted server: armed, nothing in flight in THIS process.
    let activity = Arc::new(trigger::Activity::default());
    let depth = trigger::DepthEndpoint {
        trigger: Some(Arc::new(q.clone())),
        activity: Arc::clone(&activity),
        armed: true,
        policy: QueuePolicy::default(),
    };
    let status = depth.status();
    assert_eq!(status.lost(), 1);
    assert!(status.stuck(), "{status:?}");
    let sentence = status.sentence(10_000);
    assert!(
        sentence.contains("1 request was claimed by a pass that never finished"),
        "{sentence}"
    );
    assert!(
        sentence.contains("its next start requeues it and the catch-up reviews it"),
        "an armed server's next start requeues a lost request (ikigai-intray 0.1.36): \
         {sentence}"
    );
    assert!(!sentence.contains("NOTHING RECOVERS"), "{sentence}");
    assert!(
        !sentence.contains("STUCK reviewer"),
        "the stuck-watcher advice is about a WAITING request, and nothing is waiting: \
         {sentence}"
    );

    // Unarmed after the restart: still lost, still said, and nothing will bring it back.
    let unarmed = trigger::DepthEndpoint {
        trigger: Some(Arc::new(q.clone())),
        activity: Arc::new(trigger::Activity::default()),
        armed: false,
        policy: QueuePolicy::default(),
    };
    let sentence = unarmed.status().sentence(0);
    assert!(
        sentence.contains("NOTHING RECOVERS IT: an unarmed server runs no reactor"),
        "{sentence}"
    );
    assert!(!sentence.contains("next start requeues"), "{sentence}");

    // A pass in flight holds exactly one claimed tuple: that one is working, not lost.
    let pass = activity.begin(5_000).expect("a pass");
    let status = depth.status();
    assert_eq!(status.lost(), 0);
    assert!(!status.stuck(), "{status:?}");
    assert!(!status.sentence(6_000).contains("never finished"));
    pass.succeeded();
}

/// ★★ **The floor test for `ikigai-intray = "0.1.36"`: a request a restart caught mid-pass is
/// requeued at the next armed start and reviewed** (ledger
/// [#742](http://localhost:1060/l/default/item/742), [#738](http://localhost:1060/l/default/item/738)).
///
/// The tuple is put where a reactor that died mid-pass leaves it — `.processing/`, out of the
/// inbox — and then [`trigger::arm`] starts a fresh reactor over the tree. Through 0.1.35 the
/// catch-up read only `inbox/` and nothing ever reached the review. On 0.1.36 with the crate's
/// default (`Interrupted::DeadLetter`) the tuple would land in `error/` and the review would
/// not be called; with gonk's `Interrupted::Requeue` it goes back to the inbox, the catch-up
/// reviews it exactly once, and it settles in the outbox. The banner sentence `arm` returns
/// names it.
#[test]
fn a_request_a_restart_caught_mid_pass_is_requeued_and_reviewed() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut q = queue(dir.path());
    q.arm = true;
    trigger::prepare(&q).expect("prepare");
    let id = trigger::drop_tuple(
        &q,
        &Tuple {
            repo: "demo".to_string(),
            path: "a.rs".to_string(),
        },
    )
    .expect("dropped");
    // The dead reactor's claim: the same rename `SpaceReactor` makes, and no settle after it.
    let processing = q.dir().join(ikigai_intray::PROCESSING_DIR);
    std::fs::create_dir_all(&processing).expect("staging");
    std::fs::rename(
        q.inbox().join(format!("{id}.tuple")),
        processing.join(format!("{id}.tuple")),
    )
    .expect("claimed");
    assert_eq!(
        trigger::pending(&q),
        0,
        "the inbox reads empty, which was the bug"
    );

    let (kernel, seen) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let hub = Arc::new(kernel);
    let scopes = trigger::reviewer_grant_shape(&hub, "urn:repo:demo:review:a.rs", "localhost")
        .expect("a bound review");
    let banner = trigger::arm(&q, Arc::clone(&hub), &scopes).expect("arming");
    assert!(
        banner.contains("Requeued 1 request a stopped pass had claimed") && banner.contains(&id),
        "the banner names what recovery requeued: {banner}"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline
        && trigger::depth(Some(&q))
            != (trigger::Depth::Counted {
                inbox: 0,
                processing: 0,
                outbox: 1,
                error: 0,
            })
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let calls = seen.lock().expect("not poisoned").clone();
    assert_eq!(
        calls.len(),
        1,
        "the interrupted request must reach the review exactly once: {calls:?}"
    );
    assert_eq!(calls[0].1, "urn:repo:demo:review:a.rs");
    assert_eq!(
        trigger::depth(Some(&q)),
        trigger::Depth::Counted {
            inbox: 0,
            processing: 0,
            outbox: 1,
            error: 0
        },
        "requeued and handled — not dead-lettered, which is the crate's default"
    );
}

/// An armed start with nothing interrupted says so, rather than saying nothing.
#[test]
fn an_armed_start_with_nothing_interrupted_says_so() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut q = queue(dir.path());
    q.arm = true;
    trigger::prepare(&q).expect("prepare");
    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let hub = Arc::new(kernel);
    let scopes = trigger::reviewer_grant_shape(&hub, "urn:repo:demo:review:a.rs", "localhost")
        .expect("a bound review");
    let banner = trigger::arm(&q, hub, &scopes).expect("arming");
    assert_eq!(banner, "No request was interrupted by the last stop.");
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
    assert_eq!(v["in_processing"], 0);
    assert_eq!(v["lost"], 0);
    assert_eq!(v["stuck"], false);
    // ★ The sentence rides WITH the numbers, so the badge, the page and the socket cannot
    // disagree about what the queue is doing.
    assert!(v["sentence"].as_str().is_some_and(|s| !s.is_empty()), "{v}");
}

// ------------------------------------------- 5. the serious share: ledger #496

/// ★ **The serious share is a number this host reports, because the Queue page now filters
/// on a label the model wrote.** Ledger [#449](http://localhost:1060/l/default/item/449)
/// measured a prompt moving that share 27% → 62% by re-labelling; a gate on the word makes
/// the word load-bearing, and this is the tripwire.
///
/// Three things pinned: the labels are read off the pass's OWN answer and only a derived one
/// (an archive hit replays old labels and counts nothing); the depth turns them into a share
/// against the configured set; and the JSON face carries the histogram beside it, for the
/// daily scan that watches it.
#[test]
fn the_depth_reports_the_serious_share_of_what_this_run_minted() {
    let policy = QueuePolicy::default();
    let serious = policy.serious[0].clone();
    let derived = format!(
        r#"{{"derived":true,"minted":["a","b","c"],"annotations":[{{"severity":"{serious}"}},{{"severity":"zzz"}},{{"severity":null}}]}}"#
    );
    assert_eq!(
        trigger::minted_labels(derived.as_bytes()),
        vec![
            serious.clone(),
            "zzz".to_string(),
            trigger::UNRATED.to_string()
        ]
    );
    let hit = derived.replace(r#""derived":true"#, r#""derived":false"#);
    assert!(
        trigger::minted_labels(hit.as_bytes()).is_empty(),
        "an archive hit replays labels an earlier pass chose"
    );
    assert!(trigger::minted_labels(b"not json").is_empty());

    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let activity = Arc::new(trigger::Activity::default());
    let kernel = Kernel::new(Arc::new(Fallback::new(trigger::space(
        &q,
        Arc::clone(&activity),
        true,
        policy.clone(),
    ))));
    let reader = Capability::scoped([ikigai_browse::CAP_WILDCARD]);
    let depth = |as_json: bool| {
        let mut request = Request::new(Verb::Source, Iri::parse(trigger::DEPTH).unwrap());
        if as_json {
            request = request.with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
        }
        String::from_utf8(issue(&kernel, request, &reader).expect("the depth").bytes).unwrap()
    };

    // Nothing minted yet: no share, and no sentence about one — "0 of 0" is not a number.
    let idle: serde_json::Value = serde_json::from_str(&depth(true)).expect("json");
    assert_eq!(idle["findings_this_run"], 0);
    assert!(idle["serious_share_percent"].is_null(), "{idle}");
    assert!(!depth(false).contains("minted this run"));

    activity
        .begin(1_000)
        .expect("a pass")
        .succeeded_with(&trigger::minted_labels(derived.as_bytes()));
    let after: serde_json::Value = serde_json::from_str(&depth(true)).expect("json");
    assert_eq!(after["findings_this_run"], 3);
    assert_eq!(after["serious_this_run"], 1);
    assert_eq!(after["serious_share_percent"], 33);
    assert_eq!(after["findings_by_severity"][&serious], 1);
    assert_eq!(after["findings_by_severity"]["zzz"], 1);
    assert_eq!(after["findings_by_severity"][trigger::UNRATED], 1);
    // The set the share was read against rides with it, so a reader of the number knows
    // what "serious" meant on this server.
    assert_eq!(
        after["serious"],
        serde_json::json!(policy.serious),
        "{after}"
    );
    let sentence = depth(false);
    assert!(
        sentence.contains("3 findings minted this run, 1 serious (33%)"),
        "{sentence}"
    );
}

/// ★ **A retargeted `handler` file does not fire its target** (ledger #887, from the Hermes
/// audit's `handler-retarget`, ledger #877).
///
/// The file lives in the directory a dropper writes into. Here it is rewritten to name the
/// review itself — a resource the reviewer's capability CAN reach, so nothing but the handler
/// decision stands between the rewrite and a call. Through ikigai-intray 0.1.40 the reactor
/// fired whatever the file named; under `with_host_handler` ([`trigger::fires`]) the tuple is
/// refused and dead-lettered with a note naming the target, and nothing is called.
#[test]
fn a_retargeted_handler_file_is_dead_lettered_never_fired() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut q = queue(dir.path());
    q.arm = true;
    trigger::prepare(&q).expect("prepare");
    let review = "urn:repo:demo:review:a.rs";
    let (kernel, seen) = kernel_with_recorder(&q, review);
    let hub = Arc::new(kernel);
    let scopes = trigger::reviewer_grant_shape(&hub, review, "localhost").expect("a bound review");
    trigger::arm(&q, Arc::clone(&hub), &scopes).expect("arming");

    // Someone with the drop tree retargets the handler, then drops.
    std::fs::write(q.dir().join("handler"), format!("{review}\n")).expect("a retarget");
    let tuple = Tuple {
        repo: "demo".to_string(),
        path: "a.rs".to_string(),
    };
    trigger::drop_tuple(&q, &tuple).expect("a drop");

    let settled = |depth: &trigger::Depth| matches!(depth, trigger::Depth::Counted { outbox, error, .. } if outbox + error > 0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !settled(&trigger::depth(Some(&q))) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        trigger::depth(Some(&q)),
        trigger::Depth::Counted {
            inbox: 0,
            processing: 0,
            outbox: 0,
            error: 1
        },
        "the tuple is dead-lettered, not handled"
    );
    assert!(
        seen.lock().expect("not poisoned").is_empty(),
        "the retargeted handler's target must never be called: {:?}",
        seen.lock().expect("not poisoned")
    );
    let note = std::fs::read_dir(q.dir().join("error"))
        .expect("an error directory")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .find(|path| path.extension().is_some_and(|ext| ext == "err"))
        .map(|path| std::fs::read_to_string(path).expect("a note"))
        .expect("a dead-letter note");
    assert!(
        note.contains("refused") && note.contains(review),
        "the note names the refused target: {note}"
    );
}
