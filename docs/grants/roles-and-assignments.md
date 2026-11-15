# Roles and assignments as data

Design spike for default#3749. Status: proposed design, not implemented. The only code in this
change is [tests/roles_assignments_3749.rs](../../tests/roles_assignments_3749.rs), which pins
three facts the design relies on. The build item this informs stays open.

This document is the companion to [rules as of T](../contexts/rules-as-of.md) (default#3755).
That spike specifies how role, assignment and proposal *revisions* are stored and read over
time. This one specifies what a role and an assignment *contain*, how they reproduce today's
grants exactly, how a principal's effective grant is computed, and how gonk moves off
`grants.json` without changing what any identity holds. Where #3755 names a resource, a field
or a capability, this document uses its name. Every place this document differs from #3755 is
listed as a decision for Brian (decisions 1–5).

The item's machine-specific numbers (one person holding 102 tokens on the hub, the hub 10,
three workers 9 each, a reviewer 5) and the incident of 2026-11-13 are background. I did not
reproduce them, because this worker reads only this checkout. One local measurement bears on
them: `client add w --worker default --print` resolves to **7** tokens (table below). A hub
worker holding 9 therefore holds 2 tokens that `--worker` does not mint. The migration has to
find those tokens and keep them, and the zero-diff proof below exists for that reason.

## Problem and reproduced baseline

Today a grant is a **name mapped to a flat list of token strings**. It is shared by every
certificate and passkey enrolled under it, and it is edited by rewriting the whole list.

| Existing behavior | Evidence |
| --- | --- |
| `grants.json` is grant name → array of scopes (or an object with a `scopes` array). Nothing else is stored: no role, parameters, owner, reason, validity or parent. | [src/quic.rs:924](../../src/quic.rs#L924) |
| `clients.json` maps a certificate fingerprint to one grant name (`{"grant", "label"}`), with an optional `default` for an enrolled entry that names none. | [src/quic.rs:642](../../src/quic.rs#L642), [src/quic.rs:1457](../../src/quic.rs#L1457) |
| A passkey holds a *list* of grant names, and its scopes are the union of those grants, failing closed if any one does not resolve. | [src/identity.rs:251](../../src/identity.rs#L251), [src/identity.rs:899](../../src/identity.rs#L899) |
| `client add` and `passkey invite` build the list from flags through one function, in a fixed category order with duplicates removed. | [src/main.rs:1408](../../src/main.rs#L1408) |
| `--worker L` is `--ledger L=worker`, and nothing more. | [src/config.rs:1616](../../src/config.rs#L1616), [src/config.rs:1918](../../src/config.rs#L1918) |
| Rewriting an existing grant is refused unless `--force`, and `--force` writes the grant *whole* from the flags, narrowing included, by design. | [src/config.rs:269](../../src/config.rs#L269), [src/quic.rs:1296](../../src/quic.rs#L1296) |
| A change is described as a set difference (`removed`, `added`). Reordering is no change. | [src/quic.rs:1181](../../src/quic.rs#L1181) |
| Onboarding's apply enrolls `onb-{name}` with `force = true`, which replaces the grant whole and skips the refusal. | [src/onboard.rs:2176](../../src/onboard.rs#L2176) |
| One function refuses the tokens no identity may hold (broad store tokens, offering wildcards, door markers, principal names, the owner's admin tokens, the store reserve). It runs at startup, per connection, per call and in both writers. | [src/quic.rs:1001](../../src/quic.rs#L1001), [src/grants.rs:964](../../src/grants.rs#L964), [src/budget.rs:169](../../src/budget.rs#L169) |
| A connection's authority is minted from both files, and every call re-reads them. A narrowed grant narrows the standing connection. A widened grant waits for a new connection, at most `MAX_CONNECTION_AGE` (10 min). | [src/quic.rs:1353](../../src/quic.rs#L1353), [src/admit.rs:851](../../src/admit.rs#L851), [src/quic.rs:1332](../../src/quic.rs#L1332) |
| Every call's read goes through a stamp-checked cache of the parsed files. | [src/quic.rs:90](../../src/quic.rs#L90), [src/file_cache.rs:1](../../src/file_cache.rs#L1) |
| The kernel narrows capabilities by **literal set intersection**. `attenuate` keeps only held strings, and `allows` is an exact match. There is no operation that widens. | ikigai-core 0.3.5 `src/capability.rs:387`, `:433`, `:546` (registry source; [Cargo.lock:2032](../../Cargo.lock#L2032)) |
| Gonk's own containment is *not* literal. `urn:cap:ledger:write:{L}` covers `claim`, `comment`, `close`, `about`, `alias` and `session` on `{L}`. | [src/grants.rs:201](../../src/grants.rs#L201) |
| ikigai-script's namespace grant `urn:cap:script:{act}:{ns}-*` covers every name in the namespace. | ikigai-script 0.5.0 `src/authority.rs:252` |
| ikigai-browse's `urn:cap:browse:read:*` covers every root. | ikigai-browse 0.22.0 `src/lib.rs:1033` |

The last four rows produce the fact this design depends on most. **"Narrowed by its parent"
cannot be the kernel's `attenuate`.** A worker delegated by a writer would lose `claim`,
`comment` and `session`, because the writer holds `write`, not those strings. A script runner
delegated by a namespace holder would lose the script. The test pins both cases
([tests/roles_assignments_3749.rs:12](../../tests/roles_assignments_3749.rs#L12),
[:28](../../tests/roles_assignments_3749.rs#L28)).

## The role templates `client add` hard-codes today

Each minting flag is a role, applied once per occurrence with its parameters. The tokens below
come from running `client add w <flag> --print` on this checkout's binary, against an empty
config home. They match the functions cited, which compute each token from the owning
library's own spelling ([src/grants.rs:1](../../src/grants.rs#L1)).

| Flag | Proposed role id | Parameters | Tokens (ledger `L`, …) | Code |
| --- | --- | --- | --- | --- |
| `--ledger L=read` | `ledger-read` | `ledger` | `urn:cap:ledger:read:L`, `urn:cap:store:read:graph:urn:iki:ledger:graph:L`, `urn:cap:store:read:graph:urn:iki:ledger:graph:L:session` (3; the session graph since ikigai-ledger 0.23.0) | [src/grants.rs:137](../../src/grants.rs#L137) |
| `--ledger L=audit` | `ledger-audit` | `ledger` | read's 3, plus `urn:cap:store:read:graph:urn:iki:ledger:graph:L:deleted` (4) | same |
| `--ledger L=worker`, `--worker L` | `ledger-worker` | `ledger` | read's 3, plus `urn:cap:ledger:claim:L`, `urn:cap:ledger:comment:L`, `urn:cap:ledger:session:L`, `urn:cap:store:write:graph:urn:iki:ledger:graph:L`, `urn:cap:store:write:graph:urn:iki:ledger:graph:L:session`, `urn:cap:store:read:graph:urn:collab:participants` (9) | same |
| `--ledger L=write` | `ledger-write` | `ledger` | read's 3, plus `urn:cap:ledger:write:L`, `urn:cap:store:write:graph:urn:iki:ledger:graph:L`, `urn:cap:store:write:graph:urn:iki:ledger:graph:L:session`, `urn:cap:store:read:graph:urn:collab:participants` (7) | same |
| `--ledger L=delete` | `ledger-delete` | `ledger` | write's 7, plus `urn:cap:ledger:delete:L`, `urn:cap:store:write:graph:urn:iki:ledger:graph:L:deleted` (9) | same |
| `--ledger L=purge` | `ledger-purge` | `ledger` | delete's 9, plus `urn:cap:ledger:purge:L`, `urn:cap:store:read:graph:urn:iki:ledger:graph:L:deleted` (11) | same |
| `--browse-graph read` | `browse-graph-read` | none | `urn:cap:store:read:graph:urn:iki:browse:graph:default` (1) | [src/grants.rs:475](../../src/grants.rs#L475) |
| `--browse-graph write` (also `delete`, `purge`) | `browse-graph-write` | none | read's 1, plus `urn:cap:store:write:graph:urn:iki:browse:graph:default` (2). `worker` and `audit` are refused. | same |
| `--ledger-graph L` | `ledger-graph` | `ledger` | `urn:cap:store:read:graph:urn:iki:ledger:graph:L`, `urn:cap:store:write:graph:…:L`, `urn:cap:gonk:raw-write:graph:urn:iki:ledger:graph:L` (3) | [src/grants.rs:283](../../src/grants.rs#L283) |
| `--alias L=S` | `alias` | `ledger`, `scheme` | `urn:cap:ledger:read:L`, `urn:cap:store:read:graph:urn:iki:ledger:graph:L` (not the session graph), plus `urn:cap:ledger:alias:L`, `urn:cap:alias:scheme:S`, `urn:cap:store:read:graph:urn:iki:alias:graph`, `urn:cap:store:write:graph:urn:iki:alias:graph` (6) | [src/grants.rs:354](../../src/grants.rs#L354) |
| `--link-in L` | `link-in` | `ledger` | `urn:cap:ledger:link-in:L`, plus mirror's 2 (3) | [src/grants.rs:387](../../src/grants.rs#L387) |
| `--mirror-to L` | `mirror-to` | `ledger` | `urn:cap:store:read:graph:urn:iki:ledger:graph:L`, `urn:cap:store:write:graph:urn:iki:ledger:graph:L` (2) | [src/grants.rs:419](../../src/grants.rs#L419) |
| `--browse read [--root R]…` | `browse-read` | `roots` (empty = all) | `urn:cap:browse:read:*`, or one `urn:cap:browse:read:R` per root; plus `urn:cap:store:read:graph:urn:iki:browse:graph:default` | [src/grants.rs:584](../../src/grants.rs#L584), [src/grants.rs:634](../../src/grants.rs#L634) |
| `--browse derive [--root R]…` | `browse-derive` | `roots`, `host` (from this server's `gonk.mount`) | browse-read's, plus `urn:cap:annotate`, `urn:cap:net:{host}`. Refused with no mount, or with no configured root. | same |
| `--script N=read` | `script-read` | `name` (a name, `{ns}-*`, or `public`) | `urn:cap:script:read:N` (1) | [src/grants.rs:765](../../src/grants.rs#L765) |
| `--script N=run` | `script-run` | `name` | read's, plus `urn:cap:script:run:N` (2) | same |
| `--script N=publish` | `script-publish` | `name` (not `public`) | run's 2, plus `urn:cap:script:write:N`, `urn:cap:script:delete:N` (4) | same |
| `--nl draft` | `nl-draft` | `host` (from `gonk.mount`) | `urn:cap:net:{host}` (1). Refused with no mount. | [src/grants.rs:827](../../src/grants.rs#L827) |
| `--scope U` | `scope` (the residue role) | `scopes` (a list) | each `U` literally, after the syntax check and `grant_refusal` | [src/config.rs:1984](../../src/config.rs#L1984) |

Four properties of today's assembly must carry over:

1. **A grant is a set.** `scopes_for` adds tokens in category order (ledgers, browse graph,
   ledger graphs, across, browse role and nl, scripts, `--scope`), not command-line order, and
   it removes duplicates. For example, `--worker default --ledger default=write` yields 8
   tokens, not 12. Every comparison downstream (`GrantChange`, the door's narrowing) is
   set-based. Pinned at
   [tests/roles_assignments_3749.rs:41](../../tests/roles_assignments_3749.rs#L41).
2. **Two roles read server settings** (`browse-derive` and `nl-draft` read `gonk.mount`'s host,
   and `browse-*` reads the configured roots). Their parameters are resolved *when the
   assignment is accepted* and stored as concrete values. They are not re-read from
   `config.toml` at evaluation time. Otherwise moving the mount would silently re-point every
   net grant.
3. **The expansion is computed, never transcribed.** The built-in roles are *generator roles*.
   Their template is "call `grants::grants_for(ledger, Worker)`", not a literal token list,
   because the tokens come from `ikigai-ledger` and `ikigai-store` and have changed under gonk
   before (`session` arrived with ikigai-ledger 0.22.0,
   [src/grants.rs:29](../../src/grants.rs#L29)). See decision 6 for what an upgrade like that
   means for a pinned role revision.
4. **The `grant_refusal` families are never expandable.** No role, built-in or custom, may
   expand to a token `grant_refusal` refuses. Such a role revision is refused when it is
   proposed (InvalidArgument), and the evaluator applies the check again
   (see [Effective grant](#the-effective-grant)).

## Resource map

All names below are under `urn:iki:gonk:`, written without that prefix as #3755 writes them.
Sets have a singular, type-first name and keyed partitions. `{org}` is the organization key.
A single gonk deployment has one, named `default` (decision 7).

**Atoms** hold state. Each is immutable once written, except where marked mutable.

| Resource | Holds | Notes |
| --- | --- | --- |
| `policy:organization:{org}:head` | Mutable. The current snapshot digest, transaction sequence and migration epoch. | #3755's single compare-and-set point. This design adds `authority`, which says whether `grants-json` or `policy` is authoritative (see [Mid-migration](#how-the-door-keeps-working-mid-migration)). |
| `policy:organization:{org}:snapshot:{digest}` | Complete references to every revision below. | As #3755. |
| `policy:organization:{org}:change:{id}` | An accepted change: diff, previous and new snapshot, author, server time and sequence, reason, proposal, approvals. | As #3755. The import is change #1. |
| `organization:id:{org}:revision:{digest}` | The organization tree: units, their parent unit, and the principal that owns each unit. | As #3755. One root unit at migration. |
| `role:organization:{org}:id:{id}:revision:{digest}` | `kind` (`generator` or `template`), parameter schema, and the body. A generator's body is the name of a `grants.rs` function plus its evaluator version. A template's body is literal tokens with `{param}` holes. Also `label`, `comment`, and `may-claim` work kinds. | As #3755, plus `kind` (decision 1). Built-in ids are the role ids in the table above, under the reserved id prefix `gonk-`. |
| `assignment:organization:{org}:id:{id}:revision:{digest}` | `subject` (principal IRI), `role` (an exact role revision), `parameters`, `unit` (scope in the tree), `valid-from`, `valid-until`, `parent` (the assignment it is delegated from, or none), `owner`, `reason`, `approval` (references). | As #3755. Field names are #3755's. `unit` is this design's name for #3755's `scope` (decision 2). |
| `binding:organization:{org}:id:{id}:revision:{digest}` | Identity → principal (a certificate fingerprint, or a passkey credential id), and the `legacy-grant` name it was imported from. | As #3755. |
| `proposal:organization:{org}:id:{id}:revision:{digest}` | Base snapshot, author, reason, the **operations** (below), and the computed **effect** (below). | As #3755. The effect is added (decision 3). |
| `proposal:organization:{org}:id:{id}:state` | Mutable. `pending`, `accepted`, `rejected` or `withdrawn`, plus approval references. | As #3755. |
| `approval:organization:{org}:id:{id}` | Approver, time, proposal revision digest, base snapshot digest, and the widening summary the approver saw. | As #3755. |
| `budget:organization:{org}:id:{id}:revision:{digest}`, `…:balance`, `…:receipt:{receipt}` | Quantity, unit, parent allocation. Debits. | As #3755. Out of scope here except for the [Budgets and kinds of work](#budgets-and-kinds-of-work) notes. |
| `work-kind:organization:{org}:revision:{digest}` | The SKOS scheme, and the ledger-label → kind mapping. | As #3755. |

**Compositions** hold nothing. Each is computed from the atoms.

| Resource | Computed from | Notes |
| --- | --- | --- |
| `role:organization:{org}:id:{id}:revision:{digest}:expansion:{parameters-digest}` | One role revision and one parameter set. | Pure. The digest covers the canonical parameter encoding. |
| `grant:organization:{org}:principal:{principal}:current` | The head snapshot, at server time. | As #3755. **This is what the door reads.** |
| `grant:organization:{org}:principal:{principal}:as-of:{instant}` | As #3755. | Not specified further here. |
| `grant:organization:{org}:legacy:{name}` | The head snapshot, filtered to the assignments bound with `legacy-grant = {name}`. | New. The `grants.json` export, one entry at a time (see [Migration](#migration-from-grantsjson)). |
| `proposal:organization:{org}:id:{id}:revision:{digest}:effect` | The base snapshot plus the proposal's operations. | The diff a person approves. Also stored in the proposal atom, so an approval binds what was shown. |

**Where it is stored.** The atoms are quads in one named graph,
`urn:iki:gonk:policy:graph:{org}`, in this server's durable store. They are not stored in
`grants.json` or any other flat list. The graph follows the participants and alias graphs'
posture. Its store tokens are never minted by any role. A raw read or raw write of the graph is
refused at the door, as `raw_read_scopes` and `raw_write_refusal` already do for those graphs
([src/admit.rs:666](../../src/admit.rs#L666), [src/admit.rs:524](../../src/admit.rs#L524), [src/grants.rs:344](../../src/grants.rs#L344)).
The policy endpoints write to it under the host's own capability, as the backup does. Backups
include it automatically, because a backup is every graph.

## The effective grant

**Definition.** For a principal `p`, at server time `t`, in snapshot `s`:

```text
active(p, t)  = { a ∈ s.assignments : a.subject = p ∧ a.valid-from ≤ t < a.valid-until
                                       ∧ every ancestor of a is active at t }
ceiling(a, t) = ROOT_CEILING                                   if a.parent is none
              = effective_of_assignment(a.parent, t)            otherwise
effective_of_assignment(a, t) = { x ∈ expand(a.role, a.parameters) : covers(ceiling(a, t), x) }
effective(p, t) = ⋃ { effective_of_assignment(a, t) : a ∈ active(p, t) }   (first-seen order)
```

Then the door applies `grant_refusal` to `effective(p, t)` exactly as it does to a
`grants.json` entry today ([src/quic.rs:1153](../../src/quic.rs#L1153)). A refused set
refuses the identity whole. It never serves the remainder, the same posture as a passkey whose
one grant fails ([src/identity.rs:899](../../src/identity.rs#L899)). An empty set is refused,
as an empty grant is today.

**`covers(held, x)`** is semantic containment, the union of the rules the owning libraries
publish. It is never literal intersection (fact pinned in the test):

1. `x ∈ held` (literal).
2. `grants::implied_by(x, held)`: the ledger's write family
   ([src/grants.rs:201](../../src/grants.rs#L201)).
3. `x = urn:cap:script:{act}:{name}`, and `ikigai_script::authority::holds` admits it under
   `held` (a namespace).
4. `x = urn:cap:browse:read:{root}`, and `urn:cap:browse:read:*` ∈ `held`.

The child keeps *its own spelling* of `x`. A worker delegated from a writer holds
`urn:cap:ledger:claim:L`, not `…:write:L`, so delegation never converts a narrow token into
the broad one it was checked against. A new family rule is a code change to `covers` together
with a test against the owning library's own check. Until a rule exists, the token is covered
only literally. That fails safe: the token is dropped, never widened. Decision 8 asks whether
`covers` should move into ikigai-core.

**`ROOT_CEILING`** is every token except the `grant_refusal` families. Delegating from no
parent is possible only for an assignment whose acceptance a person with
`urn:cap:gonk:policy:apply:{org}` made, under the proposal rules below.

**"No child exceeds its parent"** is enforced twice:

- **At acceptance:** a proposal that adds or revises an assignment with a parent must have
  `expand(child) ⊆covers ceiling(child, t)` for every `t` in the child's window. Otherwise it
  is refused (Denied: "delegation exceeds parent", naming the tokens). The child's window must
  lie inside the parent's (InvalidArgument). A cycle through `parent` is InvalidArgument.
- **At evaluation:** the filter above. When a parent later narrows, its children's excess
  tokens drop out of their effective grant in the same snapshot. They are not refused, and the
  narrowing is not blocked. When a parent's window ends or the parent is ended, every
  descendant is inactive.

**When it is recomputed.** `grant:…:current` for every affected principal is recomputed and
written **in the same transaction** as the change that moves the head. The door then reads one
stored set per call, as cheaply as today's cached file read. A materialized set also carries
`valid-until`, the next validity boundary of any assignment in it. A read at or after that
instant recomputes before answering. So an expiry is effective at its instant, not at the next
edit. Gonk also recomputes everything:

- at startup, when the evaluator version stored in the head differs from the running binary's,
  because a generator role's expansion may have changed (decision 6);
- on restore, after the restore finishes and before the first door opens.

**At the door.** The capability is minted as now:
`Capability::scoped(effective).with_principal(principal)`
([src/quic.rs:1124](../../src/quic.rs#L1124)). The per-call re-read and narrowing
([src/admit.rs:851](../../src/admit.rs#L851)) and the widening bound
(`MAX_CONNECTION_AGE`) are unchanged. They read `grant:…:current` instead of two files. A
passkey's effective grant is the union over its assignments, the same as `scopes_of_all` over
its grant names today.

## Proposals, diffs, and the second person

**A proposal's operations.** Each is one of:

- `add-assignment {subject, role, parameters, unit, valid-from, valid-until, parent, owner, reason}`
- `end-assignment {assignment, at}`. The window is shortened, never deleted, so history stays
  readable (#3755).
- `revise-assignment {assignment, …fields}`. This is a new revision of the same id.
- `add-role` and `revise-role`. These create a new revision. Existing assignments keep their
  pinned revision until a `rebind-assignment {assignment, role-revision}` operation moves them
  (#3755 decision 5).
- `add-binding` and `end-binding`
- `add-unit`, `move-unit` and `end-unit`

**A proposal's effect**, computed by the server against the base snapshot and stored with the
proposal, is the **diff**:

```text
effect = for each principal whose effective grant differs between base and base+operations:
           { principal, removes: [token…], adds: [token…],
             window-extended: bool, kinds-relaxed: [kind…] }
         plus, for each role revision that changes: { role, adds: [token…], removes: [token…] }
```

It is computed through the same `effective` function the door uses. It uses
`GrantChange`'s set semantics and its `removes` / `adds` line format
([src/quic.rs:1181](../../src/quic.rs#L1181)), so the CLI's existing output stays readable.
The `adds` are computed under `covers`. A token the principal's base grant already covers is
not an add (for example `claim:L` when `write:L` is already held). This matches how
`implied_by` is already used to say a mint adds nothing.

**"Widening needs a second person", mechanically:**

1. A proposal is **widening** if its effect has any `adds`, any `window-extended`, any
   `kinds-relaxed`, any increase in a budget's quantity, or any role revision with `adds`.
   This holds even when no assignment uses that role yet. Anything else is **narrowing-only**,
   and that includes an empty effect.
2. Accepting a narrowing-only proposal needs `urn:cap:gonk:policy:apply:{org}`, and the
   applicant may be its author.
3. Accepting a widening proposal additionally needs one `approval` atom where:
   - the approver is authenticated as a **person**: a passkey principal (`urn:iki:gonk:passkey:…`,
     [src/identity.rs:209](../../src/identity.rs#L209)), never a certificate. This follows the
     door's existing rule that a machine drafts and a person publishes
     ([src/admit.rs:802](../../src/admit.rs#L802));
   - the approver is **not the proposal's author**, and is not bound to the author's principal
     by any binding;
   - the approver holds `urn:cap:gonk:policy:approve:{org}`, **and** the approver's own effective
     grant covers every token in `adds`. Nobody can approve authority they could not hold;
   - the approval names the proposal *revision* digest and the base snapshot digest. A new
     revision, or a moved head, makes it stale (Conflict).
4. Acceptance is a compare-and-set on `head` against the proposal's base. If the head moved,
   it fails with Conflict, and the proposal must be recomputed and, if widening, approved
   again.

**What happens to `client add`.** After cutover, `client add <name> --worker default`
becomes "propose `add-assignment` of role `gonk-ledger-worker {ledger: default}` to this
client's principal", and it **never replaces anything**. Removing authority is a separate,
explicit `end-assignment` (`client grant end <name> <assignment>`), whose diff lists every
removed token. `--force` no longer exists. A run of `client add` that would have replaced a
wider grant with a narrower one now adds an assignment, with no effect. This removes by
construction the 2026-11-13 near-miss, in which `--force` nearly narrowed every worker's
grant: there is no longer an operation that writes a whole grant. `--print` prints the effect.

## Budgets and kinds of work

These follow #3755 and are not specified further here. Two points interact with roles:

- **A budget is a capability with a quantity.** A role template may carry
  `budget {unit, quantity}`. An assignment's quantity is reserved from its parent's balance at
  acceptance, so delegation splits the quantity, and the split is checked as a narrowing. A
  spent budget is refused at the door with Denied, as #3755 rules. gonk's only quantity-like
  grant today is the store's monotone time budget, `urn:cap:store:budget:<ms>`
  ([src/budget.rs:5](../../src/budget.rs#L5)). It is a rate, not a balance, so it is not a
  model for spending.
- **Kinds of work.** A role revision's `may-claim` names concepts in the work-kind scheme. A
  claim on an item is admitted only if a ledger label on the item maps to a kind that the
  claimant's effective `may-claim` includes. This check runs at the door, before
  `urn:iki:ledger:claim` is forwarded, and it is in addition to `urn:cap:ledger:claim:L`, not a
  replacement for it. Today no claim check reads labels. The onboarding kinds
  (`docs/onboard/kinds.ttl`, [line 10](../onboard/kinds.ttl)) are node *kinds*, not work kinds.
  They are named alike, and the implementation must keep them apart.

## Capabilities each action declares and enforces

The tokens are #3755's. This table lists the actions this design adds. Each endpoint declares
the token, and its handler enforces the concrete instance.

| Action | Declares | Enforces in addition |
| --- | --- | --- |
| Read one's own `grant:…:current` and assignments | `urn:cap:gonk:grant:read:{org}:*` | that the subject is the caller's own principal (decision 9: proposed to be granted to every identity) |
| Read another principal's grant or assignments | `urn:cap:gonk:grant:read:{org}:*` | `urn:cap:gonk:grant:read:{org}:{principal}` for that subject, or `urn:cap:gonk:policy:audit:{org}` |
| Read a role revision or expansion | `urn:cap:gonk:policy:read:{org}` | none (a role names no subject) |
| Create or revise a proposal; compute its effect | `urn:cap:gonk:policy:propose:{org}` | that the author is the authenticated principal; that every `add-assignment` with a parent is covered by that parent; for one with no parent, nothing at proposal time, but only an approver can make it accepted |
| Approve | `urn:cap:gonk:policy:approve:{org}` | a passkey principal, not the author, whose effective grant covers `adds`, bound to the revision and base digests |
| Accept (apply) | `urn:cap:gonk:policy:apply:{org}` | compare-and-set on the base; approvals if widening; graph invariants; `grant_refusal` on every changed effective grant |
| Import (migration) | `urn:cap:gonk:policy:import:{org}` | owner socket only, as a member of the admin family ([src/grants.rs:964](../../src/grants.rs#L964)); the policy head's `authority` is `grants-json` |
| Export `grant:…:legacy:{name}` | `urn:cap:gonk:policy:audit:{org}` | none |

`urn:cap:gonk:policy:import:{org}` joins `CAP_GONK_ADMIN`, so it can never be granted to a
network identity. `propose`, `approve` and `apply` can be granted. They are ordinary tokens
that the owner assigns through the same proposal flow, starting with the import.

## Failure modes and refusals

Access is checked before existence, as in #3755. Every refusal names what to change.

| Class | Conditions |
| --- | --- |
| **Denied** | The caller lacks the action's token. A widening has no approval, or its approver is the author, a certificate, or does not cover the `adds`. Delegation exceeds the parent. An approver's authority was revoked between approval and accept. Claiming an item whose kind is not in `may-claim`. A spent budget. The door's own refusal when `effective` is empty or `grant_refusal` refuses it (as today, with the reason). |
| **Conflict** | The proposal's base is not the head. The approval digest does not match the proposal revision. A `rebind` targets a role revision that a concurrent accept retired. Import when the policy is already authoritative, or when `grants.json` / `clients.json` changed while the import read them (stamp mismatch). Cutover when the shadow comparison is not clean. |
| **InvalidArgument** | Parameters that fail the generator's own check (a ledger name `Ledger::parse` refuses, a scheme `alias::parse_scheme` refuses, a script name, a root that is not configured, `derive` or `nl-draft` with no mount: the same sentences `client add` prints today). A role that expands to a `grant_refusal` token. A malformed or inverted window. A child window outside the parent's. A cycle. A missing reason or owner. An unknown operation. |
| **NotFound** | After access is checked: an unknown principal, assignment, role revision, proposal or unit. |

Operational failures (a store I/O error, a corrupt policy graph) fail closed. The door
refuses every identity, just as an unparsable `clients.json` stops the server today
([src/quic.rs:21](../../src/quic.rs#L21)). They are not reported as NotFound.

## What is cacheable

- **A role expansion** is a pure function of (role revision, parameters, evaluator version)
  and is cacheable with no thread. A new evaluator version is a new name.
- **`grant:…:current`** is cacheable on a thread that every accepted change to the head cuts.
  It also expires at its `valid-until`. The door's per-call read uses it. This replaces today's
  stamp-checked file cache with a kernel-threaded one, with the same guarantee: a narrowing
  reaches the next call.
- **A proposal's effect** is cacheable, keyed by the proposal revision and base digests. It is
  immutable.
- **Not cacheable:** a response that discloses another subject's grant is filtered per caller,
  and #3755's rule applies (uncached until partitioning is proven). Refusals are not cached.

## Tests that would prove it

The test in this change pins the containment facts and set semantics. These tests belong to the
build item, through a real kernel and each door:

1. **Role parity:** for every row of the role table and every authority, `expand(gonk-…,
   params)` equals the tokens `client add … --print` resolves for the same flags, as sets.
   Combine flags pairwise and check that the union equals `scopes_for`.
2. **Containment:** a worker delegated by a writer keeps `claim`, `comment` and `session`. A
   `team-report` runner delegated by `team-*` keeps it. A per-root browse reader delegated by
   the wildcard keeps it. A token with no family rule under a parent that lacks it literally is
   dropped and counted in the effect.
3. **No child exceeds its parent:** a proposal for a child wider than its parent is Denied with
   the tokens named. Narrowing the parent later drops the child's excess in the same snapshot.
   Ending the parent deactivates the descendants. A cycle is InvalidArgument.
4. **Validity:** an assignment ending at `T` is honored at `T − 1 ns` and refused at `T` on a
   standing QUIC connection with no edit in between.
5. **Second person:** self-approval, approval by a certificate, approval by someone who does
   not cover `adds`, and approval of a superseded revision are each refused. A narrowing-only
   proposal is accepted by its author alone. A mixed proposal needs approval.
6. **Race:** two proposals on one base are accepted concurrently, and exactly one wins. The
   other gets Conflict.
7. **No whole-grant write:** after cutover, `client add` with fewer flags than the grant holds
   leaves the effective grant unchanged, and its `--print` says so.
8. **Zero-diff import:** the fixtures below. `grants.json`, `clients.json` and the passkey
   lists are imported, and every identity's effective grant is compared with today's resolver,
   including refused identities.
9. **Mid-migration door:** a standing QUIC connection and a passkey session make calls across
   the cutover. No call is refused or widened. A forced difference is planted in shadow, and
   cutover refuses.
10. **Evaluator upgrade:** a generator's expansion is changed under a fixed snapshot. Startup
    recomputes, and the recorded change names every principal it affected.

## Migration from `grants.json`

Nothing published changes shape. The capability token spellings stay the same. The
`grants.json` array shape survives as an export. The CLI flags survive as proposal
constructors, except `--force` (decision 10).

**Import (one change, `policy:…:change:1`).** The owner runs it from the socket. Its inputs are
read under the existing locks, in the existing order: clients, then grants
([src/quic.rs:151](../../src/quic.rs#L151)).

1. Run `check_grants` on the file. A file today's server would refuse at startup is refused
   here too.
2. **Decompose each grant name `G` with scopes `S_G`.** Enumerate the built-in role instances
   whose parameters appear in `S_G`'s tokens: every ledger name in a `urn:cap:ledger:*:L` or
   ledger-graph token, every alias scheme, script name, root and net host. Select each
   instance whose expansion is contained in `S_G` **literally**, then keep only the maximal
   ones (purge over delete over write, publish over run over read). Add one `gonk-scope`
   instance holding the **residue** `R_G = S_G − ⋃ selected expansions`. A generator instance
   is never selected if it would add a token not in `S_G`.
3. Create one legacy role revision per `G`. Its `kind` is `template`, its id is `legacy-{G}`,
   and its body is the list of selected instances plus the residue. Bind every identity enrolled
   under `G` to its principal, with `legacy-grant = G`:
   - a certificate, with `client_iri(fingerprint)` ([src/quic.rs:603](../../src/quic.rs#L603));
   - a passkey, with `passkey_iri(credential)`, once per grant in its list;
   - `clients.json`'s `default` grant, for each enrolled entry that names none.
4. Each binding gets one assignment of `legacy-{G}`. The fields are: no parent, the root unit,
   `valid-from` = the import time, no `valid-until`, `owner` = the importing principal, and
   `reason` = "imported from grants.json sha256:{digest}". Approvals are recorded as
   **unknown**, never invented (#3755). A shared grant stays shared: narrowing `legacy-{G}` is
   one role revision plus one `rebind` per member, and the diff shows every member.

**Zero-diff proof.** For every identity `i` that either file names, compare
`old(i)` with `new(i)`:

- `old(i)` is today's resolver result: `quic::authority` for a fingerprint, or
  `scopes_of_all` for a passkey. It is either a set or a refusal reason.
- `new(i)` is `effective(principal(i), import time)`, with `grant_refusal` applied.

The import is accepted only if, for every identity, both results are refusals or both are sets
and `GrantChange::against(old, new).changes()` is false. That is set equality, the same
comparison `--force` already uses, and reordering does not count
([tests/roles_assignments_3749.rs:41](../../tests/roles_assignments_3749.rs#L41)). The
decomposition cannot make the proof fail silently. Step 2 selects only expansions contained in
`S_G`, and the residue holds every token they miss, so `⋃ selected ∪ R_G = S_G` by
construction. Root-owned assignments have no ceiling to narrow them. The proof therefore checks
the *evaluator* end to end, and the check is still run on every identity rather than inferred.
The import change records the count of identities compared, the number of differences (which
must be 0), and the per-`G` residue size. `ikigai-gonk policy import --print` shows the whole
decomposition and writes nothing. On the hub, the residue explains the 2 extra tokens per
worker, whatever they turn out to be.

## How the door keeps working mid-migration

There is exactly one authority at a time, recorded in `policy:…:head.authority`. There is never
a dual write.

1. **Before import:** `authority = grants-json`. The door is unchanged.
2. **Shadow (after import, before cutover):** `authority = grants-json`. The door still answers
   every call from the files. When the file cache's stamp changes
   ([src/file_cache.rs:1](../../src/file_cache.rs#L1)), the policy service *re-imports* as a new
   change. This is one-way: file → graph, never graph → file. On each call, the door also
   computes `new(i)` and logs any difference with the principal and the tokens. The difference
   is never enforced. The legacy writers (`client add`, `passkey invite`, `passkey grant
   add|remove`, onboarding apply) keep writing the files. The proposal endpoints are refused
   with Conflict ("policy is not authoritative").
3. **Cutover:** this is one transaction, refused with Conflict unless the shadow log has no
   differences since the last import. It sets `authority = policy`, re-runs the zero-diff
   proof, and writes the export. From the next call, the door reads `grant:…:current`. Standing
   connections are unaffected: their next call's standing check reads the new source, which
   yields the same set, so `standing_scopes` returns `None`. A passkey session's next request
   resolves the same way.
4. **After cutover:** `grants.json` and the enrollment part of `clients.json` are a
   **read-only export**, regenerated from `grant:…:legacy:{name}` on every accepted change.
   The export has the same array shape and an `epoch` / `digest` manifest beside it. Three
   things still read `grants.json` today and keep working against the export: `client list`,
   `passkey list`, and the reviewer's `gonk.review.grant`
   ([src/trigger.rs:297](../../src/trigger.rs#L297)). The legacy writers refuse with the
   equivalent proposal command. At startup, an export whose digest does not match the manifest
   (a hand edit) stops the server with the diff. It is never read as authority.
5. **Rollback:** write a fresh export from the current head, then set
   `authority = grants-json`. Never restore an older, wider file. A rollback to an older binary
   needs that export first. Because the export has the shape the older binary reads, this works.

## DECISIONS FOR BRIAN

Decisions 1–5 are the differences from [rules as of T](../contexts/rules-as-of.md), as the
hub's comment on the item asked. Decisions 6–12 are new to this spike.

1. **Generator roles (differs from #3755).** #3755 defines a role as a parameter schema plus a
   capability template. This design adds `kind = generator`: the built-in roles call
   `grants.rs` instead of holding literal tokens, so they keep the "computed, never transcribed"
   rule. *Recommend:* adopt it. Literal templates for the built-in roles would drift the first
   time a library adds a token.
2. **`unit` for #3755's `scope` (differs).** "Scope" already means a capability token
   throughout gonk (`--scope`, `scopes_for_grant`). *Recommend:* call the organization-tree
   position `unit`, and change #3755's field name to match.
3. **The effect stored with the proposal (differs).** #3755 stores the diff and an "intended
   effect". This design stores the server-computed per-principal `adds` / `removes`, and binds
   the approval to it. *Recommend:* adopt it. The second-person rule is decided on the computed
   effect, never on the proposer's claim.
4. **Approver must be a passkey person who covers the adds (narrows #3755).** #3755 requires "a
   second authenticated person". *Recommend:* additionally require a passkey principal (not a
   certificate), and require that the approver's own grant covers every added token.
5. **Single-person deployments (differs).** #3755 rules out any owner bypass. The hub
   apparently has one person. Under decision 4, no widening could ever be approved there.
   *Recommend:* no bypass. Enroll a second person's passkey with
   `urn:cap:gonk:policy:approve:default` *as part of the import* (the import is the one
   unapproved change, and it is recorded as such). If a second person is not available, the
   alternative is an explicit, recorded `sole-operator` setting on the organization, under
   which the author may approve their own widening after a 24-hour delay and every such
   approval is flagged. Brian should choose between these two. They are not equivalent.
6. **Library upgrades that change an expansion.** When ikigai-ledger 0.22.0 added `session`, a
   worker's grant widened with no approval. Under this design, that happens at the startup
   recompute. *Recommend:* treat an evaluator-version change as a recorded change with its
   effect computed. If the change widens, the startup **holds** the new expansion and keeps
   serving the old one until it is approved. A narrowing applies immediately.
7. **The organization key.** *Recommend:* one organization per gonk, named `default`, and a
   tree with one root unit at import. Units are added later by proposal.
8. **Where `covers` lives.** The four family rules are in gonk and two libraries. *Recommend:*
   implement `covers` in gonk for the build, with a test against each library's own check, and
   file an upstream item to give ikigai-core a containment hook that each library registers, so
   that `attenuate` can use it too.
9. **Reading one's own grant.** *Recommend:* every identity can read its own
   `grant:…:current` and assignments. Today an identity cannot ask what it holds, and `client
   list` is socket-only.
10. **`--force`.** *Recommend:* remove it at cutover, and make `client add` additive. Narrowing
    becomes an explicit `client grant end`. Keep the flags, so scripts that mint new identities
    keep working.
11. **Onboarding's apply.** It enrolls `onb-{name}` with `force = true`
    ([src/onboard.rs:2176](../../src/onboard.rs#L2176)). *Recommend:* after cutover, the
    controller's signed entitlement becomes the proposal's approval. The approver is the
    controller signature's owner, so a widening still has a person behind it. It is then
    applied as an `add-assignment` with `legacy-grant = onb-{name}`, and it never replaces a
    grant.
12. **Residue at import.** *Recommend:* import the residue as `gonk-scope` (zero diff) and
    list it in `policy import --print`. Do not refuse the import until the residue is
    explained, because the zero-diff proof is the point of the import. Explaining the residue
    is follow-up work that narrows, or that names a role for it.
