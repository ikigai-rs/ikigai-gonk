//! The git-event review trigger: the queue, the pass, and the grant that is missing on
//! purpose. Ledger #261.
//!
//! # What this file is actually asserting
//!
//! Three things, and only the first is about code that runs:
//!
//! 1. **The trigger's call is the button's call.** `ikigai-browse` pinned the other half —
//!    `the_button_and_a_trigger_are_one_call_with_two_causes` derives the button's exact
//!    call and asserts the trigger's is an archive HIT on it. This file pins gonk's half: the
//!    Request `crate::trigger` builds is `Source urn:repo:{repo}:review:{path}` with
//!    `as=application/json` and nothing else, and the pass endpoint issues exactly that and
//!    returns the answer whole.
//! 2. **The queue is a queue.** Tuples are files, an identical request collapses to one
//!    tuple, and what is in the inbox is still there for a process that starts later.
//! 3. ⚠ **The trigger is unarmed, and the test says so out loud.**
//!    [`the_trigger_is_unarmed_and_this_test_is_what_changes_when_444_lands`] asserts that no
//!    grant this server can MINT carries the authority a pass needs. Brian, 2026-09-19:
//!    *"Nothing gets published to Gonk except by the human."* A pass mints its findings as
//!    annotations as its terminal step, so until ledger #444 gives a finding a pending state,
//!    granting that authority to an unattended drainer would publish unattended. That test is
//!    the thing a #444 arc has to change deliberately rather than drift past.

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
    }
}

/// The exact scopes a review pass needs, as `ikigai-browse` declares them.
fn reviewer() -> Capability {
    Capability::scoped(trigger::reviewer_grant_shape("localhost"))
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
        // The same three capabilities the real review declares, so a capability that would
        // be refused by `ikigai-browse` is refused here too.
        Description::new("recorder")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .requires(grants::CAP_NET_ANY)
            .requires(ikigai_browse::CAP_ANNOTATE)
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
    let mut spaces = trigger::space(trigger_queue);
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
        &reviewer(),
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
/// ⚠ All three are needed EVEN FOR A FREE ARCHIVE HIT: `review` declares them flatly on its
/// `Description`, so the kernel checks them before the endpoint runs and long before it
/// looks in the archive. There is no cheaper grant for the cheap case.
#[test]
fn a_pass_is_denied_short_of_any_one_of_the_three() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, seen) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let tuple = Tuple {
        repo: "demo".to_string(),
        path: "a.rs".to_string(),
    };
    let full = trigger::reviewer_grant_shape("localhost");
    for dropped in [
        ikigai_browse::CAP_WILDCARD,
        "urn:cap:net:localhost",
        ikigai_browse::CAP_ANNOTATE,
    ] {
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

/// ⚠⚠ **THE TRIGGER IS UNARMED, AND THIS TEST IS WHAT A #444 ARC HAS TO CHANGE.**
///
/// Brian, 2026-09-19: *"Nothing gets published to Gonk except by the human."* A review pass
/// mints its findings as annotations as its terminal step, so an unattended drainer would
/// publish unattended. What stops it is not a flag — it is that **no grant this server can
/// mint carries the authority a pass needs**, so there is nothing to run one under and no
/// drainer in this binary at all.
///
/// When ledger #444 gives a finding a pending state, this assertion is the thing that must
/// be consciously revisited, rather than a comment that drifts.
#[test]
fn the_trigger_is_unarmed_and_this_test_is_what_changes_when_444_lands() {
    let needed = trigger::reviewer_grant_shape("localhost");
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
    ] {
        assert!(
            !mintable.contains(&token.to_string()),
            "`{token}` is now mintable by this server's own provisioning. That is what arms \
             a headless review pass, and a pass publishes its findings the moment it runs — \
             so this needs ledger #444 (a pending state for a finding) first, not a flag"
        );
    }
    // The two store tokens ARE mintable (`--browse-graph write`), which is the point: three
    // of the five scopes a pass needs have no flag at all. Ledger #435.
    for token in grants::browse_graph_grants(Authority::Write).expect("the browse graph") {
        assert!(
            needed.contains(&token),
            "the reviewer grant's shape must be the browse graph's own tokens, not a \
             transcription of them"
        );
    }
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
        &reviewer(),
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

/// The reviewer grant's shape is COMPUTED from the crates that enforce it, never typed — so
/// a spelling cannot drift from the token that is checked.
#[test]
fn the_reviewer_grant_shape_is_computed_from_the_crates_that_enforce_it() {
    let scopes = trigger::reviewer_grant_shape("localhost");
    assert!(scopes.contains(&ikigai_browse::CAP_WILDCARD.to_string()));
    assert!(scopes.contains(&ikigai_browse::CAP_ANNOTATE.to_string()));
    assert!(scopes.contains(&"urn:cap:net:localhost".to_string()));
    assert_eq!(scopes.len(), 5, "three browse tokens and the graph's two");
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
}

// ---------------------------------------------------- 6. what the doors are offered

/// ★ **A configured trigger adds exactly two resources and no more.**
///
/// The sibling of `tests/conformance.rs`'s catalog pin, from the other side: that one holds
/// the catalog of a gonk with no trigger, this one holds the DELTA a `gonk.review.space`
/// line makes. `ikigai-intray` binds one template and this crate binds one endpoint; a
/// version that bound a third would be behind every door this binary opens, silently.
#[test]
fn a_configured_trigger_adds_exactly_the_queue_and_the_pass() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let kernel = Kernel::new(Arc::new(Fallback::new(trigger::space(&q))));
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
        ["gonk-review-pass".to_string(), "space".to_string()],
        "the trigger's whole served surface"
    );
}

/// The pass declares the three capabilities it can never do without, and says it Sources.
///
/// ⚠ Declaring NOTHING would be an over-offer — the manifold would say any caller may run a
/// pass, and the refusal would arrive one hop in, from a resource the caller never named.
#[test]
fn the_pass_declares_what_a_review_declares() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let q = queue(dir.path());
    trigger::prepare(&q).expect("prepare");
    let (kernel, _) = kernel_with_recorder(&q, "urn:repo:demo:review:a.rs");
    let described = kernel
        .describe(&Iri::parse(trigger::PASS).unwrap())
        .expect("the pass describes itself");
    let mut requires = described.requires.clone();
    requires.sort();
    assert_eq!(
        requires,
        [
            ikigai_browse::CAP_ANNOTATE.to_string(),
            ikigai_browse::CAP_WILDCARD.to_string(),
            grants::CAP_NET_ANY.to_string(),
        ]
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>(),
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
