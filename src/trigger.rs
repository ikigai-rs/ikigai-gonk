//! The git-event review trigger: a queue, a pass, and a deliberately missing grant.
//!
//! ```text
//! a git hook  ->  ikigai-gonk review request <repo> <path>   drops a TUPLE (no server, no grant)
//!                 urn:space:{name}                           the queue: rd / out / take
//!                 urn:iki:gonk:review:pass                   ONE tuple -> ONE review pass
//! ```
//!
//! Ledger #261. What is new here is the QUEUE and the ADAPTER between a tuple and a review;
//! the review itself is `ikigai-browse`'s `urn:repo:{repo}:review:{path}`, unchanged and
//! un-wrapped.
//!
//! # ★ The invariant: one call, two causes
//!
//! Brian, 2026-09-19: *"There shouldn't be any real difference between the automated reviews
//! that we're about to trigger and the UI-driven ones."* `ikigai-browse` pinned that from its
//! own side — `the_button_and_a_trigger_are_one_call_with_two_causes` derives the button's
//! exact call and then asserts that the trigger's exact call is an archive HIT on it: same
//! version tag, same minted set, nothing paid.
//!
//! So this module does not assemble a prompt, does not post-process a finding, and does not
//! write an annotation. [`review_request`] builds the one Request the trigger issues, and it
//! is the trigger side of that test spelled in gonk:
//!
//! ```text
//! Source  urn:repo:{repo}:review:{path}   as=application/json
//! ```
//!
//! — the same verb, the same IRI, one argument, no `provider`. It is a pure function so the
//! claim is a unit test over a value rather than a belief about a call site.
//!
//! # ⚠⚠ THE TRIGGER IS COMPLETE AND DELIBERATELY UNARMED
//!
//! Brian, 2026-09-19: **"Nothing gets published to Gonk except by the human."** Not only
//! critical findings — nothing. And today a review pass mints its annotations as its
//! terminal step, so a pass that ran with no human present would publish with no human
//! present.
//!
//! ★ **The thing that stops it is the authority, not a flag.** A pass needs
//! `urn:cap:annotate`, `urn:cap:net:*` and `urn:cap:browse:read:*`, and this server mints
//! none of them for anybody. There is no background worker in this binary: the queue fills,
//! and a person drains it one tuple at a time over the owner-only socket, which is a human
//! publishing. See [`DRAIN_ONE`].
//!
//! What arms it is **ledger #444** — a pending state for a finding, which is a change in
//! `ikigai-browse` (where the minting is) and in gonk's queue UI. Not a config key someone
//! flips. When it lands, the automatic drainer is `ikigai-intray`'s `SpaceReactor` over this
//! same space with a `handler` file naming [`PASS`] — and [`PassEndpoint::invoke`] is still
//! the single place a pass completes, so that is the one call site a pending state changes.
//!
//! # ⚠ Why the reactor's own `cap` file is not how this server grants authority
//!
//! `SpaceReactor::capability_for` reads `<root>/{space}/cap` and builds
//! `Capability::scoped` from its lines — the TRUSTED minting path, not an attenuation, so
//! that file REPLACES the reactor's default and can exceed it without limit. And it sits in
//! the same directory, with the same owner and mode, as the `inbox/` a dropper writes into:
//! anything that can drop a tuple can rewrite the authority the tuple runs under, and
//! nothing validates the result.
//!
//! gonk refuses the broad store tokens, the offering wildcards and the backup family's
//! tokens on every certificate and every passkey it admits ([`crate::quic::check_grants`]),
//! at startup and again per use. A second authority file with none of those checks would be
//! the widest grant in the server and the one nothing reads. So this server names a GRANT
//! (`gonk.review.grant`, resolved through [`reviewer_scopes`]) and refuses to run a trigger
//! beside a `cap` file at all ([`refuse_cap_file`]).
//!
//! # The tuple
//!
//! Turtle, because the intray's associative match is a SPARQL ASK over the tuple's graph and
//! a non-RDF tuple can never be selected by one. [`tuple_turtle`] is the only writer, and it
//! is byte-deterministic on purpose: the intray content-addresses a drop, so the SAME
//! (repo, path) dropped twice lands on one filename and the inbox holds one tuple.
//!
//! ⚠ **That is why the tuple does not carry the commit.** A SHA or a timestamp would make
//! every drop unique and the queue would grow instead of collapsing. The cost is real and is
//! recorded on #261: the commit->pass edge cannot come out of the tuple. What falls out
//! instead is better — the content hash is read by the pass from the LIVE tree, so three
//! commits to one file while the queue waits collapse to one pass over the final state,
//! which is exactly what a person clicking `review` would get.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, Error, Invocation, Iri, Representation, Request, Result, Space,
    Verb,
};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::store::Store;

/// `urn:iki:gonk:review:pass` — one tuple, one review pass.
pub const PASS: &str = "urn:iki:gonk:review:pass";

/// The directory under the data home that holds the spaces tree.
///
/// ⚠ **Not `~/.ikigai/workspace/spaces`**, which is the cli's. gonk owns this one, creates
/// it `0700`, and is the only writer of anything in it but `inbox/`.
pub const SPACES_DIR: &str = "spaces";

/// The XSD datatype every scalar here declares — a claim about the wire, not the grammar.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The face the trigger asks for: the machine one, exactly as `ikigai-browse`'s own
/// `the_button_and_a_trigger_are_one_call_with_two_causes` asks for it.
const JSON: &str = "application/json";

/// How a person runs one queued request, `{space}` left for the caller to fill — printed by
/// the banner and repeated in the README, from here so the two cannot drift.
///
/// ★ It READS the tuple rather than taking it (`source`, not `delete`), so the request stays
/// queued until a person says it is done, and a pass is spent only when someone asks for
/// one. That IS "nothing gets published to gonk except by the human".
///
/// ⚠ The later stages of an engine pipeline are BARE IRIs — `| urn:iki:gonk:review:pass`,
/// never `| source urn:…`, which fails with "invalid IRI: No scheme found" because the word
/// `source` is parsed as the stage's target. Measured against a live socket door,
/// 2026-09-19.
pub const DRAIN_ONE: &str = "source urn:space:{space} tuple=ID | urn:iki:gonk:review:pass";

// ------------------------------------------------------------------- settings

/// What an operator configured, or `None` when no `gonk.review.space` line exists.
///
/// ★ The whole feature is one line, the same switch shape as `gonk.mount`: with no line
/// this server binds no space, offers no pass, and serves exactly the catalog it served
/// before. `declared = enforced` runs both ways — an unconfigured gonk must not advertise a
/// queue nothing fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// The space name — one segment, the `{name}` of `urn:space:{name}`.
    pub space: String,
    /// The grant in `grants.json` a pass would run under, when one is named.
    ///
    /// ⚠ Nothing in this binary runs a pass under it today; the banner prints what it
    /// resolves to so an operator can SEE the authority they have written down before
    /// anything can use it.
    pub grant: Option<String>,
    /// The spaces tree, `<data home>/spaces` unless `gonk.review.root` says otherwise.
    pub root: PathBuf,
}

impl Trigger {
    /// This trigger's own directory, `<root>/{space}`.
    pub fn dir(&self) -> PathBuf {
        self.root.join(&self.space)
    }

    /// Where a dropped tuple lands.
    pub fn inbox(&self) -> PathBuf {
        self.dir().join("inbox")
    }
}

/// A space name is the `{name}` of `urn:space:{name}` — one segment, never a path.
///
/// Checked here, at configuration time, rather than at first drop: `ikigai-intray` refuses
/// the same shapes inside an invocation, which would make a typo in a config file surface
/// as a failing tuple nobody is watching.
pub fn check_space_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() || name.contains(['/', '\\', ':', '.']) {
        return Err(format!(
            "gonk.review.space: `{name}` is not a space name — one segment, no `/ \\ : .` \
             (it is the `{{name}}` of `urn:space:{{name}}`)"
        ));
    }
    Ok(())
}

/// ⚠ **Refuse to run a trigger beside a `SpaceReactor` `cap` file.**
///
/// `<root>/{space}/cap` is `ikigai-intray`'s own authority file, and it MINTS rather than
/// attenuates: whatever scopes it lists become the capability a handler runs under,
/// bypassing every refusal [`crate::quic::check_grants`] applies to a certificate or a
/// passkey. It also lives one directory above the `inbox/` any dropper can write.
///
/// gonk never writes one, so its presence means either an operator put it there or
/// something that can drop tuples did. Both are worth stopping for, and stopping is cheap:
/// the file is checked once at startup, before the store is open and before a door is bound.
pub fn refuse_cap_file(trigger: &Trigger) -> std::result::Result<(), String> {
    let cap = trigger.dir().join("cap");
    if cap.exists() {
        return Err(format!(
            "{} exists. That file is `ikigai-intray`'s reactor grant, and it MINTS a \
             capability rather than narrowing one — it would give a handler authority this \
             server refuses on every certificate and every passkey it admits, from a file \
             in the same directory as the inbox anything can drop into. gonk names a GRANT \
             instead (`gonk.review.grant`); remove this file",
            cap.display()
        ));
    }
    Ok(())
}

/// The scopes a named grant carries, through the same checked path every door uses.
///
/// ★ This is the authority decision of ledger #261 in one function: a headless pass runs
/// under a grant an operator WROTE DOWN in `grants.json`, beside the passkey grant a click
/// borrows — not under root, not under the dropper's, and not under anything this server
/// minted for itself. [`crate::quic::scopes_for_grant`] fails closed on an unknown grant,
/// an empty one, a broad store token and an offering wildcard.
pub fn reviewer_scopes(
    grants: &std::collections::BTreeMap<String, Vec<String>>,
    grant: &str,
) -> std::result::Result<Vec<String>, String> {
    crate::quic::scopes_for_grant(grants, grant)
}

/// The five scopes a review pass actually needs, for an operator writing the grant by hand.
///
/// ⚠ `net` is deliberately the NARROW form. `urn:cap:net:*` is the OFFERING wildcard
/// `ikigai-browse` declares on every derivation, and as a grant it means every host this
/// kernel could dial; [`crate::quic::check_grants`] refuses it. `host` is where the mounted
/// peer lives.
///
/// ⚠ And all five are needed EVEN WHEN THE PASS IS A FREE ARCHIVE HIT. `review` declares its
/// three capabilities flatly on its `Description`, so the kernel checks them before the
/// endpoint runs and long before it looks in the archive. There is no cheaper grant for the
/// cheap case.
pub fn reviewer_grant_shape(host: &str) -> Vec<String> {
    let mut scopes = vec![
        ikigai_browse::CAP_WILDCARD.to_string(),
        format!("urn:cap:net:{host}"),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ];
    if let Ok(graph) = crate::grants::browse_graph_grants(crate::grants::Authority::Write) {
        scopes.extend(graph);
    }
    scopes
}

// ---------------------------------------------------------------------- the tuple

/// One queued request: which repository, and which file in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuple {
    /// A configured `gonk.browse.root` name — the `{repo}` of `urn:repo:{repo}:…`.
    pub repo: String,
    /// The path within that root, as `git` spells it.
    pub path: String,
}

impl Tuple {
    /// The tuple's own name. Skolemized, never a blank node — the module recipe's rule, and
    /// the reason a queued request can be joined to anything else in a query.
    pub fn iri(&self) -> String {
        format!("urn:iki:gonk:review:request:{}:{}", self.repo, self.path)
    }
}

/// The ONE writer of a tuple's bytes, and the exact bytes a git hook produces.
///
/// ★ **Byte-deterministic on purpose.** `ikigai-intray` content-addresses a drop — the file
/// is named for the hash of its content — so identical bytes for the same (repo, path) mean
/// the inbox holds exactly one tuple however many times a hook fires. A timestamp, a commit
/// SHA or a re-serialization through a graph library would each break that silently, which
/// is why this is a `format!` and not a Turtle serializer.
///
/// ⚠ The two predicates are `ikigai-browse`'s own (`ik:repo`, `ik:path`, the same ones every
/// annotation carries), so nothing here mints a vocabulary term. A `ReviewRequest` CLASS
/// would be a vocabulary change and is reported rather than invented.
///
/// ```
/// use ikigai_gonk::trigger::{tuple_turtle, Tuple};
/// let t = Tuple { repo: "ikigai-gonk".into(), path: "src/k.rs".into() };
/// assert_eq!(
///     tuple_turtle(&t),
///     "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
///      <urn:iki:gonk:review:request:ikigai-gonk:src/k.rs> ik:repo \"ikigai-gonk\" ; ik:path \"src/k.rs\" .\n"
/// );
/// ```
pub fn tuple_turtle(tuple: &Tuple) -> String {
    format!(
        "@prefix ik: <{IK}> .\n<{iri}> ik:repo \"{repo}\" ; ik:path \"{path}\" .\n",
        IK = IK_NS,
        iri = tuple.iri(),
        repo = tuple.repo,
        path = tuple.path,
    )
}

/// The vocabulary namespace the tuple's two predicates come from.
const IK_NS: &str = "https://ikigai-rs.dev/ns#";

/// ⚠ **Refuse a request this module cannot express, rather than writing a tuple nothing can
/// read back.**
///
/// [`tuple_turtle`] is a `format!` and not a Turtle serializer, deliberately — a serializer
/// would be free to reorder or re-quote and would break the byte-determinism the queue's
/// deduplication rests on. The price of that choice is that it does no escaping, so a `"`,
/// a `\` or a newline in a path would emit Turtle that [`parse_tuple`] then refuses. A git
/// path may legally contain all three.
///
/// A poison tuple is the worst outcome available here: the drop succeeds, the hook reports
/// nothing, and a request sits in the inbox failing every time anyone runs it. So the check
/// is at the DROP, where a person is still watching, and it refuses rather than mangling —
/// a bound refuses, it does not truncate.
///
/// The set is the union of what Turtle's quoted literal and an IRI each forbid, so one rule
/// covers both positions a value is emitted in.
///
/// # Errors
///
/// When either field is empty or carries a character this module cannot emit.
pub fn check_request(tuple: &Tuple) -> std::result::Result<(), String> {
    for (field, value) in [("repository", &tuple.repo), ("path", &tuple.path)] {
        if value.is_empty() {
            return Err(format!("a review request's {field} may not be empty"));
        }
        if let Some(bad) = value
            .chars()
            .find(|c| c.is_whitespace() || c.is_control() || "\"\\<>{}|^`".contains(*c))
        {
            return Err(format!(
                "a review request's {field} may not contain `{}`: this server emits a \
                 request as Turtle with a `urn:iki:gonk:review:request:` IRI, and that \
                 character is legal in neither. Refused here rather than queued as a tuple \
                 nothing can read back",
                bad.escape_default()
            ));
        }
    }
    Ok(())
}

/// Read a tuple back.
///
/// Parsed as RDF rather than by string surgery, so a tuple dropped by something other than
/// [`tuple_turtle`] — a hand-written one, a richer one a later arc adds fields to — still
/// resolves, and so a tuple that is not RDF at all fails with an explanation instead of a
/// wrong repository name.
///
/// # Errors
///
/// When the bytes are not Turtle, or carry no `ik:repo`/`ik:path` pair on one subject, or
/// carry more than one such subject — a tuple is ONE request, because the queue's whole
/// bound is that a pass consumes one tuple.
pub fn parse_tuple(bytes: &[u8]) -> Result<Tuple> {
    let store = Store::new().map_err(|e| Error::Endpoint(format!("tuple: store init: {e}")))?;
    store
        .load_from_slice(RdfParser::from_format(RdfFormat::Turtle), bytes)
        .map_err(|e| Error::InvalidArgument {
            name: "content".to_string(),
            detail: format!("a review tuple is Turtle naming one `ik:repo` and one `ik:path`: {e}"),
        })?;
    let mut found: Vec<Tuple> = Vec::new();
    let repo_p = oxigraph::model::NamedNode::new(format!("{IK_NS}repo"))
        .map_err(|e| Error::Endpoint(format!("tuple: {e}")))?;
    let path_p = oxigraph::model::NamedNode::new(format!("{IK_NS}path"))
        .map_err(|e| Error::Endpoint(format!("tuple: {e}")))?;
    for quad in store.quads_for_pattern(None, Some(repo_p.as_ref()), None, None) {
        let quad = quad.map_err(|e| Error::Endpoint(format!("tuple: {e}")))?;
        let Some(repo) = literal(&quad.object) else {
            continue;
        };
        let path = store
            .quads_for_pattern(
                Some(quad.subject.as_ref()),
                Some(path_p.as_ref()),
                None,
                None,
            )
            .filter_map(|q| q.ok())
            .find_map(|q| literal(&q.object));
        if let Some(path) = path {
            found.push(Tuple { repo, path });
        }
    }
    found.sort_by(|a, b| (&a.repo, &a.path).cmp(&(&b.repo, &b.path)));
    found.dedup();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(Error::InvalidArgument {
            name: "content".to_string(),
            detail: "this tuple names no `ik:repo` and `ik:path` pair — it is not a review \
                     request"
                .to_string(),
        }),
        n => Err(Error::InvalidArgument {
            name: "content".to_string(),
            detail: format!(
                "this tuple names {n} review requests. A tuple is ONE request: the queue's \
                 whole bound is that a pass consumes one tuple at a time"
            ),
        }),
    }
}

fn literal(term: &oxigraph::model::Term) -> Option<String> {
    match term {
        oxigraph::model::Term::Literal(l) => Some(l.value().to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------- the pass

/// ★★ **THE INVARIANT, as a value.**
///
/// The one Request a trigger issues, built here so that "the trigger and the button are the
/// same call" is a unit test over a value rather than a claim about a call site. It is the
/// trigger side of `ikigai-browse`'s own
/// `the_button_and_a_trigger_are_one_call_with_two_causes`, which derives the button's exact
/// call and then asserts this one is an archive HIT on it — same version tag, same minted
/// annotations, nothing paid.
///
/// ⚠ **One argument, and no `provider`.** A `provider=` would fold a different model
/// identity into the archive key and into every finding's `dcterms:creator`, so a trigger
/// that picked one would key a different entry from the button's. When a trigger should
/// choose, the derivation-free `urn:repo:{repo}:review-options:{path}` is where the choice
/// comes from — it requires the browse grant alone — and that is a later arc, not a default.
///
/// # Errors
///
/// When `repo` and `path` do not make a resolvable IRI — a path carrying a space, say.
pub fn review_request(repo: &str, path: &str) -> Result<Request> {
    let iri = Iri::parse(format!("urn:repo:{repo}:review:{path}")).map_err(|e| {
        Error::InvalidArgument {
            name: "path".to_string(),
            detail: format!("`urn:repo:{repo}:review:{path}` is not a resolvable IRI: {e}"),
        }
    })?;
    Ok(Request::new(Verb::Source, iri)
        .with_arg("as", ikigai_core::ArgRef::Inline(JSON.as_bytes().to_vec())))
}

/// `urn:iki:gonk:review:pass` — the adapter between a queued tuple and a review.
///
/// # ★ This is the single place a pass completes
///
/// Whatever causes it — a person piping a tuple in today, a `SpaceReactor` firing it when
/// ledger #444 has given a finding a pending state — a pass reaches its end HERE, in
/// [`PassEndpoint::invoke`], at the one `inv.issue` below. A pending state changes what this
/// function does with the pass's answer; it changes nothing else in this module and nothing
/// at all in the queue.
///
/// # Capability
///
/// The caller's, unchanged and unwidened: `inv.issue` carries it into the review. This
/// endpoint declares exactly what the review declares — browse read, net, annotate — because
/// it can never do less and must not advertise that it might. Declaring nothing would be an
/// over-offer that fails one hop in; declaring more would refuse callers the review accepts.
pub struct PassEndpoint;

#[async_trait]
impl Endpoint for PassEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "gonk-review-pass answers Source (run one queued tuple), not {:?}",
                inv.request.verb
            )));
        }
        // The tuple arrives as `content` — the name a pipe's value lands in, and the name
        // `ikigai-intray`'s reactor passes it under.
        let bytes = inv
            .inline_arg("content")
            .map_err(|_| Error::MissingArgument("content".to_string()))?;
        let tuple = parse_tuple(bytes)?;
        let request = review_request(&tuple.repo, &tuple.path)?;
        // ★ THE ONE CALL. Nothing before it builds a prompt; nothing after it touches a
        // finding. The review's own answer is the answer, whole.
        let answer = inv.issue(request).await?;
        Ok(answer)
    }

    fn name(&self) -> &str {
        "gonk-review-pass"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-review-pass")
            .title("Run one queued review request")
            .summary(
                "Reads a review tuple (Turtle naming one `ik:repo` and one `ik:path`) and \
                 resolves `urn:repo:{repo}:review:{path}` with `as=application/json` and \
                 nothing else — the same verb, IRI and arguments the Review button sends, so \
                 a triggered pass and a clicked one share an archive key and a set of minted \
                 annotations. Assembles no prompt and writes no annotation of its own: the \
                 answer is the review's, whole.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            // ⚠ `urn:cap:net:*` is the OFFERING wildcard browse declares on every
            // derivation, and browse does not re-export its constant — so it is taken from
            // this server's own `grants::CAP_NET_ANY`, which is the string `grants.json` is
            // checked against, rather than typed a third time.
            .requires(crate::grants::CAP_NET_ANY)
            .requires(ikigai_browse::CAP_ANNOTATE)
            .input(
                ArgSpec::new("content")
                    .class(XSD_STRING)
                    .summary("the tuple to run — where a piped value lands"),
            )
            .input(
                ArgSpec::new("in")
                    .optional()
                    .class(XSD_STRING)
                    .summary("the tuple, under the name the space reactor also offers it by"),
            )
            .input(
                ArgSpec::new("space").optional().class(XSD_STRING).summary(
                    "which space the tuple came from; recorded by the caller, unused here",
                ),
            )
            .input(
                ArgSpec::new("tuple")
                    .optional()
                    .class(XSD_STRING)
                    .summary("the tuple's id; recorded by the caller, unused here"),
            )
            .output(JSON)
    }
}

// ---------------------------------------------------------------------- composition

/// The trigger's space: the queue at `urn:space:{name}` and the pass in front of it.
///
/// ⚠ **The queue's three tokens are minted by nobody.** `urn:cap:space:{out,read,take}` have
/// no flag in this server's provisioning and appear in no grant it writes, so the HTTP door
/// and the QUIC door both answer a typed `Denied` — the queue is reachable from the
/// owner-only socket, which is the door a person drains it through.
pub fn space(trigger: &Trigger) -> Vec<Arc<dyn Space>> {
    vec![
        Arc::new(
            ikigai_core::EndpointSpace::new().bind(ikigai_core::Exact::new(PASS), PassEndpoint),
        ) as Arc<dyn Space>,
        Arc::new(ikigai_intray::space(trigger.root.clone())) as Arc<dyn Space>,
    ]
}

/// Create the trigger's directories, `0700`, before anything can write into them.
///
/// The inbox exists up front for the reason `ikigai-intray`'s own watcher creates it up
/// front: a directory that appears later races whatever writes into it, and the FIRST tuple
/// a space ever receives is the one most likely to be lost. gonk has a second reason —
/// `review request` writes into the inbox with no server running, so the tree must be there
/// whether or not this process has started.
pub fn prepare(trigger: &Trigger) -> std::result::Result<(), String> {
    for dir in [trigger.dir(), trigger.inbox()] {
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        restrict(&dir)?;
    }
    Ok(())
}

/// `0700` on a directory this server owns.
#[cfg(unix)]
fn restrict(dir: &Path) -> std::result::Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", dir.display()))
}

#[cfg(not(unix))]
fn restrict(_dir: &Path) -> std::result::Result<(), String> {
    Ok(())
}

/// Drop one tuple into the queue — `ikigai-gonk review request <repo> <path>`.
///
/// ★ **Through the space's own Sink, not by writing a file.** The endpoint computes the
/// blake3 id and publishes with a staging write plus a rename, so a reader never sees a
/// half-written tuple and an identical drop lands on its own name. Re-implementing that in
/// a shell hook would be a second content-addressing scheme, and two ids for one request is
/// exactly the duplicate this queue exists to avoid.
///
/// The kernel here is a throwaway holding only the space: no store is opened, no door is
/// bound, nothing is dialled. That is what makes this safe in a `post-commit` hook — it
/// cannot block on a model, a peer, or the running server, and it works when gonk is down.
///
/// # Errors
///
/// When the space name is not a name, the tree cannot be created, or the drop fails.
pub fn drop_tuple(trigger: &Trigger, tuple: &Tuple) -> std::result::Result<String, String> {
    check_space_name(&trigger.space)?;
    check_request(tuple)?;
    prepare(trigger)?;
    let kernel = ikigai_core::Kernel::new(Arc::new(ikigai_intray::space(trigger.root.clone())));
    let iri = Iri::parse(format!("urn:space:{}", trigger.space))
        .map_err(|e| format!("urn:space:{}: {e}", trigger.space))?;
    let request = Request::new(Verb::Sink, iri).with_arg(
        "content",
        ikigai_core::ArgRef::Inline(tuple_turtle(tuple).into_bytes()),
    );
    let capability = ikigai_core::Capability::scoped([ikigai_intray::CAP_OUT]);
    // Through `ikigai-resolve`'s blocking seam rather than an executor of our own: this runs
    // from `main` before any runtime exists, which is the one place in this binary where
    // `block_on` is not the panic `crate::mount` documents.
    let (answer, _) = ikigai_resolve::Resolver::issue_as(&kernel, request, &capability)
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&answer.bytes).trim().to_string())
}

/// How deep the queue is — or why that number is not available.
///
/// ★ **Three answers, never one, because two of them are "no number" and they mean opposite
/// things.** A page that renders `0` for an unconfigured queue, an empty queue and an
/// unreadable one says "nothing has happened" in all three cases; only one of those is true.
/// Ledger [#446](http://localhost:1060/l/default/item/446) is that shape, and the Queue page
/// is the one surface whose entire job is to show process state, so it is the last place a
/// silent absence belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Depth {
    /// No `gonk.review.space`: this server binds no queue, and nothing can drop into one.
    NotConfigured,
    /// The three stages' counts. `outbox` and `error` are the reactor's, and are zero on
    /// this server because it runs no reactor — they are read anyway, so that the day one
    /// runs the page does not have to change.
    Counted {
        /// Waiting to be reviewed.
        inbox: usize,
        /// Handled.
        outbox: usize,
        /// Dead-lettered.
        error: usize,
    },
    /// The tree is configured and could not be read. The string is the reason.
    Unreadable(String),
}

/// [`Depth`] for a configured trigger, or [`Depth::NotConfigured`] for `None`.
///
/// ⚠ **This counts FILES, where everything else in this server reads through the kernel** —
/// and the reason is a real gap rather than a shortcut. The count lives behind
/// `Source urn:space:{name}`, which requires `urn:cap:space:read`, and this server mints that
/// token for NOBODY ([`space`]): not for an anonymous caller, not for a passkey identity, not
/// for a certificate. So there is no capability any page could be rendering under that would
/// be allowed to ask the kernel, and a read through it would be a typed `Denied` on every
/// request — which is exactly the missing number the page exists to supply. Counting the
/// directory is what [`pending`] already does for the banner; this is the same read, told
/// apart from its two failure modes. Reported up as the design question it is: either the
/// depth becomes a resource of gonk's own with its own floor, or the space's read token
/// becomes mintable.
pub fn depth(trigger: Option<&Trigger>) -> Depth {
    let Some(trigger) = trigger else {
        return Depth::NotConfigured;
    };
    // The inbox is created 0700 by `prepare` at startup, so a missing one is a fact worth
    // reporting rather than a zero. The other two stages belong to a reactor that has never
    // run here, so their absence IS zero.
    let inbox = match count_tuples(&trigger.inbox()) {
        Ok(n) => n,
        Err(e) => return Depth::Unreadable(format!("{}: {e}", trigger.inbox().display())),
    };
    Depth::Counted {
        inbox,
        outbox: count_tuples(&trigger.dir().join("outbox")).unwrap_or(0),
        error: count_tuples(&trigger.dir().join("error")).unwrap_or(0),
    }
}

/// The `*.tuple` files in one stage directory.
fn count_tuples(dir: &Path) -> std::io::Result<usize> {
    Ok(std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".tuple"))
        })
        .count())
}

/// The tuple ids waiting in a trigger's inbox, sorted — what the banner counts.
pub fn pending(trigger: &Trigger) -> usize {
    match std::fs::read_dir(trigger.inbox()) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(".tuple"))
            })
            .count(),
        Err(_) => 0,
    }
}
