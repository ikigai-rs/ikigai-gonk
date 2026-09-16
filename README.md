# ikigai-gonk

**A standalone work-ledger server for [ikigai](https://github.com/ikigai-rs).** One binary
holds the machine's durable RDF store and serves named ledgers — items, comments, labels,
links, claims, and a ranked `next` — through three doors: HTTP on loopback port 1060, with a
browser face for reading, filing, editing, closing, deleting and querying; an owner-only Unix
socket; and QUIC for other machines, where every connection is admitted by a client
certificate and runs under the grant that certificate is enrolled for.

```text
$ ikigai-gonk
ikigai-gonk 0.1.0 — holding the store at /Users/you/.ikigai/store
  http    http://localhost:1060/ — loopback (127.0.0.1:1060); anonymous read+write: default; 0 passkey(s)
  browse  not composed — no gonk.browse.root; urn:repo:* and the git/gh facades are not bound
  llm     not mounted — no gonk.mount; urn:llm:* resolves nowhere, so explain, review and the PR-derived layers are not bound
  socket  /Users/you/.ikigai/gonk.sock — owner only
  quic    off — no client certificate is enrolled (there is no /Users/you/.config/ikigai/gonk/clients.json); to open it, run `ikigai-gonk client add <name> --ledger <ledger>=write` and restart
  mount   mount = "prefer urn:iki:ledger:=/Users/you/.ikigai/gonk.sock"  (and the same for urn:iki:store:)

$ curl -X POST --data-binary 'Wire gonk into the cli' http://127.0.0.1:1060/iki/ledger/append
#1 urn:iki:ledger:default:item:01m2h3t8337vvs9j

$ curl http://127.0.0.1:1060/iki/ledger/items
   #1  open    p-  Wire gonk into the cli

1 item(s)
```

## Install and run

From a clone:

```sh
cargo install --locked --path .
ikigai-gonk
```

That is the whole setup. The dataset lives in `~/.ikigai/store`, the socket at
`~/.ikigai/gonk.sock`, and HTTP on `127.0.0.1:1060`. No configuration file is needed, and no
certificate is needed until another machine has to reach it.

`ikigai-gonk --help` prints every flag and file.

## In a browser

Open **<http://localhost:1060/>** — `localhost`, not `127.0.0.1`: a passkey is bound to a
name, and a browser refuses an IP address as one.

- **Browse.** The front page is the first ledger you may read, open items first, with filters
  for closed and all and a title search. Each item has its own page: body, filing metadata,
  labels, links, `about` targets and comments.
- **File, edit, work.** A form on the ledger page files an item (first line the title, a blank
  line, then the body). An item page comments, edits the title, body and priority, closes with
  a reason or reopens, claims or releases, defers or resumes, labels and links.
- **Delete and purge are two acts.** *Delete* moves the item into the ledger's graveyard graph
  and leaves a tombstone; it is recoverable by hand. *Purge* destroys the content in both
  graphs and leaves only the tombstone. They are separate sections with separate
  confirmations, and each appears only for a caller whose grant holds it.
- **Query.** <http://localhost:1060/sparql> runs SELECT, ASK, CONSTRUCT and DESCRIBE over one
  ledger's graph and renders the answer as a table (or Turtle, or a boolean). Eight sample
  queries sit above the editor — open by priority, p1 only, a `COUNT` by `repo:` label,
  security, recently updated, items with comments, closed with their reasons, and a body-text
  search to edit. A sample fills the box and runs nothing: you press Run. A `ledger:` IRI
  reads as its local name (`open`, not `…/ledger#open`) with the full IRI on the cell; the
  same URL answers a machine in the store's own formats, raw IRIs and all: `curl -H 'Accept:
  application/sparql-results+json' 'http://127.0.0.1:1060/sparql?query=…'`.
  ⚠ There is deliberately **no "oldest" sample**: `dcterms:created` is when an item was
  *filed*, and a bulk migration files hundreds in one minute, so it does not say how long the
  work has waited. The page says so next to the buttons.

The face is hypermedia: server-rendered HTML with [htmx](https://htmx.org) for the in-place
updates, no single-page app and no build step. Every page is a **transform of a graph face** —
a ledger page is `urn:iki:ledger:{ledger}:items as=text/turtle`, re-serialized as RDF/XML and
rendered by one XSLT stylesheet whose templates match on `rdf:type`, server-side through
`ikigai-xslt`. It works at phone width, follows the system's light or dark scheme, and its
palette is held to the WCAG AA contrast floor in both by a test.

### Passkeys: who you are, and what that grants

An anonymous caller on this machine can read and write the ledgers in `gonk.http.ledger` —
exactly what the HTTP door granted before it had a face. Anything more, such as delete, purge
or another ledger, belongs to an identity:

```sh
ikigai-gonk passkey invite brian --ledger default=delete
```

```text
passkey invite for `brian` — grant `brian` (6 scopes) in /Users/you/.config/ikigai/gonk/grants.json
  valid for 30 minutes, once
  open  http://localhost:1060/#invite=…
```

What you will see:

1. **Open the link** in a browser on this machine. A panel, *Create a passkey for this
   server*, asks for a label (say `laptop Touch ID`).
2. **Create passkey.** The browser's own passkey sheet appears: Touch ID, a security key, or a
   phone. Approve it. The server enrols the credential's public key under the grant `brian`,
   the panel closes, and the page says *Passkey created for … (grant brian). Now click Sign in
   with passkey to use it.*
3. **Sign in with passkey**, in the header, and approve the sheet a second time. The page
   reloads showing *Signed in as … (grant brian)*, and that browser holds the grant until it
   signs out, the session's 12 hours end, or the server restarts.

The second click is deliberate. When the creation sheet closes it still holds the window's
focus, and a browser refuses a sign-in prompt from a page without focus, so gonk waits for
the click rather than chaining the two. If a sheet is cancelled or times out, the page says so
and names the button to click again. A creation that did not finish leaves the invite unused,
good until it expires.

- **One grant table for both identity doors.** A passkey is an identity the way a client
  certificate is. Both map to a grant name in `gonk/clients.json` (certificates under
  `clients`, passkeys under `passkeys`), and the grant name maps to scopes in
  `gonk/grants.json`, the one file where scope lists live. Enrol a laptop certificate and a
  passkey under the same name and they hold the same authority, and editing the grant changes
  both on the next request or connection. Delete a passkey's entry from `clients.json` to
  revoke it, with no restart.
- **An identity is strictly stronger than anonymous.** A signed-in caller holds the anonymous
  grant plus its own, and `passkey invite` refuses a grant that adds nothing.
- **The invite is the registration's trust anchor.** No attestation is parsed. The question
  that matters is whether this person was invited, and a single-use, expiring code answers it.
  The server stores only the code's SHA-256.
- **Verification** is `ikigai-passkey`'s: ES256, the exact origin `http://localhost:<port>`,
  RP ID `localhost`, a single-use challenge, user verification required, and a signature
  counter that must not go backwards.
- ⚠ **The session cookie is set by the page's script, so it is not `HttpOnly`.** The HTTP
  transport builds every response from a resource representation and cannot add `Set-Cookie`.
  What remains is `SameSite=Strict`, a same-origin Content-Security-Policy with no inline
  script, and a session table that lives in memory.

### What the HTTP door refuses a browser

A door that grants writes to whatever reaches loopback also grants them to any web page open
on the machine: a page on any site can `POST` a form to `http://127.0.0.1:1060/` without a
preflight. So the capability is computed per request, and two browser-only signals take it
away:

- a write whose `Origin` or `Sec-Fetch-Site` names another site gets **nothing** (403). A local
  process such as `curl` or a script sends neither header, so it is unaffected;
- a request whose `Host` is not `localhost`, `127.0.0.1` or `[::1]` gets **nothing**, reads
  included. That is the DNS-rebinding defence.

### SPARQL, confined by construction

`/sparql` never touches the store's whole-dataset doors. A query runs at
`urn:iki:store:graph-{select,ask,construct,describe}` with `graph=` set to the chosen
ledger's graph and **the caller's** capability. The store sets that graph as the query's entire
dataset before evaluation, so there is nothing else to see, and a grant naming one ledger
cannot read another. `FROM` and `FROM NAMED` are refused rather than silently overridden; the
page says so. It is read-only: an update is not a query form it runs.

### Routes

| path | resource | |
| --- | --- | --- |
| `/` | `urn:iki:gonk:page:home` | the first readable ledger |
| `/l/{ledger}` · `/l/{ledger}/items` | `urn:iki:gonk:page:ledger:{ledger}` · `…:fragment:items:{ledger}` | a ledger, page and fragment |
| `/l/{ledger}/item/{id}` · `…/card` | `urn:iki:gonk:page:item:{ledger}:{id}` · `…:fragment:item:…` | one item |
| `POST /act` | `urn:iki:gonk:act` | a form, as one ledger action |
| `/sparql` · `/sparql/results` | `urn:iki:gonk:sparql` · `…:fragment:sparql` | query |
| `POST /auth/{op}` | `urn:iki:gonk:passkey:{op}` | passkey ceremonies and sessions |
| `/static/{name}` | `urn:iki:gonk:asset:{name}` | `gonk.css`, `gonk.js`, `htmx.min.js` |

These exist **only on the HTTP door**. The socket and QUIC doors serve exactly the store and
the ledger, as before, and `tests/conformance.rs` pins both catalogs.

**Every form issues an action the ledger already declares.** `/act` builds the target from
`ikigai-ledger`'s own naming (`urn:iki:ledger:{ledger}:{action}`, or `…:item:{id}`), refuses a
verb the target does not describe, and refuses any field that the verb's contract does not
name. It runs under the caller's capability, so the ledger's checks decide.

## Filing and reading work over HTTP

Every resource of [`ikigai-ledger`](https://github.com/ikigai-rs/ikigai-ledger) is reachable
by the mechanical mapping `ikigai-web` uses: the path's segments become the IRI, the method
the verb, query parameters the arguments, the body the content, and `Accept` the
representation. `as` and `content` are the transport's own names, so a query parameter
spelled either is ignored.

```sh
curl -X POST --data-binary $'Publish the store\n\nIt needs a README.' http://127.0.0.1:1060/iki/ledger/append
curl http://127.0.0.1:1060/iki/ledger/items
curl http://127.0.0.1:1060/iki/ledger/next
curl -H 'Accept: text/turtle' http://127.0.0.1:1060/iki/ledger/items
curl -X POST 'http://127.0.0.1:1060/iki/ledger/close?item=1'
```

| method | path | resource |
| --- | --- | --- |
| `POST` | `/iki/ledger/append` | `Sink urn:iki:ledger:append` |
| `GET` | `/iki/ledger/items` | `Source urn:iki:ledger:items` |
| `GET` | `/iki/ledger/acme/next` | `Source urn:iki:ledger:acme:next` |
| `DELETE` | `/iki/ledger/item/01m2h…` | needs the delete grant — anonymous callers are refused |

⚠ These resource paths are for programs, not browsers. `ikigai-web` turns the **first** type in
`Accept` into the requested face, and a browser's first type is `text/html`, which the ledger's
own resources do not serve. Opening `/iki/ledger/items` in a browser therefore answers
`400 … text/html is not a face this resource serves`. The pages above are the browser's way in.

A path nothing serves answers `404` with a sentence naming where to start.

## From another ikigai process on this machine

Gonk holds the dataset, so nothing else opens it. Every other ikigai process — the REPL, a
script, an agent — reaches the ledger and the store through gonk's socket. Two lines in
`~/.config/ikigai/config.toml`, because a mount claims one prefix:

```toml
mount = "prefer urn:iki:store:=/Users/you/.ikigai/gonk.sock"
mount = "prefer urn:iki:ledger:=/Users/you/.ikigai/gonk.sock"
```

```sh
ikigai -c 'sink urn:iki:ledger:append Filed from the cli' -c 'source urn:iki:ledger:items'
```

A host that used to open the store itself (`store = true` in that file) drops that line and
takes these two. If both run at once, the second to start is refused by RocksDB with the
path it tried and these two lines.

With browse roots configured, two more prefixes are served here — and they are separate mount
lines for the same reason, plus one spelling that is easy to get wrong:

```toml
mount = "prefer urn:repo:=/Users/you/.ikigai/gonk.sock"
mount = "prefer urn:iki:annotation=/Users/you/.ikigai/gonk.sock"   # ⚠ no trailing colon
```

The annotation line has no trailing colon deliberately: mounts match by plain string prefix,
and `urn:iki:annotation` covers both `urn:iki:annotation:{id}` and the bare
`urn:iki:annotation` that a Sink mints under — the only write path the family has. Written
`urn:iki:annotation:=`, every new annotation fails to route, silently.

⚠ **`urn:repo:` is one prefix over two families.** That mount sends `urn:repo:{root}:file:…`
(browse) *and* `urn:repo:status`, `urn:repo:log`, `urn:repo:pr:list` (the facades) to gonk,
where they run in gonk's process, under gonk's working directory, with whatever capability the
caller carried across. A host that wants the facades local and only the roots remote cannot
say so with a prefix mount. (`urn:system:exec` is outside that prefix and stays wherever the
calling host binds it, unless mounted by name.)

## From another machine, over QUIC

The QUIC door opens once a client **certificate** is enrolled. Passkeys never open it: they
live in the same `clients.json`, but they sign in on the HTTP door only. Setting
`gonk.quic.bind` (or `--quic-bind`) turns "once a certificate is enrolled" into "must open":
with a bind named and no certificate to admit, gonk refuses to start rather than quietly
leaving the door shut. On the gonk machine:

```sh
ikigai-gonk client add laptop --ledger default=write
```

That generates this server's identity on first use, mints a client identity into
`~/.config/ikigai/gonk/quic/clients/laptop/`, and enrols its certificate fingerprint under a
grant called `laptop` holding exactly the tokens for reading and writing the `default`
ledger. Restart `ikigai-gonk` — the set of trusted certificates is read at startup — and the
banner reports `quic  udp 0.0.0.0:1060`.

Copy that directory to the other machine; it holds `client.crt`, `client.key` and the
`server.crt` to pin, laid out as the cli's `--cert-dir` expects. There, with a cli built with
QUIC:

```sh
cargo install --locked ikigai-cli --features quic
ikigai --connect quic://gonk-host:1060 --cert-dir ~/gonk-laptop -c 'source urn:iki:ledger:items'
```

or, to put the ledger into that machine's own ikigai processes:

```toml
mount = "prefer urn:iki:store:=quic://gonk-host:1060 /home/you/gonk-laptop"
mount = "prefer urn:iki:ledger:=quic://gonk-host:1060 /home/you/gonk-laptop"
```

**To keep a client's private key on the client**, let it generate its own identity and
enrol only the certificate: `ikigai cert generate --dir ~/gonk-laptop` there, copy its
`client.crt` here, and `ikigai-gonk client add laptop --cert client.crt --ledger
default=write`; then copy the bundle's `server.crt` back over the one in `~/gonk-laptop`.

What admission means today: a client holding a trusted certificate and an enrolment is
admitted until its entry is removed from `clients.json`, which takes effect on its next
connection. There is no certificate authority, no expiry, no rotation and no automated trust
distribution — the server certificate is pinned by copying it.

`ikigai-gonk grants <ledger> <read|write|delete|purge>` prints the token list for one
ledger, for writing `grants.json` by hand.

## Explaining what it browses

With a browse root configured and a **mounted model**, gonk serves `ikigai-browse`'s
explanation families over the same dataset: an LLM-derived orientation for a file or a
directory, a review pass whose findings are minted as real annotations, and the same two for
a pull request.

```toml
gonk.browse.root = "core=~/git-personal/ikigai-core"
gonk.mount = "prefer urn:llm:=quic://127.0.0.1:4433 ~/.config/ikigai/gonk/quic/peers/plasma"
```

```text
urn:repo:{root}:explain[:{path}]           an orientation, archived
urn:repo:{root}:explain-versions[:{path}]  what the archive holds — derives nothing
urn:repo:{root}:review:{path}              findings as annotations in this dataset
urn:repo:{root}:pr:{n}:explain, :review    the same two over a pull request
```

**The model is mounted, not linked**, and that is the decision this feature turned on. gonk
could have depended on `ikigai-llm` and talked to Ollama itself; then this binary would carry
an outbound HTTP client, on a server whose argument for what it is safe to run is largely an
argument about what is compiled into it. Instead `urn:llm:` resolves on a peer — an
`ikigai serve quic://…` that lends its inference, or an ikigai host on this machine over its
socket — and what gonk gains is one connection to one configured address.

**`prefer`, and what it means for a ledger server.** The cli's `prefer` is an override
wrapped in a failover over the local spaces: the peer when it answers, this machine when it
does not. gonk binds nothing under `urn:llm:`, so there is no local half — and the half that
matters here is the other one: **an absent peer costs explain and nothing else.** gonk does
not dial at startup, a failed dial is held off for thirty seconds rather than retried per
request, and a ledger read never touches the mount. A peer that is down makes a derivation
`Unavailable` — the transient it is — not `Denied` and not `Unresolved`.

**Binding is a property of the config, not of reachability.** With no `gonk.mount` line the
explanation rows are not bound at all, because an action the manifold offers and the kernel
can never satisfy is an over-offer. With one, they are bound whether or not the peer answers
today.

**Which peer to mount.** Either works, and the choice is about topology rather than
protocol. A socket (`prefer urn:llm:=~/.ikigai/host.sock`) needs no certificates and never
touches a network interface, but it couples gonk's model traffic to whatever else that host
serves — on this machine, the personal agent that holds the calendar and contacts grants,
which is the wrong neighbour for a server whose blast radius is the point. A QUIC peer needs
a client certificate the peer has enrolled, and is **the same configuration whether the model
is on this machine or another one**: mounting a bigger machine's inference later changes the
host in one line and nothing else. That is why the line above is the one this README shows.

**Getting the certificate**, which is the same relationship as
[the QUIC door](#from-another-machine-over-quic) with gonk on the other side of it. On the
gonk machine, generate an identity for this mount and hand the peer its public half:

```sh
ikigai cert generate --dir ~/.config/ikigai/gonk/quic/peers/plasma
# on the peer: trust it, then restart the peer (trusted certs are read at startup)
cp ~/.config/ikigai/gonk/quic/peers/plasma/client.crt <peer cert-dir>/clients/gonk.crt
# and back on the gonk machine, pin the peer:
cp <peer cert-dir>/server.crt ~/.config/ikigai/gonk/quic/peers/plasma/server.crt
```

The directory holds `client.crt`, `client.key` and the `server.crt` to pin — the same layout
`ikigai --cert-dir` uses, and the same one `ikigai-gonk client add` writes for gonk's own
clients. `gonk.mount` is the ONLY key that names it, and `urn:llm:` is the only prefix a
mount line may claim: a mount is composed in front of the local spaces and its catalog is
served through every door, so what these doors can serve stays a property of the manifest
plus one namespace.

**What a derivation costs, and what bounds it.** Each grain has a provider and a `max_tokens`
ceiling (`gonk.explain.*`), and the ceiling is the only bound on one call — a thinking model
with no ceiling burns the budget on reasoning and returns nothing. Over time the bound is the
**archive**: an explanation is keyed by `(path, content-hash, version-tag)` in this dataset
and derived once per content version, so the second read of an unchanged file is a store
read. Measured on a scratch repository against `quic://127.0.0.1:4433`: the file grain
derived in **10.2s** on `qwen3-coder:30b` and came back from the archive in **13ms**; the
directory rollup derived in 61s on a 70B model, because a rollup asks the default tier.

A model swap on the peer re-keys the archive by itself: the version tag folds the model
identity `ikigai-browse` resolves through `urn:llm:{provider}:model` at explain time, which
is why gonk sets no model label. A `provider=` argument may name only the tiers this server
already asks with — widening that set would let anyone who may explain point this server at
any backend the peer holds, and that is not a caller's decision.

**Who may spend** is [the doors table](#the-three-doors), and the short form is that an
anonymous HTTP caller holds no browse grant at all, so it can neither derive an explanation
nor read an archived one, while the socket door's root can do both.

## The three doors

| door | reaches it | runs under |
| --- | --- | --- |
| **HTTP**, `127.0.0.1:1060` | any process on this machine | anonymously, the read and write tokens of the ledgers in `gonk.http.ledger` (default: `default`); signed in, that plus the passkey's grant; nothing for a non-loopback peer, a foreign `Host`, or a cross-site write |
| **socket**, `~/.ikigai/gonk.sock` | this user only (`0600`, peer UID checked) | root — the socket's owner can already read the dataset's files |
| **QUIC**, `udp 0.0.0.0:1060` | a certificate under `gonk/quic/clients/` | the grant its fingerprint maps to in `gonk/clients.json`; refused when it maps to none |

**Anonymously, the HTTP door is loopback only and grants little.** It can list, file,
comment, close, claim, label and link in its ledgers. It cannot delete or purge, cannot reach
another ledger, and cannot touch the store's whole-dataset doors (`urn:iki:store:select`,
`urn:iki:store:update`), because none of those tokens are in its grant. A passkey adds exactly
its grant and nothing else.

**And with browse roots configured, that argument has to be re-made, because more is now
compiled in.** `urn:repo:*` reads files off the disk and runs `git`; `urn:system:exec` runs a
named tool; `urn:iki:annotation` writes to the dataset. What each door reaches:

| family | anonymous HTTP | signed-in HTTP | socket | QUIC |
| --- | --- | --- | --- | --- |
| the configured ledgers | read + write | + the passkey's grant | all | per grant |
| `urn:repo:{root}:*` (`urn:cap:browse:read:{root}`) | **no** | only if the grant names it | yes (root) | only if the grant names it |
| `urn:iki:annotation` (`urn:cap:annotate`) | **no** | only if the grant names it | yes (root) | only if the grant names it |
| `urn:repo:{status,log,…}`, `urn:system:exec` (`urn:cap:exec:{tool}`) | **no** | only if the grant names it | yes (root) | only if the grant names it |
| `urn:iki:store:select` and the other broad doors (`urn:cap:store:read`) | **no** | **no** — this server hands the broad tokens to nobody | yes (root) | **no** — refused in `grants.json` |
| `urn:repo:{root}:{explain,review}`, `pr:{n}:{explain,review}` — **spends model tokens** (`urn:cap:net:{host}`, plus `urn:cap:annotate` for the two reviews) | **no** | only if the grant names it | yes (root) | only if the grant names it |
| `urn:llm:*` on the mounted peer (`urn:cap:net:{host}`) | **no** | only if the grant names it | yes (root) | only if the grant names it |

**Deriving is the privileged act, and it is one capability away from every door.** Explaining
a file, reviewing one, or explaining a pull request calls a model on the mounted peer, which
costs the operator tokens and — where the peer is metered — money. `ikigai-browse` declares
that as `urn:cap:net:*` (the offering wildcard) on every derivation, and `declared =
enforced`, so a caller with no net grant is refused before dispatch and never appears in
front of a model. **This server mints no net grant**: `ikigai-gonk grants`, `client add` and
`passkey invite` write per-ledger tokens only, so an identity that may derive is one an
operator wrote by hand into `grants.json`, naming the host
(`urn:cap:net:localhost`) — the wildcard itself is refused there, like `urn:cap:exec:*`.

⚠ **A net grant is the authority to spend that peer's inference, not only to explain.** The
mount serves the peer's whole `urn:llm:` namespace through every door, so the same grant that
derives an explanation can `source urn:llm:ask` directly — and a direct ask is neither
archived nor bounded by the `gonk.explain.*` ceilings, because those are arguments this
server passes to an explain, not a policy the mount enforces. What bounds it is the peer's
own ceiling (`ikigai serve quic://… --cap urn:cap:net:localhost` grants inference and nothing
else) and the fact that reaching the mount at all takes a grant nothing here mints.

Three things make that table true rather than aspirational. `ikigai-gonk grants` mints
per-ledger tokens only, so nothing this server writes into a grant names browse, exec or the
whole dataset. `grants.json` is refused at startup if any grant names a whole-dataset store
token **or the exec wildcard `urn:cap:exec:*`** — which `ikigai-repo` declares as an
*offering* ("holds some grant under this prefix") and which as a *grant* means every program
on the machine; the per-tool spelling `urn:cap:exec:git` is what that crate enforces at
dispatch and what an operator means. And `urn:kernel:actions` is capability-scoped by
construction, so an anonymous caller asking "what can I do?" is offered no browse row, no
facade, no broad store door and no derivation — `tests/browse.rs` asserts both halves, the
offer and the typed `Denied`, for a caller with per-ledger tokens, for one with a browse
grant and no net grant, and for one with both.

**The socket door is root, and root now reaches further.** An owner-only socket client can
read any file under a configured root and run `git`/`gh` through the facades. That is
authority the same user already has — the socket is `0600` with the peer UID checked, and
whoever holds it can read the dataset's files and run `git` themselves — but it IS a wider
surface than before, and it is reached by mounting this socket, so a host that mounts gonk
mounts all of it.

**Every door shares one kernel.** A kernel owns its cache and its golden threads, and each
transport takes one by value; separate kernels over one store would let a write through one
door leave another serving the read it cached before. So there is one kernel, the hub, and
every door forwards to it under a cache that stores nothing. The HTTP door's kernel adds the
pages in front and a not-found catch-all behind, and its pages never cache; every ledger read
a page makes is a hub read.

## Remote access, later — what it will take

The HTTP door refuses a non-loopback bind, and that refusal stays until all of these exist:

1. **TLS.** WebAuthn needs a secure context. `http://localhost` is one and `http://` on a LAN
   never is, so a passkey cannot work off loopback without HTTPS.
2. **A real relying-party name.** The RP ID is `localhost` today, and an IP address cannot be
   one. A remote face needs a domain name, and a passkey registered for `localhost` does not
   carry over to it; people re-enrol.
3. **No anonymous grant off loopback.** A non-loopback peer already gets an empty anonymous
   capability, so everything it can do would come from its passkey. That is the property
   that makes a remote bind meaningful. The bind refusal should lift only for a TLS listener,
   so the anonymous-loopback grant can never face a network.
4. **The cookie question revisited.** A remote session should be an `HttpOnly`, `Secure`
   cookie set by the server, which needs a transport that can set response headers.

Until then, other machines use the QUIC door.

## What it refuses

- **A non-loopback HTTP bind.** `--bind 0.0.0.0:1060` stops the server before it opens
  anything, and says to use QUIC.
- **A second holder of the store.** Another gonk, or an `ikigai` with `store = true`, holding
  the dataset stops this one with the path and the mount lines that fix it.
- **The store's broad tokens in any grant.** `urn:cap:store:read` is every graph and
  `urn:cap:store:write` is `DROP ALL`. A `grants.json` naming either stops the server at
  startup, and a connection or session whose grant names one is refused. The store's narrow
  write door refuses the broad key anyway, so such a grant could not write either.
- **A certificate it trusts but has no grant for**, or whose grant is unknown or empty. The
  refusal is logged with the full fingerprint.
- **A QUIC door it was told to open and cannot.** A named QUIC bind with no client
  certificate enrolled, or a certificate enrolled with no `client.crt` trusted, stops the
  server. Neither case generates the server identity first.
- **An unreadable authority file.** A `clients.json` (certificates or passkeys) or
  `grants.json` that exists and does not parse stops the server rather than serving under a
  guess.
- **A passkey ceremony that is not exactly right**: the wrong origin (an IP address instead of
  `localhost`), a reused or expired challenge, a used or expired invite, a key that is not
  P-256, an assertion without user verification, or a signature counter that went backwards.
- **More than 256 outstanding challenges or sessions.** Past the bound a new one is refused;
  nobody else's in-flight sign-in is evicted.
- **A `gonk.*` config key it does not read**, and a socket path too long to bind.
- **A browse root that is not there, is named twice, or is named something that reads as
  another family** (`pr`, `style`, or a name containing `:`, `/`, `{`, `}`). A missing
  directory looks exactly like an unconfigured root once the server is up — a resolution
  miss — so it is refused while there is still somewhere to print the reason.
- **A grant naming `urn:cap:exec:*`**, the offering wildcard, which as a grant is every
  program on this machine. Name the tools: `urn:cap:exec:git`, `urn:cap:exec:gh`.
- **A grant naming `urn:cap:net:*`**, the offering wildcard `ikigai-browse` declares on every
  derivation, which as a grant is every host this kernel could dial. Name the host:
  `urn:cap:net:localhost`. Both wildcards are checked at startup and again per connection,
  because the file is re-read per connection.
- **A `gonk.mount` line that is not `prefer urn:llm:=<target>`**: another mode, another
  prefix, a `quic://` target with no certificate directory (or one that is not there), a
  socket target WITH one, or two lines claiming the same prefix.
- **A `gonk.explain.*.provider` outside `urn:llm:`**, which nothing in this process could
  resolve, or a `max_tokens` that is not a positive number — a ceiling is the only bound on
  what one derivation costs, so a typo in it must stop the server rather than fall back.

## Configuration

Flags override config wholesale; there is no environment-variable channel. The config home
is `~/.config/ikigai` (or `$XDG_CONFIG_HOME/ikigai`).

```toml
# ~/.config/ikigai/config.toml
gonk.bind = "127.0.0.1:1060"          # or gonk.port = 1060, which always means loopback
gonk.socket = "~/.ikigai/gonk.sock"
# gonk.quic.bind = "0.0.0.0:1060"     # unset: QUIC opens here once a certificate is enrolled;
                                      # set: QUIC must open, or gonk refuses to start
gonk.http.ledger = "default"          # repeatable
gonk.browse.root = "core=~/git-personal/ikigai-core"   # repeatable; unset, no browse family
gonk.mount = "prefer urn:llm:=quic://127.0.0.1:4433 ~/.config/ikigai/gonk/quic/peers/plasma"
# gonk.explain.file.provider = "urn:llm:coder:ask"     # the tiers, and the per-call ceilings
# gonk.explain.file.max_tokens = 400                   # file 400, dir 600, review 800, pr 600
# gonk.explain.dir.provider = "urn:llm:ask"            # .dir / .review / .pr take the same pair
# gonk.explain.max_prompt_bytes = 16384
```

A `gonk.browse.root` line is what composes `urn:repo:*` and `ikigai-repo`'s facades at all.
The name is spliced into `urn:repo:<name>:…`, so it may not contain `:`, `/`, `{` or `}`, may
not repeat, and may not be `pr` or `style` (those read as the other family's names); the
directory must exist. Each is refused at startup with the line to edit — `ikigai-browse`
would assert instead, which arrives as a panic where the banner should be.

| file | what it holds |
| --- | --- |
| `store.toml`, `gonk.store.toml` | `path = "…"` — where the dataset lives, read by `ikigai-store` |
| `gonk/clients.json` | identity → grant name: certificate fingerprints under `clients` (the same shape `ikigai serve quic://…` reads), passkeys under `passkeys` |
| `gonk/grants.json` | grant name → capability scopes (the same shape as the cli's `grants.json`) |
| `gonk/invites.json` | outstanding passkey invites, by the SHA-256 of their codes |
| `gonk/quic/` | `server.crt`, `server.key`, and one `clients/<name>/` bundle per client |

## What it composes

The manifest is the module manifest: gonk links
[`ikigai-store`](https://github.com/ikigai-rs/ikigai-store) with its RocksDB backend,
[`ikigai-ledger`](https://github.com/ikigai-rs/ikigai-ledger),
[`ikigai-browse`](https://github.com/ikigai-rs/ikigai-browse) and
[`ikigai-repo`](https://github.com/ikigai-rs/ikigai-repo), the `ikigai-web`, `ikigai-ipc` and
`ikigai-quic` transports, and `ikigai-resolve` for the mount. For the browser face it adds two
libraries it calls as functions and binds no resources from: `ikigai-xslt` (the stylesheet
engine) and `ikigai-passkey` (the assertion verifier). **No LLM client** — the model is
mounted, never linked.

⚠ **That is a correction, and it has now been made twice.** Through 0.1.0 this section said
"no filesystem module, no process execution and no outbound network client is compiled in, so
none is reachable whatever a grant says". The browse family made two of those three false:
`urn:repo:{root}:file` and `:tree` read the filesystem under a configured root, and
`urn:repo:{status,log,branch}`, `urn:repo:pr:*` and `urn:system:exec` spawn `git` and `gh`.

**The third has now changed too, and it is worth stating exactly rather than dropping.** With
a `gonk.mount` line, gonk opens an outbound connection — QUIC, or a Unix socket — **to one
peer named in its config, presenting one client certificate, speaking ikigai's own wire
protocol**. That is narrower than what the old sentence ruled out in three ways that matter:

- **No HTTP client is linked, and none is reachable.** `urn:httpGet` and its family are not
  bound here; `ikigai-llm` is not a dependency. A capability cannot produce a request to an
  arbitrary URL, because nothing in this process can build one.
- **The address is config, not an argument.** A caller cannot say where to connect. The peer
  is the one `gonk.mount` names, and the only IRI prefix a mount line may claim is
  `urn:llm:` — so even an operator cannot quietly put another namespace behind these doors.
- **The peer's own ceiling is the far end.** `ikigai serve quic://… --cap urn:cap:net:localhost`
  admits gonk's certificate to inference and to nothing else on that machine.

What it is NOT narrower in: reaching the peer at all is a capability question now
(`urn:cap:net:{host}`), not a linkage question — and that capability spends tokens. See
[The three doors](#the-three-doors). All three families are **off unless configured**: with no
`gonk.browse.root` line neither browse nor the facades are bound, and with no `gonk.mount`
line the explanation families are not bound either, because an action no kernel in this
process can satisfy is an over-offer.

`tests/conformance.rs` pins the socket and QUIC catalog to the store's twelve resources and the
ledger's fourteen, pins the HTTP door's to those plus its ten pages, and walks
`ikigai-conformance` over the hub, a door, and the HTTP door. `tests/browse.rs` pins the
browse composition's twenty more, in the hub and through a door, pins the five the mount adds
(and their absence without one), and pins the spend gate per capability.

### One dataset, and what it costs

The annotation family takes a handle on the store, so its quads land in the same dataset the
ledgers live in and a ledger item joins an annotation on a repo file in one local query — the
point of composing them here rather than federating.

`ikigai-store` answers a bare handout (`open_shared`) by making **every** read
`Expiry::Always`, because a handle it cannot see is a writer it cannot see; and expiry
propagates, so that would have de-cached every ledger read as a side effect of adding a browse
face. Measured on a 247-item ledger, `urn:iki:ledger:items` goes from **11.9µs to 12.9ms** that
way — a thousandfold, with every test still passing and the types identical.

gonk does not accept that, and it does not have to, because it knows something the store
cannot: **who the other holder is and where it writes.** `ikigai-browse` puts every quad it
stores into the **default graph**, hard-coded in all three of its writers, so `main` opens with
`DurableStore::open_shared_declaring(path, SharerWrites::only_the_default_graph())` and the
store answers freshness per read from that promise: a scoped read
(`urn:iki:store:graph-{select,ask,construct,describe}`), whose universe is one NAMED graph by
construction, is cacheable again under the store's own three write threads — and that is what
every ledger read is made of. Declared, the same read is **11.9µs**. The broad faces stay
uncacheable, by name: `urn:iki:store:{select,ask,construct,describe}`, `urn:iki:store:info`,
and therefore `urn:iki:ledger:ledgers`, which asks *which graphs exist* through the broad door.

⚠ **A false promise there would be silent, unbounded staleness** — reads of a graph the sharer
does write, cached against threads its writes never cut. So the promise is not left to a
comment: `tests/browse.rs` prints all three numbers and takes the store's own
`reserved_graphs_fingerprint()` either side of a real browse write, which fingerprints the
**quads** of every reserved graph rather than the set of graph names, and errors rather than
passing vacuously on a store that promised nothing.

### Watched roots, and why the reads are cached at all

`ikigai-browse` declares its `tree`, `file`, `hash` and `state` reads live and uncacheable,
which is the only honest declaration a *library* can make: caching is a promise that something
will notice when the file changes, and a library cannot know whether its host is watching. A
server can. gonk watches every configured root (FSEvents/inotify, recursively, `.git`
included — `state` is `git` output, so the refs are its input) and cuts one golden thread per
root, `urn:iki:gonk:browse:root:{name}`, on any change beneath it.

Only a **watched** root's reads are declared cacheable, and only for a plain `Source` with no
arguments: the HTML and `annotations=include` faces read the annotation overlay, which browse
rewrites during a read when a file has drifted, and that is state this thread does not track.
A root whose watcher fails to start is named on the banner as `live, unwatched` and its reads
are served exactly as browse declares them. There is no composition in which a read is cached
and unwatched — the failure the ecosystem already had once, where `urn:file:*` was cacheable
on a kernel with no watcher and a replaced stylesheet was served stale until restart, is the
failure this ordering exists to make impossible. Measured on a scratch root, a watched
`tree` read goes from 33.9µs to 6.7µs and a `file` read from 31.0µs to 8.3µs.

⚠ **Invalidation is asynchronous**, with the platform's own latency between the write and the
notification (FSEvents coalesces; a read issued in the same instant as the edit can still be
served from the cache). It is a bound of well under a second, not a correctness hole — the cut
always arrives — but a script that writes a file and reads it back with no gap can see the
previous version once.

`urn:repo:style` is browse's own: it is cacheable with a thread per `a11y.toml` candidate, and
gonk starts the watch that crate ships for them (`Mount::space_watched`), so an edit to
`~/.config/ikigai/gonk.a11y.toml` lands on the next read rather than the next restart.

For the resources themselves — named ledgers, the grant table, delete versus purge, ordering
policies — see `ikigai-ledger`'s README.

## Port 1060

Port 1060 is the default for both HTTP (TCP) and QUIC (UDP), which are separate namespaces, so
one number names the server. It is also NetKernel's own port, registered with IANA as
`polestar`: a machine running both collides, which is why it is a default and not a constant.
`--port`, `--bind` and `--quic-bind` move it.

The name and the number are one tribute. GONK carries NK in order; the GNK power droid plods
around carrying charge so the rest of the scene can work. And 1060 is XML read as Roman
numerals and summed — X + M + L — for the orientation NetKernel 3 was built around.

## Running it as a service

A `launchd` agent needs only the binary; everything else comes from the config home.

```xml
<plist version="1.0"><dict>
  <key>Label</key><string>dev.ikigai-rs.gonk</string>
  <key>ProgramArguments</key><array><string>/Users/you/.cargo/bin/ikigai-gonk</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/tmp/ikigai-gonk.log</string>
</dict></plist>
```

## Not built

- **No identity reaches a write.** A form filed while signed in is not attributed to the
  passkey: the capability carries the grant, but nothing carries WHO to the ledger's `author`.
- **No manifold-driven forms yet.** The forms are hand-written in the stylesheet. They post
  only actions and inputs the ledger declares, and `/act` refuses anything else, but a new
  ledger input does not appear on the page until the stylesheet names it.
- **No graph visualization**, no live updates (a change made elsewhere shows on the next
  load), and no page without JavaScript for the in-place forms.
- **No passkey management commands** beyond `invite`: list and revoke by editing
  `clients.json`.
- **No certificate lifecycle.** No CA, expiry, rotation or distribution; see the QUIC
  section for what admission means.
- **No live reload of trusted certificates.** Grants and enrolments are re-read per QUIC
  connection; the certificate set is read at startup.
- **One trace per door.** A traced call through a door records the forward, not the hub's
  resolution beneath it.
- **No browse graph of its own.** `ikigai-browse` hard-codes the default graph for every quad
  it writes, so annotations cannot be given `urn:iki:browse:graph:…` beside the ledgers' named
  graphs — and the default graph is exactly what `urn:cap:store:read:graph:<iri>` cannot name.
  The consequence is that a ledger↔browse join is a **root-capability** query: the socket
  door can run it, the HTTP door's anonymous caller cannot. Changing that needs a graph knob
  in `ikigai-browse`.
- **No archived explanation without a net grant.** `urn:repo:{root}:explain` is ONE action
  whether it derives or serves an entry the archive already holds, so the `urn:cap:net:*` it
  declares is required either way. `version=` provably derives nothing (`ikigai-browse`
  answers `NotFound` on a miss rather than falling back to a model), but there is no way to
  grant "may read what was already paid for" without also granting "may spend". That is the
  grain the HTTP browse face will have to answer, since an anonymous reader is exactly the
  caller who should see archived text and never derive.
- **No explanation archive to start from.** The archive the dev server holds is not migrated
  here — gonk starts with an empty browse dataset and pays for the first explanation of
  everything.
- **No rate limit on derivation.** The bounds on spending are the per-call `max_tokens`
  ceilings, the archive (once per content version, reused forever), and who holds a net
  grant. Nothing counts calls or spend over time; `ikigai-throttle`'s `RateLimit` overlay is
  the shape that would, and it is not linked.
- **No trace across the mount for a first call.** The tracer is forwarded to the peer only
  when it is already connected, because `set_tracer` is called on the resolve path and
  dialling from there would turn `trace` into a connect attempt.
- **Coarse browse invalidation.** One golden thread per ROOT: any change under a root
  recomputes every cached read of it. A per-file thread would have to be built from the file's
  own IRI, and the percent-encoding that produces it is private to `ikigai-browse`; a thread
  computed one way at the read and another way at the cut is worse than invalidating too much.
- **No browse face on the HTTP door.** The family is reachable through the socket and QUIC
  doors and by SPARQL; the browser face still serves ledgers only, and no route maps to
  `urn:repo:*`.

The page's htmx is htmx 2.0.4 (Zero-Clause BSD), vendored as `web/htmx.min.js`.

## License

MIT OR Apache-2.0.
