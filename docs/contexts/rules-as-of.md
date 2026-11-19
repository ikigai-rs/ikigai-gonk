# Rules as of T: grants, harness, and configuration

Design spike for default#3755. Status: proposed contracts and storage design, not an
implemented historical authorization service. The dynamic-contexts and organizational-grants
models in the item are requirements. Machine-specific token counts and incidents in the item
are background, not measurements reproduced here.

## Problem and reproduced baseline

An answer to “was this allowed when it was done?” needs the authority, rules, configuration,
and executable inputs used at that time, together with the evidence of the actual request.
Replaying today's grants cannot supply that evidence. Historical authority must never become
a way to exercise a revoked grant or read something the caller cannot read today.

The repository supports the following narrower, reproducible premises:

| Existing behavior | Evidence read in this checkout |
| --- | --- |
| Grants parse as a map from a grant name to scopes; an object can wrap a `scopes` array. This is not a principal's full organizational assignment history. | [src/quic.rs:922](../../src/quic.rs#L922) |
| `put_grant` replaces one map entry with an array; a changed entry requires `force`. Its signature takes neither author nor reason, and this write stores neither. | [src/quic.rs:1292](../../src/quic.rs#L1292) |
| Certificate enrollment writes a grant and an identity-to-grant binding into two files. | [src/quic.rs:1452](../../src/quic.rs#L1452) |
| Current certificate authority is resolved from enrollment and grants, with no historical selector. | [src/quic.rs:1344](../../src/quic.rs#L1344) |
| QUIC admission rechecks standing authority and narrows an existing connection. | [src/admit.rs:812](../../src/admit.rs#L812) |
| The file cache shares parsed current configuration; it is not a history store. | [src/file_cache.rs:50](../../src/file_cache.rs#L50) |
| Gonk mounts `ikigai-repo` confined to configured roots. | [src/browse.rs:422](../../src/browse.rs#L422) |
| A Git object read checks browse access and `urn:cap:exec:read:git`, then issues a repository resource read under the caller's capability. Historical file bytes already compose through `urn:repo:show` with `dir`, `rev`, and `path`. | [src/git_object.rs:175](../../src/git_object.rs#L175), [src/git_object.rs:279](../../src/git_object.rs#L279) |
| The existing weekly history computation validates a full SHA and returns `.cacheable()` representations. Its comment about “no thread to cut” describes immutability, not an API for thread-free cache admission. | [src/history.rs:217](../../src/history.rs#L217), [src/history.rs:265](../../src/history.rs#L265), [src/history.rs:16](../../src/history.rs#L16) |
| The resolved core version is 0.3.5. Its kernel attaches a canonical-target thread to cacheable Source/Exists answers and propagates dependency threads and expiry. | [Cargo.lock:2032](../../Cargo.lock#L2032); published `ikigai-core-0.3.5/src/kernel.rs:4991–5023`, read from the Cargo registry source, [source at the package's recorded commit](https://github.com/ikigai-rs/ikigai-core/blob/dc6d0ac6c48d9c31b0059609c564705627853bf5/crates/ikigai-core/src/kernel.rs#L4991) |

The small executable baseline is
[tests/rules_as_of_3755.rs:5](../../tests/rules_as_of_3755.rs#L5): create a write grant, refuse
an unforced replacement, then force a read-only replacement and inspect the complete stored
JSON. It contains only the new scopes, with no previous scopes or provenance. This is a
passing characterization of a design gap, not a claim that today's API violates its contract.
No production implementation belongs in this spike. The existing authority revocation tests
also supply a useful boundary fixture:
[tests/authz_cache_2458.rs:96](../../tests/authz_cache_2458.rs#L96).

## Time semantics and the three views

Use server-recorded transaction time for the unqualified **as of T** question: only facts
committed by T may contribute. Every accepted policy transaction has a globally ordered
sequence and a nondecreasing UTC timestamp; same-timestamp transactions are included together.
If a trusted clock moves backward, stop accepting policy commits until monotonicity is restored.
Use half-open validity windows `[valid_from, valid_until)` for assignments and delegation.
An effective grant at T evaluates the snapshot known at T at validity time T. A future-dated
assignment is recorded immediately but activates only at its declared validity boundary.

Disallow retroactive changes to published validity windows. An imported statement about the
past is new evidence recorded now, never a rewrite of an old answer. A separately named query
with both `known-at` and `valid-at` can later answer corrected-history questions; it must not
silently alter the meaning of `as-of`. Reject a future T for this immutable API. A current
read uses a mutable `current` composition and is not an immutable historical name.

Contexts stack by field: innermost supplied selector wins; omitted fields inherit. Freeze the
result to one immutable context revision before resolving any input. An alarm, PR, or incident
may supply a context reference as data; it does not supply authority. Detect reference cycles,
bound depth, and reject incompatible selectors rather than combining inputs from different
snapshots. Record both the supplied stack and the resolved context revision with the answer.

* **View as of:** select a policy snapshot, deployment manifest, and evaluator revision at T.
* **View as:** select a subject principal and compute its historical grant as evidence. An
  impersonation selector does not mint a capability for that subject.
* **What if:** overlay an immutable proposal diff on a pinned base, mark the result simulated,
  and use a different answer name. Simulation never commits a grant or performs a side effect.

A pinned evaluation is deterministic only when every input is fixed: policy snapshot,
context, rule/evaluator revision, request, validity instant, code revision, workflow, harness,
config, and model artifact/settings. Record external inputs or return “not reproducible”;
a model's mutable alias alone cannot justify a pure-function claim. Permission to retrieve
the result remains a present-time decision.

## Resource map

All names below are proposed under `urn:iki:gonk:`; none extend the library-owned
`urn:repo:{root}:…` grammar. In the table, omit that prefix for readability. Each set has a
singular type first and keyed partitions. IDs and paths use one canonical percent-encoded
segment per value; instants are canonical UTC RFC 3339 with fixed nanosecond precision.
Reject ambiguous encodings, duplicate keys, and noncanonical instants before lookup.

| Resource pattern | Kind and state held |
| --- | --- |
| `policy:organization:{org}:head` | Mutable atom: active snapshot digest, transaction sequence, migration epoch. The single compare-and-set serialization point for the organization's policy. |
| `policy:organization:{org}:snapshot:{digest}` | Immutable atom: schema version, parent digest, and complete references to organization, role, assignment, binding, budget, taxonomy, rule, and deployment revisions. |
| `policy:organization:{org}:change:{id}` | Immutable accepted-change atom: canonical diff, previous/new snapshot, authenticated author, server time/sequence, reason, proposal, approver attestations, and hash. |
| `organization:id:{org}:revision:{digest}` | Immutable atom: organization tree and scope containment rules. No multiple parents or cycles. |
| `role:organization:{org}:id:{id}:revision:{digest}` | Immutable atom: parameter schema and capability template. Referenced by exact revision, never by a mutable role label. |
| `assignment:organization:{org}:id:{id}:revision:{digest}` | Immutable atom: subject principal, pinned role, parameters, scope, validity, parent delegation, owner, reason, and approval evidence. |
| `binding:organization:{org}:id:{id}:revision:{digest}` | Immutable atom: authenticated identity-to-principal and principal-to-assignment bindings, including imported legacy grant membership. A shared grant name is not a person's identity. |
| `budget:organization:{org}:id:{id}:revision:{digest}` | Immutable atom: quantity, unit, parent allocation and validity. Spend receipts are immutable atoms at `budget:organization:{org}:id:{id}:receipt:{receipt}`; a mutable `…:balance` atom serializes allocations and debits. |
| `work-kind:organization:{org}:revision:{digest}` | Immutable SKOS taxonomy atom, including explicit mapping from ledger label IRIs to claim eligibility. |
| `proposal:organization:{org}:id:{id}:revision:{digest}` | Immutable diff atom with base snapshot, author, reason, and intended effect; mutable `proposal:organization:{org}:id:{id}:state` holds pending/accepted/rejected and approval references. |
| `approval:organization:{org}:id:{id}` | Immutable attestation atom: authenticated approver, time, proposal digest, base snapshot, and reviewed widening summary. |
| `policy:organization:{org}:as-of:{instant}` | Composition: map a closed transaction-time prefix to its snapshot, returning the cutoff sequence and coverage interval. |
| `grant:organization:{org}:principal:{principal}:as-of:{instant}` | Composition: effective historical capabilities and derivation paths at T from that snapshot. Includes snapshot and evaluator digests, validity boundaries, and historical budget evidence. |
| `grant:organization:{org}:principal:{principal}:current` | Mutable composition: active head evaluated at current server time, expiring at the next validity boundary. |
| `deployment:organization:{org}:id:{id}` | Immutable activation atom: server time, author, reason, harness/config repository identities and full commits, paths/blob hashes, code/workflow/rules/model revisions, environment input digests. |
| `artifact:repository:{repo}:commit:{sha}:path:{path}` | Composition over `ikigai-repo`: bytes at a full commit, with repository identity and blob digest. Repository identity is an immutable registration, not a movable root alias. |
| `context:id:{id}:revision:{digest}` | Immutable atom: frozen selectors, deployment, policy cutoff, evaluator and overlay references. A separate `context:id:{id}:head` atom can move. |
| `answer:context:{digest}:request:{digest}` | Immutable evidence atom: output, resolved inputs, trace, time of execution, and decision outcome. Request digest covers canonical arguments, subject and representation/schema; capability-sensitive output also includes the capability fingerprint. |

Choose the explicit `grant:…:as-of:{instant}` keyed partition as the public historical grant
address. It is independently citable and testable without ambient context. A context corridor
resolves to this same name and records it; the corridor is a composition, not a second history
store. Bare instants are organization-local server transaction time, not Git author time.

## Accepting changes and deriving authority

Persist immutable atoms and the new head in one durable store transaction. Allocate the
server sequence/time in that transaction; acknowledge only after commit. An immutable atom's
digest includes its schema and canonical content. Keep a hash-linked change chain and signed
checkpoints in backups: hashing alone does not prevent an owner with disk access rewriting
the entire chain. This is auditable provenance, not a promise against a malicious host owner.

Every edit, including removal, role update, binding edit, delegation, budget split, taxonomy
edit, and deployment activation, begins with a proposal and a diff. Derive author from the
authenticated capability; accept a display label only as untrusted annotation. Require a
nonempty reason, owner and validity window. Preserve author, recorder/importer, and approver
as distinct fields. Rejection and approval are events too; nobody edits an old approval.

At acceptance, compare-and-set against the proposal's exact base head, recheck the author's
and approver's authority today, validate the entire graph, then compute the semantic delta
for every affected principal. Adding scope, extending validity, relaxing work-kind rules,
increasing a budget, or widening a role's parameters counts as widening. Mixed edits count
as widening. If containment cannot be proved, require widening approval and still refuse
unprovable delegation. Require a second authenticated person distinct from the proposer for
widening, with an attestation bound to the exact diff and base. A new diff requires new approval.
Role edits create new revisions; moving assignments to them is an explicit proposed diff.

Delegation is the intersection of parent authority, child role/template, scope and validity,
recursively up to the organization root. Reject cycles and every child exceeding its parent;
use capability scope semantics, not string-prefix comparison. Revoking a parent invalidates
its descendants in the new snapshot. A parent must not outlive its own parent. Enumerate and
bound the impacted subgraph before accepting; never partially apply a large edit.

Budget allocation reserves quantity from the parent atomically; simultaneous splits cannot
allocate the same quantity twice. Spend is a separate door transaction before work starts,
with an idempotency key and receipt; an exhausted capability refuses admission. Historical
reads show allocations and receipts known by T and never replenish today's balance. Claim
admission must check the pinned work-kind mapping plus current caller authority and current
budget in the same admission protocol. Changes to labels or taxonomy cannot bypass that check.

For “was this allowed?”, evaluate the recorded request under the subject's historical
effective grant and pinned policy/evaluator. Return allowed/denied/indeterminate with evidence.
Missing request inputs or pre-migration coverage means indeterminate, never “allowed.” That
historical decision is evidence, not authorization to replay the action.

## Capabilities: declared and enforced

These are proposed exact tokens, scoped by organization and subject/resource. A catalog may
declare an offering wildcard; an endpoint must still enforce the concrete bound scope.
Every Source also offers Meta under the same read authority. Writes use Sink, not Source.
Delete is unsupported for immutable evidence. No proposal accepts raw RDF policy writes from
ordinary callers; store mutation tokens stay behind the policy service's narrow writer door.

| Action | Declaration and enforcement |
| --- | --- |
| Read historical or current grant, role, assignment, change, approval or policy snapshot | Declare `urn:cap:gonk:policy:read:{org}`. Enforce it plus `urn:cap:gonk:grant:read:{org}:{principal}` for each disclosed subject. Role/organization-wide reads require `urn:cap:gonk:policy:audit:{org}` when they reveal other subjects. Filtered lists must not reveal hidden counts or IDs. |
| Propose or revise an edit | Declare/enforce `urn:cap:gonk:policy:propose:{org}` and the authenticated principal. Also enforce authority to delegate the proposed concrete scopes and budgets. A proposer cannot grant beyond that ceiling. |
| Approve widening | Declare/enforce `urn:cap:gonk:policy:approve:{org}`, authenticated human identity, second-person rule and scope ceiling. Approval is not acceptance. |
| Accept a proposal, including narrowing | Declare/enforce `urn:cap:gonk:policy:apply:{org}`; recheck proposal/approvals and all graph invariants against the current head. No `force` bypass. |
| Allocate or spend budget | Declare/enforce `urn:cap:gonk:budget:allocate:{org}:{id}` or `urn:cap:gonk:budget:spend:{org}:{id}` plus the underlying work capability and current balance. Allocation is a policy proposal; spend is an atomic receipt transaction. |
| Read harness/config bytes at a commit | Declare/enforce `urn:cap:browse:read:{root}` and `urn:cap:exec:read:git`, plus artifact-specific secret/read classification today. Issue nested repository reads under the caller's attenuated capability. |
| Read or activate deployment | Read declares policy-read plus each referenced artifact's read requirements. Activation declares/enforces `urn:cap:gonk:deployment:apply:{org}` and uses the proposal/approval protocol; its credentials do not flow to readers. |
| Resolve context, simulate, or read an answer | Declare/enforce `urn:cap:gonk:context:read:{id}` and each underlying input's present read requirements. Simulation additionally requires policy-read/propose and subject-read scopes; it invokes no mutating endpoints. Answer retrieval rechecks its recorded input-access requirements. |
| Retain, export, or purge evidence | Declare/enforce `urn:cap:gonk:policy:retention:{org}`; exports also check each exported input's present read requirements. Purge requires a proposed retention change and separate approval. |

Today-visible information about a historical grant need not imply possession of the grant's
action tokens: an authorized auditor may inspect a revoked write grant without being able to
write. Conversely, historical ledger/config contents still require today's permission to those
contents. A context selector never supplies read tokens. Normalize the caller at the door,
then intersect with any requested historical subject for execution; never union capabilities.
Check input access before cache lookup and again before returning a response if the authority
epoch changed during the read. No existence test or cache hit precedes the access check.

## Git-backed harness and config

Register the config home and harness repositories as explicit confined roots. Never pass an
arbitrary directory supplied in a context. Resolve a full commit and normalized relative path,
then Source `urn:repo:show` with `dir=<registered root>`, `rev=<full commit>`, and `path=<path>`
through the kernel, under the caller's capability. This follows the existing composition at
[src/git_object.rs:286](../../src/git_object.rs#L286). The new context module must not use
`Command`, shell pipelines, or a direct Git library to bypass `ikigai-repo` contracts. Reading
configuration does not load modules, execute hooks, expand secrets, or apply that configuration
to the running server. Keep private keys, session material and resolved secrets out of Git and
out of historical answer payloads; record a secret version reference where necessary.

“Commit C” means exactly its tree bytes. “Active at T” selects the last accepted deployment
activation by T and uses its full commits and blob hashes. Git author and committer timestamps
can be backdated, branches can move, and two commits can have the same time; neither date proves
deployment. For a repository-history-only view, require a pinned ancestry tip and explicit
selection policy (first-parent committer-time, deterministic traversal/tie break), and record
the selected full SHA. This optional composition is labeled repository history, not deployed
configuration. Prefer activation records for the product's default time view.

Uncommitted deployment bytes cannot be cited as a commit. Reject activation until committed,
or explicitly capture an immutable, classified artifact and label it a dirty snapshot with its
base commit and diff. Start with committed-only activation. Pin the actual deployment's rules,
harness, code/workflow and model revision together; don't independently take each repository's
nearest commit by date. If the repository facade lacks a required temporal selector, add that
contract in `ikigai-repo` first; this spike does not assert an existing date-selection API.

## Cacheability, threads, and retention

An as-of answer for a past instant is immutable once its transaction prefix is closed. Commit
transactions serialize the cutoff with the head: a read at the current clock boundary waits
for that prefix to close. Never backfill a transaction into a closed prefix. Return
`snapshotDigest`, `cutoffSequence`, `coverageStart`, `evaluatedAt`, `schemaVersion`, and
`evaluatorDigest`. Cached values are meaningful only within their import epoch and evaluator
revision; an upgrade changing semantics creates a new pinned context/answer identity.

Core hangs cacheable answers on their canonical-target threads, even without a declared
dependency (the core source cited above). Use `.cacheable()` for the immutable payload and
optionally declare an additional immutable snapshot dependency thread such as
`urn:iki:gonk:policy:organization:{org}:snapshot:{digest}`. It has no ordinary mutation that
cuts it; neither does the pinned answer's own canonical-target thread. A new grant change
creates a new snapshot/name. Do not invent a thread-free cache flag. Pin both the automatic
target thread and propagated snapshot dependency in the implementation's kernel tests;
the history precedent above does not prove the absence of threads.
Return explicit `immutable: true` metadata on the pinned payload, not an inference from an
empty thread set. Runtime cache entries remain evictable; forever-valid bytes do not require
forever-resident memory.

Separate the immutable canonical payload from its access-controlled response. A response
filtered for a caller is keyed by capability fingerprint and present authority epoch, and
depends on the authorization/classification/retention threads as well as the immutable input.
Until cache partitioning and pre-hit authorization are proved, mark those envelopes uncacheable
and cache only internal pinned payloads. Mutable `current`, context heads, repository root
mappings, and classification reads carry live threads; validity boundaries use timed expiry.
Do not let a historical composition inherit the mutable policy-head thread just because an
index was used to find its frozen snapshot. Cache misses, incomplete history and refusals are
not immutable successes. On the HTTP wire use private/no-store for sensitive policy and config
responses; kernel caching is a separate decision.

Implement the cutoff lookup as a bounded seek in the append-only transaction index that returns
a closed-prefix proof and exact immutable references. Do not Source the mutable `head` inside
an allegedly permanent answer and then discard its dependency threads. If the available store
read contract carries a mutable index thread, keep that lookup envelope uncached and cache the
snapshot/evaluator-keyed payload behind it; optimize the public as-of composition only after
an immutable-prefix read contract exists. This preserves correctness without weakening core's
dependency propagation.

Retain policy history, proposal/approval evidence, deployments, spend receipts, contexts, and
cited Git objects for the organization's audit period; recommend indefinite retention of these
small policy records by default. Keep a protected Git ref/object retention manifest for every
cited commit so GC or force-push cannot silently remove it. Back up the RDF history, checkpoints
and referenced Git objects together and test restoration. Configuration containing sensitive
data may need a shorter, approved policy: retain digests and tombstone metadata after deleting
the bytes, and record that replay is no longer possible. A digest is not a recovery mechanism.

A legal/privacy purge is an explicit exception to availability, never a rewrite: record the
scope/reason/approvers, cut retention/access threads, evict kernel and artifact copies, and
purge backups according to the approved schedule. Future reads return NotFound after access
checks, with an authorized audit view exposing the tombstone. Previously exported bytes cannot
be recalled. A purged payload's digest remains stable; the promise is immutable content while
retained, not perpetual availability or perpetual permission.

## Refusals and failures

Access checks precede existence checks. Return structured failures and no partial success.
Historical “subject was denied at T” is a successful audit result, distinct from denying the
caller permission to ask the question.

| Class | Conditions |
| --- | --- |
| Denied | Missing current read/action scope; forged principal; self-approval of widening; missing second-person approval; delegation exceeds parent; expired/revoked approver; exhausted current budget. Use Denied for budget refusal in this proposed policy API; a future adoption of a budget-specific error requires an explicit contract change. |
| Conflict | Stale proposal base or approval digest; concurrent head/balance update; same idempotency key with different content; activation references a moving/dirty deployment instead of its proposed pinned inputs. Return the authorized current revision so the caller can re-propose. |
| InvalidArgument | Malformed or future instant; invalid window or parameters; missing reason/owner; cycle; ambiguous selector; invalid SHA/path; unsupported schema; unbounded or contradictory context stack. Distinguish a structurally invalid delegation graph from an unauthorized widening. |
| NotFound | Authorized lookup of absent principal/revision/path/object; no pre-import coverage; retained object lost or purged. Never substitute today's state or working-tree bytes. An audit composition can translate missing evidence to an explicit indeterminate result with the missing reference. |

Storage corruption, an unavailable repository service, and clock failure are operational
failures, not proof of absence: preserve the underlying endpoint/I/O/timeout error and fail
closed. Do not misclassify them as one of the four caller refusals above. Crashes before the
policy transaction commits leave no accepted edit; crashes after commit recover the same head
and receipt. A materialized file export failing after commit does not roll back the history;
the door must continue reading authoritative data or refuse until reconciled.

## Migration and published compatibility

1. Freeze legacy grant/enrollment writers during a bounded cutover. Read and validate grants
   and identity bindings together; record source hashes, import time, importing principal,
   reason and known coverage start. Import shared grants as pinned legacy roles and explicit
   assignments/bindings, including every member of a multi-grant principal. Mark pre-import
   authorship, approvals and history unknown; never fabricate them from file mtimes.
2. Register harness/config repositories and write the first verified deployment activation.
   Repository history before cutover is available as committed content, not proven activation.
3. Shadow-evaluate today's effective scopes against the legacy resolver for every enrolled
   principal, including denied and empty cases. Require exact semantic parity before switching
   doors. Keep the same capability token spellings; resource names above are additive.
4. Make the versioned policy store authoritative for every door and internal job in one
   migration epoch. Route client/passkey grant editing through proposals. Deprecate `--force`
   replacement with a clear refusal and proposal reference; update CLI help and contracts in
   the implementing release. No silent second-person bypass for the owner's normal CLI path.
5. If old integrations need `grants.json`, export it as a read-only current snapshot with an
   external epoch/digest manifest, preserving its published array shape. Disable legacy file
   editing and refuse unexplained drift. Do not dual-write two authorities. Rollback must
   replay the latest accepted snapshot, not restore a stale, wider file. A rollback to an old
   binary requires a frozen, verified export and an explicit deployment event.
6. Restrict raw policy-graph writes and maintain a separately audited offline recovery/import
   procedure. The socket owner can still alter disk; document this trust boundary. Exercise
   backup recovery and drift refusal before enabling mutation in production.

## Tests that would prove the design

The baseline test in this PR pins current replacement behavior only. The following acceptance
tests belong to implementation work, through a real kernel and each applicable door:

1. At T0 assign write, at T1 remove it, then read `…:as-of:T0`: write is still present with the
   original derivation; `…:as-of:T1` excludes it. Restart and restore from backup; compare exact
   digests. Test equal timestamps, boundary instants, future windows and clock rollback.
2. Commit a change and verify authenticated author, server time/sequence, nonempty reason,
   before/after references, and widening approver. Spoofed author, missing reason and self
   approval refuse; no accepted atom or head change appears. Editing a diff invalidates approval.
3. Warm historical answers as an auditor, then revoke the caller's current read authority.
   Repeat with the same connection, a fresh connection, HTTP, socket attenuation and a mounted
   caller. All refuse before returning cached bytes. Another caller with fewer scopes cannot
   read the first caller's payload, counts, input metadata, or existence. An authorized auditor
   can still inspect revoked action tokens but cannot use them to perform the action.
4. Assert declared scopes and dynamic enforcement agree for every action. Test direct atom
   reads, list endpoints, aliases, context-supplying PR/alarm resources, nested contexts, and
   answer retrieval; removing any underlying input scope denies the entire protected view.
5. Test parameterized role expansion, exact revision pinning, parent revocation, cycles,
   validity containment, ambiguous scope attenuation, taxonomy changes and scope increases
   hidden in mixed diffs. Race acceptances against the same head: exactly one wins.
6. Race budget allocation and spending: no double allocation or negative balance, exactly one
   receipt per idempotency key, no work before admission, and no refund from historical replay.
7. Create a Git fixture with C1 active at T, C2 committed earlier by backdated author time but
   activated later. Time view returns C1; explicit C2 returns its exact bytes. Move refs, edit
   working files and GC: retained C1 still resolves. Reject traversal, root substitution and
   missing current browse/exec/secret scopes. Record issued `urn:repo:show` requests and verify
   attenuation; the context module launches no subprocess.
8. Inspect actual cache entries: pinned success is cacheable with its immutable thread; moving
   the policy head preserves its digest; current answers invalidate on edits and expire at
   validity boundaries. Classification/revocation/purge cuts block access to cached results.
   Verify authorization precedes cache hits and envelopes remain uncached until safe partitioning
   is demonstrated. Failed and incomplete reads never become immutable successes.
9. Crash at transaction boundaries; verify no half-applied role/assignment/approval/head.
   Reject out-of-band file drift. Import multi-grant/shared-grant fixtures, prove current
   authority parity, and return unknown coverage before import rather than invented history.
10. Simulate a widening under a pinned proposal: the answer is labeled simulated, its citation
    differs from the accepted context, no head/balance changes, and no mutation is issued.
    Restore all retained artifacts and reproduce an answer; delete a required artifact and
    verify indeterminate evidence rather than a false “allowed.”

CI for this spike checks the characterization test and existing suite, formatting and clippy;
it cannot prove the proposed service exists. The repository's PR workflow is declared at
[.github/workflows/ci.yml:1](../../.github/workflows/ci.yml#L1).

## DECISIONS FOR BRIAN

1. **Historical time:** recommend server transaction time plus validity windows, with no
   retroactive edits. Add explicitly bitemporal queries later if corrected history is needed.
2. **Names versus ambient corridors:** recommend the keyed `grant:…:as-of:{instant}` public
   resource and contexts that resolve to it. Keep context identity in every recorded answer.
3. **Authority and approval:** recommend independent current read authority, semantic narrowing
   for execution, and a distinct human approver for every widening. No ordinary force bypass.
4. **Authoritative storage:** recommend one transactional, append-only policy history with a
   compare-and-set organization head, replacing grants.json as authority after parity checks.
5. **Role evolution:** recommend exact role revisions on assignments and explicit migration
   proposals, so editing a template never silently widens every assignee.
6. **Git time:** recommend activation manifests as the default “active at T,” full commits for
   explicit reads, and committed-only deployment initially. Compose exclusively through
   `ikigai-repo`; upstream any missing repository contract before implementing the caller.
7. **Caching:** recommend immutable threaded payloads with explicit immutability metadata,
   present-time access checks and uncached access envelopes until partitioning is proven.
8. **Retention:** recommend indefinite small policy/evidence retention and protected cited Git
   objects by default; approve shorter sensitive-content retention with explicit tombstones.
9. **Budgets and work kinds:** recommend treating allocations and taxonomy revisions as policy,
   spending as atomic admission, and historical budget evidence as incapable of funding work.
10. **Delivery order:** recommend history/import and door parity first, then proposals/approval,
    roles/delegation, deployment contexts, and budgets/work-kind admission. Do not advertise
    a reproducible historical answer before its input coverage and evaluator are recorded.
