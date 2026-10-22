# ikigai-gonk

**A standalone work-ledger server for [ikigai](https://github.com/ikigai-rs).** One binary
holds the machine's durable RDF store and serves named ledgers — items, comments, labels,
links, claims, and a ranked `next` — through three doors: HTTP on loopback port 1062, with a
browser face for reading, filing, editing, closing, deleting and querying; an owner-only Unix
socket; and QUIC for other machines, where every connection is admitted by a client
certificate and runs under the grant that certificate is enrolled for.

```text
$ ikigai-gonk
ikigai-gonk 0.1.0 — holding the store at /Users/you/.ikigai/store
  http    http://localhost:1062/ — loopback (127.0.0.1:1062); anonymous read: default; 0 passkey(s)
  browse  not composed — no gonk.browse.root; urn:repo:* and the git/gh facades are not bound
  scripts urn:script:* in /Users/you/.ikigai/scripts — sparql and plan, read-only (the ledgers' reads); anonymous runs PUBLIC scripts and read-only plans, 30 a minute in all (draft checks too), each held to the anonymous budget; a QUIC client drafts, a person publishes; backed up with the store
  llm     not mounted — no gonk.mount; urn:llm:* resolves nowhere, so explain, review and the PR-derived layers are not bound
  socket  /Users/you/.ikigai/gonk.sock — owner only
  quic    off — no client certificate is enrolled (there is no /Users/you/.config/ikigai/gonk/clients.json); to open it, run `ikigai-gonk client add <name> --ledger <ledger>=write` and restart
  mount   mount = "prefer urn:iki:ledger:=/Users/you/.ikigai/gonk.sock"  (and the same for urn:iki:store:)

$ curl -X POST --data-binary 'Wire gonk into the cli' http://127.0.0.1:1062/iki/ledger/append
#1 urn:iki:ledger:default:item:01m2h3t8337vvs9j

$ curl http://127.0.0.1:1062/iki/ledger/items
   #1  open    p-  Wire gonk into the cli

1 item(s)
```

## Install and run

**Starting from nothing?** [`docs/fresh-machine.md`](docs/fresh-machine.md) is the runbook for
a Mac or Linux box with no ikigai on it: install, the config home, the ledger, browsing a
repository, and (optional) explain and review through a model peer, with the certificate
exchange spelled out.

From a clone:

```sh
cargo install --locked --path .
ikigai-gonk
```

That is the whole setup. The dataset lives in `~/.ikigai/store`, the socket at
`~/.ikigai/gonk.sock`, and HTTP on `127.0.0.1:1062`. No configuration file is needed, and no
certificate is needed until another machine has to reach it.

`ikigai-gonk --help` prints every flag and file.

**A second, scratch gonk** beside a live one is named by flags — no `HOME` or
`XDG_CONFIG_HOME` override (ledger #799):

```sh
ikigai-gonk client add laptop --ledger default=write --config-home ./cfg --data-home ./data
ikigai-gonk --port 1070 --config-home ./cfg --data-home ./data --socket ./g.sock --no-backup
```

`--config-home` moves `config.toml`, `store.toml` and `gonk/` (clients, grants, certificates);
`--data-home` moves the socket, the store, the backups and the review queue; `--store` names the
dataset's directory outright. `client`, `passkey invite`, `review request` and `grants
--browse` take the two home flags too, so the command that provisions a scratch server writes
where that server reads, and so does `checkout` (its managed directory defaults to
`<data home>/checkouts`). A relative home is made absolute from the working directory the
command started in, so every path the banner prints can be pasted from anywhere; `--socket` is
left as given, because a Unix socket path must stay under 104 bytes. `--port 1070` moves the
QUIC door to UDP 1070 as well. The browse family's `a11y.toml` layers are read and watched in
the config home the server was given, not the process's (ledger #918).

## In a browser

Open **<http://localhost:1062/>** — `localhost`, not `127.0.0.1`: a passkey is bound to a
name, and a browser refuses an IP address as one.

- **Browse.** The front page is the first ledger you may read, open items first, with filters
  for closed and all and a title search. Each item has its own page: body, filing metadata,
  labels, links, `about` targets and comments.
- **A listing renders fifty rows and says so.** The line above the list is the count the
  filter matched, not the count the page drew — "showing the 50 most recently updated of 411
  open items", with a link for the rest. `?limit=<n>` or `?limit=all` asks for more, up to
  500 in one render; a larger number is drawn at 500 and the line names the cap, and the
  Queue page follows the same rule (one `src/rows.rs`, ledger #480). ⚠ The bound is latency, and the number is measured: the server-side
  XSLT costs about 6 ms per row at fifty rows and about 16 ms at four hundred in ONE
  document, so a page of 410 items cost **6.5 seconds** — slow enough to read as a hung
  server rather than a slow page. Reading the ledger is not the expensive part (0.09 s for
  the whole set), which is why the page still counts everything and bounds only what it draws.
  `cargo run --release --example render-cost -- <items.ttl> [rows|all]` takes the numbers again.
  Since ledger #519 the rows are rendered in **chunks of ten** apart from the page's shell
  and spliced in (`src/render.rs`), which is the linear cost — 500 items 8.5 s → 1.9 s — and
  each chunk is a cached resource (`urn:iki:gonk:render`, keyed on the chunk's own bytes), so
  the same page again is 0.1 s. `examples/snapshot-cost.rs` times every expensive page over a
  backup archive, read-only.
- **File, edit, work.** A form on the ledger page files an item (first line the title, a blank
  line, then the body). An item page comments, edits the title, body and priority, closes with
  a reason or reopens, claims or releases, defers or resumes, labels and links.
- **Delete and purge are two acts.** *Delete* moves the item into the ledger's graveyard graph
  and leaves a tombstone; it is recoverable by hand. *Purge* destroys the content in both
  graphs and leaves only the tombstone. They are separate sections with separate
  confirmations, and each appears only for a caller whose grant holds it.
- **Query.** <http://localhost:1062/sparql> runs SELECT, ASK, CONSTRUCT and DESCRIBE over one
  ledger's graph and renders the answer as a table (or Turtle, or a boolean). Nine sample
  queries sit above the editor — one named ledger (with an explicit `GRAPH` clause, because
  each ledger is its own graph), open by priority, p1 only, a `COUNT` by `repo:` label,
  security, recently updated, items with comments, closed with their reasons, and a body-text
  search to edit. A sample fills the box and runs nothing: you press Run. A `ledger:` IRI
  reads as a CURIE (`ledger:open`, not `…/ledger#open`), with the prefix bound once above the
  table; the same URL answers a machine in the store's own formats, raw IRIs and all: `curl -H
  'Accept: application/sparql-results+json' 'http://127.0.0.1:1062/sparql?query=…'`.
  ⚠ There is deliberately **no "oldest" sample**: `dcterms:created` is when an item was
  *filed*, and a bulk migration files hundreds in one minute, so it does not say how long the
  work has waited. The page says so next to the buttons.
- **A result set is somewhere to go next, not a wall of text.** An item IRI or an item number
  in a result becomes a link to that item; a `repo:` label becomes a link that asks the ledger
  a *new* question — the open items with that label — so an item the first query never
  returned still appears. What becomes a control is decided by
  [a rule table that is itself a resource](#the-render-rules-are-a-resource), not by a list of
  blessed column names.

The face is hypermedia: server-rendered HTML with [htmx](https://htmx.org) for the in-place
updates, no single-page app and no build step. Every page is a **transform of a graph face** —
a ledger page is `urn:iki:ledger:{ledger}:items as=text/turtle`, re-serialized as RDF/XML and
rendered by one XSLT stylesheet whose templates match on `rdf:type`, server-side through
`ikigai-xslt`. It works at phone width, follows the system's light or dark scheme, and its
palette is held to the WCAG AA contrast floor in both by a test.

### Passkeys: who you are, and what that grants

An anonymous caller on this machine can READ the ledgers in `gonk.http.ledger`, and write
nothing: filing, commenting, labeling, claiming and closing all need an identity, a signed-in
passkey here or a client certificate at the QUIC door (ledger #1687). Until then the HTTP door
gave that caller the ledgers' write tokens too, so any process on this machine, and on every
machine a reverse tunnel puts the door on, could write an item into instructions for an
unattended worker. The passkey sign-in itself needs no ledger token and still runs
anonymously. Anything more than reading, such as writing, delete, purge,
another ledger, or browsing and explaining the repositories
([the `--browse` roles](#provisioning-a-browsing-identity-the---browse-roles)), belongs to an
identity:

```sh
ikigai-gonk passkey invite brian --ledger default=delete
```

```text
passkey invite for `brian` — grant `brian` (6 scopes) in /Users/you/.config/ikigai/gonk/grants.json
  valid for 30 minutes, once
  open  http://localhost:1062/#invite=…
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
   signs out, the session's 12 hours end, an hour passes with no request from it
   (`gonk.http.session_idle_minutes`, 1 to 720), or the server restarts.

The second click is deliberate. When the creation sheet closes it still holds the window's
focus, and a browser refuses a sign-in prompt from a page without focus, so gonk waits for
the click rather than chaining the two. If a sheet is canceled or times out, the page says so
and names the button to click again. A creation that did not finish leaves the invite unused,
good until it expires.

- **One grant table for both identity doors.** A passkey is an identity the way a client
  certificate is. Both map to a grant name in `gonk/clients.json` (certificates under
  `clients`, passkeys under `passkeys`), and the grant name maps to scopes in
  `gonk/grants.json`, the one file where scope lists live. Enrol a laptop certificate and a
  passkey under the same name and they hold the same authority, and editing the grant changes
  both on the next request or connection.
- **List and revoke** (ledger #313). `ikigai-gonk passkey list` prints every enrolled passkey:
  its label, grant, credential id and IRI. `ikigai-gonk passkey revoke <label>` (or
  `--credential <id>`, which a label two passkeys share requires) removes that one entry from
  `clients.json`, with no restart, and the passkey's live sessions are DELETED, not just
  refused (ledger #1824): enrolling the same credential again, or restoring `clients.json`
  from a backup, never brings one back, and only a new sign-in opens a session. It appends one line to
  `gonk/passkeys.log`, `<RFC 3339 ms> gonk:PasskeyRevoked urn:iki:gonk:passkey:<credential id>
  label=… grant=…`, after the change, so the record never claims a revocation that did not
  happen. A malformed `clients.json` is refused and left exactly as it is, and the file as it
  will be is checked against what both doors read before it replaces the old one (atomically),
  so the command cannot be the edit that leaves a door reading garbage. `grants.json` is left
  alone, as `client remove` leaves it: a grant name may be shared. The running server reads
  that line (the command runs in another process), which is how the sessions end.
- **Sign out everywhere** (ledger #1824). `ikigai-gonk passkey logout --all <label>` (or
  `--credential <id>`) ends every session of one passkey and leaves it enrolled: it appends a
  `gonk:PasskeySessionsEnded` line to `gonk/passkeys.log`, and the server ends the sessions at
  its next signed-in request or sign-in, with no restart. A signed-in page can do the same for
  its own passkey with `POST /auth/logout-all` and its session token as the body.
- **Sign-ins and sign-outs are logged** (ledger #1824), on stderr beside the access log and
  in its grammar, whatever `gonk.log.access` says: `gonk:Login` or `gonk:Logout`, the passkey
  resource, then `door`, `outcome`, `principal` (the passkey IRI, `-` for a refused sign-in),
  `credential`, `sessions` and, on a logout, `why` (`logout`, `logout-all`, `revoked`,
  `unenrolled`, `expired`, `idle`). A session token is never written.
- **A ledger page your grant does not reach is a `403`, in one of two sentences** (ledger
  #2021). Signed out: *Sign in to read the ledger "ventures".*, followed by where the sign-in is
  (the header of every gonk page). Signed in: *Your passkey's grant doesn't include the ledger
  "ventures".*, and no word about signing in, which would cure nothing. Either way the grant it
  needs comes last, on its own line, for the operator. The door decides which from the request's
  principal, which it carries on reads as well as writes.
- **A browser that navigates to an error gets it as a gonk page** (ledger #2021,
  `src/refusal.rs`): the usual header, navigation and styles around it, and the status
  unchanged. Signed out and refused something a sign-in could cure, the page says the signed-out
  sentence and carries **Sign in with passkey**, which takes focus; signing in reloads the page,
  so you land on the address you asked for. Signed in, it says what the grant lacks and the
  grant it needs, with no sign-in. Anything else (a `404`, a `429`, a `5xx`, a cross-site
  write) is the same page with the status as its heading and the message under it. curl, a
  script's `fetch` and htmx get what they got before: the sentence as text, or `ikigai-web`'s
  minimal page. ⚠ A browser sends the fetch metadata this keys on (`Sec-Fetch-Mode`) only to
  `localhost` or HTTPS, so over plain `http://` on a LAN address the page is the minimal one.
- **An identity is strictly stronger than anonymous.** A signed-in caller holds the anonymous
  grant plus its own, and `passkey invite` refuses a grant that adds nothing. The anonymous
  grant includes running and reading PUBLIC scripts; publishing one takes
  `--script <name>=publish` ([scripts](#scripts-saved-queries-and-plans-ledger-939)).
- **A signed-in write carries its author.** The door mints the session's principal — the
  passkey's stable IRI `urn:iki:gonk:passkey:<credential id>`, never the label — into the
  request's capability (`urn:cap:principal:<iri>`, ikigai-core 0.1.93, ledger #1077), on
  reads and writes alike, and the form adapter forwards it as the ledger's `author` wherever
  the action declares one (file, comment, close, reopen, delete, purge). The page renders that IRI as the
  passkey's *current* label from `clients.json`, or its credential id once the passkey is
  gone; the store holds only the IRI, so a relabel rewrites nothing. A form field named
  `author` is refused: the door names the author, a submitter may not. An anonymous caller
  is nobody and its writes stay unattributed, as before. The mechanical route
  (`POST /iki/ledger/append`) is unchanged — it carries the `principal` too, and the ledger,
  which declares `author` and not `principal`, ignores it.
- **The invite is the registration's trust anchor.** No attestation is parsed. The question
  that matters is whether this person was invited, and a single-use, expiring code answers it.
  The server stores only the code's SHA-256.
- **Verification** is `ikigai-passkey`'s: ES256, the exact origin `http://localhost:<port>`,
  RP ID `localhost`, a single-use challenge, user verification required, and a signature
  counter that must not go backwards.
- ⚠ **The session cookie is set by the page's script, so it is not `HttpOnly`** (ledger
  #1591). The HTTP transport this release builds on (ikigai-web 0.1.63) builds every response
  from a resource representation and cannot add `Set-Cookie`; the seam that can
  (`ikigai_web::with_headers`) is not in a published ikigai-web yet.
  What remains is `SameSite=Strict`, a same-origin Content-Security-Policy with no inline
  script, and a session table that lives in memory.

### What the HTTP door refuses a browser

A door that grants writes to whatever reaches loopback also grants them to any web page open
on the machine: a page on any site can `POST` a form to `http://127.0.0.1:1062/` without a
preflight. So the capability is computed per request, and two browser-only signals turn it
into a **refusal** before the request reaches anything it names — `403`, or `421 Misdirected
Request` for the second:

- a write whose `Origin` or `Sec-Fetch-Site` names another site (`same-site` included: a page
  on another port of `localhost` is another origin). A local process such as `curl` or a
  script sends neither header, so it is unaffected;
- a request whose `Host` is not `localhost`, `127.0.0.1`, `[::1]` or the bound address's own
  IP (with this port), reads included. That is the DNS-rebinding defense, and it answers
  `421`: the request names a server this one is not, which is a different fact from "not
  allowed" (ledger #2). The bound IP counts because a `gonk.bind` of another loopback address
  (`127.0.0.2:1070`) is admitted, and `localhost` does not reach it (ledger #1045). Only the
  bound one joins: a door on `127.0.0.1` still refuses `127.0.0.2`. The startup banner prints
  the URL that reaches the bind — `http://localhost:1062/` for `127.0.0.1`, the IP literal
  otherwise (`http://127.0.0.2:1070/`, `http://[::1]:1062/`) — and says when passkeys cannot
  work there, since WebAuthn refuses an IP address as a relying party.

★ **Refused, not "given nothing"** (ledger #864, R2; PENDING item 2). Until then both cases
computed an EMPTY capability, and an empty capability is still offered every action that
requires nothing: the pages, and the passkey ceremonies. Audit round 4 used that to fill the
passkey challenge table from another site with 256 form posts and lock every real sign-in
out for five minutes, renewable. Those requests are now REFUSED (`403`, or `421` for the
`Host`) at the edge
(`EdgeConfig::admit_fn`, `doors::http_admit`, ledger #879), before `ikigai-web` answers
anything — so a refused request no longer learns the declared verbs from `OPTIONS` or the
contract of the capability-free actions from `?description`, which the library answers
without dispatching. The capability carries the same refusal as a marker that the admission
overlay (`crate::admit`) answers with `Denied`, one decision in two places. A cross-site
`OPTIONS` counts as a write: a browser sends one only as the preflight of a cross-site write,
which would be refused.

★ **A GET from another page is not "just a read" either** (ledger #880). A site ignores the port,
so a page on `http://localhost:8090` is SAME-SITE with gonk: the browser sends gonk's
`SameSite=Strict` session cookie with every request it makes. Through `ed43af3` such a page
could GET `/k?c=source urn:repo:R:explain:P` (or `:review:`, or `/llm/coder/ask?prompt=…`)
from an `<img>` or a `fetch` and spend the signed-in identity's inference, archiving the
answer. The line is now ORIGIN, port included, read off `Sec-Fetch-Site` (`same-origin` and
`none` are gonk's own), `Origin`, and — only from a browser that sends no `Sec-Fetch-Site` —
`Referer`:

| who sent a GET | what it gets |
| --- | --- |
| gonk's own page (htmx: the Explain and Review buttons), an address a person typed, a client with no browser (`curl`, a script) | the caller's whole capability, as before |
| another page NAVIGATING here (a link from the CMS, GitHub or a chat; a `method=get` form; `location = …`) | the READ half of the caller's capability (`admit::FOREIGN_PAGE_KEEPS`): the ledger, store-read and browse-read tokens, and `urn:cap:annotate`; never `urn:cap:net:*`, `urn:cap:exec:*` or anything not on that list. A page renders, with its forms; a derivation answers `403` and says why |
| another page LOADING this (an image, a script, a `fetch`, a frame) | a `403` at the edge: nothing gonk serves is meant to be embedded in another page |

★ **Why authority, and not a list of routes or of resources.** A side effect is reached by more
than one path (the `/k` adapter, the mechanical `/repo/R/explain/P` mapping, a page's own
sub-request), and a resource added tomorrow is on no list. The capability travels with every
sub-request into the hub unchanged, so a GET that cannot carry net authority cannot derive by
ANY path, and the kernel's own `requires` checks enforce it. Every GET with a side effect found
on 2026-10-09 needs a dropped token: `urn:repo:{root}:explain[:{path}]`, `:review:{path}`,
`:judge:{path}`, `:judge-finding:{id}` (Source and Exists), `:pr:{n}:explain`, `:pr:{n}:review`,
`urn:iki:gonk:review:pass` and the `urn:llm:` mount itself (net: inference, and the archive and
findings writes that ride on it); `urn:repo:{status,log,branch}`, `urn:repo:pr:*` (and browse's
PR rows, which resolve them) and `urn:system:exec` (exec: programs, and `gh`'s calls to
GitHub). One write is left to a navigation on purpose: a browse READ re-anchors annotations
whose file drifted, a deterministic repair any reader triggers. `urn:iki:gonk:render` needs no
token at all, and is bounded on its own (see the hub resources below).

⚠ **The challenge table has no per-origin or per-IP bound, on purpose.** With cross-site posts
refused, every caller that can still mint a challenge is on THIS machine and arrives from
loopback, and the only origin a ceremony is accepted from is this server's own — so neither
key separates an attacker from the person signing in. A local process can still fill the
table (256 challenges, five minutes); a local process can also already write every ledger the
anonymous grant names, so it is inside the boundary this door draws.

**And a write may not name someone else as its author** (ledger #864, R4). An `author` shaped
like a principal this server names — `urn:iki:gonk:passkey:<id>`, which the item page renders
as that passkey's label — is refused unless it is the request's own principal, on every route
(the mechanical `POST /iki/ledger/append?author=…`, the form adapter, the QUIC door). A
plain-text author (`chris`, `roborev`) renders as text and is kept. The rule binds the
`author` ARGUMENT, and a raw store write passes none: through 1d02e53 a ledger writer could
`INSERT DATA { GRAPH <urn:iki:ledger:graph:default> { … ledger:author "urn:iki:gonk:passkey:…" } }`
through `urn:iki:store:graph-update` and the item page rendered that passkey's label. Since
ledger #878 the door refuses a raw store write to a ledger graph — see
[Raw store writes](#raw-store-writes).

**A write that names no author is attributed by the door** (R4 option (b), 2026-10-08). On the
HTTP door the form adapter fills `author` from a signed-in session's passkey; on the QUIC door
every write that names none is filled with the client's `urn:iki:host:gonk:client:<fingerprint>`,
wherever the target declares `author`. An author given explicitly is kept when it is plain
text or the request's own principal, and refused when it names another principal.

### SPARQL: the editor page, and the SPARQL 1.1 Protocol

`/sparql` is two faces of one resource, chosen by what the caller asks for.

**Asked for a results or RDF type** (`Accept: application/sparql-results+json`, or `as=`), it
is the **SPARQL 1.1 Protocol** over [`urn:sparql:*`](#sparql-as-resources-urnsparql), under the
request's own capability — the anonymous grant, or the signed-in passkey's. Naming no ledger,
the dataset is the union of every graph that capability may read: an anonymous caller's
ledgers, plus whatever a passkey's grant names (the browse graph, for `--browse read` or
`--browse-graph read`). `ledger=` narrows it to one ledger's graph, `graph=` to the graphs
named.

```sh
curl -s -G http://127.0.0.1:1062/sparql -H 'Accept: text/csv' \
  --data-urlencode 'query=SELECT ?g (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g'
curl -s http://127.0.0.1:1062/sparql -H 'Accept: application/sparql-results+json' \
  -H 'Content-Type: application/sparql-query' --data-binary 'ASK { ?s ?p ?o }'
```

- **GET** `?query=`, **POST** form-encoded (`query=`) and **POST** `application/sparql-query`
  all answer alike; any other POST body is a 400.
- ⚠ **A POST is a read, declared as a Sink.** `ikigai-web` maps every POST to `Sink` and has no
  seam for a host to route one to a Source, so the protocol's POST forms arrive at this
  resource's Sink action, which reads the query from the body and writes nothing. The manifold
  shows that action and says so. (The standalone `ikigai-web` answers its own `/sparql` POST
  outside that mapping.)
- `default-graph-uri` and `named-graph-uri` are **refused, not ignored**: the store's dataset is
  one set of graphs that is the default graph and the named graphs at once, so the two cannot
  be honored separately. Use `graph=`, or `GRAPH <iri>` in the query.

**Asked for HTML**, it is the editor page, and the rest of this section is about that.

`/sparql` never touches the store's whole-dataset doors. A query runs at
`urn:iki:store:graph-{select,ask,construct,describe}` with `graph=` set to the chosen
ledger's graph and **the caller's** capability. The store sets that graph as the query's entire
dataset before evaluation, so there is nothing else to see, and a grant naming one ledger
cannot read another. `FROM` and `FROM NAMED` are refused rather than silently overridden; the
page says so. It is read-only: an update is not a query form it runs.

⚠ The confinement to ONE graph is **this page's**, not the endpoint's. Since `ikigai-store`
0.2.5 a scoped read takes a whitespace-separated SET of graphs, so a caller holding a read
token for each can join across them — a ledger and the browse graph, say — by calling
`urn:iki:store:graph-select` directly; `FROM` stays refused over a set as much as over one
graph, and `GRAPH <g>` for a `g` outside the issued set matches nothing rather than erroring.
This page has no graph selector yet, so it shows that join as an example and does not offer to
run it (`web::CROSS_GRAPH`).

### How deep and how long a SPARQL text may be

Through `b16f79c`, one GET of `/sparql/results?query=SELECT * WHERE { FILTER(((…3000 parentheses…1…))) }`
**aborted gonk** (ledger #915): the SPARQL parser reads nesting by recursion, and a stack overflow
in Rust is not a panic, it ends the process. Every door reached it — the editor page and its
fragment, the protocol face, `/iki/store/graph-*`, `/sparql/select`, `/k`, the socket, QUIC, and
the review space's `match` — and so did a query with no nesting at all: `{} UNION {} UNION …`,
`1*1*…`, `1+1+…` or a path `a/a/…` is a left-deep tree the parser or the store walks by recursion,
and 6.5 KB of `+1`s was enough. gonk now holds two bounds and one size, all measured with
`cargo run --release --example sparql-depth` (which runs each probe in a child process):

- **Nesting: 64** (`sparql::MAX_NESTING`). `(`, `{`, `[`, `<<` and a negating `!`, mixed,
  outside strings and comments. `!` is the one prefix operator the parser reads by recursion, so
  `!!!…1` nests with no bracket (6,000 aborted a 2 MiB release parse); each one counts until the
  group holding it closes, and `!=` is not one. On a 2 MiB thread a release build aborted past 885 parentheses and a debug build
  past 99 nested blank-node lists; gonk's own queries, its sample queries and the gonk Book's nest
  under 10. The count is an UPPER bound by construction: the parser may read `<` as an IRI or as
  less-than, so the scan runs every reading at once and counts a closer only where every reading
  is in code — `FILTER(?a <(((…)))> 2)` cannot hide its parentheses inside an "IRI".
- **Length: 32 KiB for a CALLER's query** (`sparql::MAX_QUERY_BYTES`), because no bracket count
  sees a chain. Not applied to an update: the ledger and the browse family write long literals
  through `urn:iki:store:graph-update` themselves. ★ And not applied to gonk's OWN sub-requests
  (ledger #965): `ikigai-ledger` reads every open item's labels, links and comments in one
  `VALUES` at about 51 bytes an item, so past some 650 open items that query is over 32 KiB, and
  with the bound at every depth the home page, `/l/default` and `urn:iki:ledger:next` all
  answered `400`. A flat `VALUES` builds no deep tree, and the 64 MiB stacks are the margin.
- **Thread stacks: 64 MiB** (`stack::THREAD_STACK_BYTES`), for every thread that serves a request:
  the HTTP runtime's workers and blocking pool, the socket's thread per connection, and the QUIC
  runtime's workers. The last two are spawned inside `ikigai-ipc` and `ikigai-quic`, which take no
  size, so `main` sets the process's default for a thread spawned without one before any thread
  exists. The densest chain measured (a path, two bytes a step) costs a release build about 1.5 KB
  of stack a step, so a query at the length bound needs about 24 MiB.

A refusal is `400` at the HTTP door and `InvalidArgument` everywhere else, and comes before any
parse. Both bounds are enforced in the hub, around the store and the review space
(`sparql::bounded`), so they hold for every door: nesting at every depth, length at depth 0 — a
request a door issued, since every door reaches the hub at the hub's depth 0. An endpoint that
forwards a caller's text in a sub-request checks the length itself: the editor page refuses before
its own `spargebra` parse, and `urn:sparql:*` before it reads the caller's readable graphs.

**What it costs.** A stack is reserved address space, not memory: an idle 64 MiB thread costs what
an idle 2 MiB one does. A deep request leaves the pages it touched with its thread, so the worst
case is 64 MiB times the threads serving at once — one tokio worker per core, the blocking threads
in use, one thread per open socket connection — and a request inside both bounds touches well under
half of it.

⚠ **What this does not cover.** The 64 MiB arithmetic is a release build's; a debug build's frames
are several times larger and only the nesting bound protects it. A chain inside the length bound is
answered — but a PATH chain costs the store cubic time (2,000 steps took 168 s in a release
build), so a 4 KB query could hold a core for minutes, and operator chains are quadratic. Since
`ikigai-store` 0.2.8 that is bounded in TIME as well (next section). Any other recursion over
caller input that neither bound names is only made to need a longer input, not prevented. And since
0.2.7 the store parses and evaluates on a thread of its own (16 MiB plus 512 bytes per byte of
query), not on these 64 MiB stacks: a release build holds every chain up to the store's algebra
bound there, but a DEBUG build aborts past 405 terms of `1*1*…` (reported to the store).

★ **When `ikigai-store` releases its own pre-parse bound** (ledger #915), it supersedes this
scan for the store's doors. The scan stays: it is one linear pass at the edge, and it also covers
the two parses that are not the store's (the editor's, and the review space's `match`).

### How long a SPARQL query may run

`ikigai-store` 0.2.8 evaluates every query within a time budget, and answers a caller who runs past
it with a typed timeout (`503` here), never a partial result: **5 s** for a capability holding no
budget grant, up to **120 s** for one holding `urn:cap:store:budget:<ms>`, 120 s for root. A
request's own `budget=<ms>` can only lower that. The store also refuses a query of more than 32
joined patterns or 1,024 algebra nodes before planning it, which caps the planner's cubic cost.

**An anonymous caller gets 1 s** (ledger #964, #979): `gonk.http.anonymous_sparql_budget_ms`,
from 1 to 5000, default 1000. An anonymous loopback caller can read the configured ledgers, so it
can send a query that costs nothing to write and minutes to answer. A `VALUES` cross product
carries its own data and needs no graph, and over the reading room's twin of this door 1.3 KB of one
answered 272 MB after 11.2 s. So gonk stamps `budget=` onto the SPARQL an anonymous caller sends,
on every route it can send it by: `/sparql/select` and the other `urn:sparql:*` forms, the
store's `/iki/store/…` doors, the editor page's protocol face and its results fragment, and `/k`.
A caller's query that travels INSIDE another request (a script's run, a step of an ad hoc plan at
`urn:plan:eval`, the dry run of `urn:nl:sparql:check`) never passes that door, so those three are
held to the same budget on the task instead (`budget::within_run`, ledgers #1175 and #1210).

- **Where**: at the one place every door's request enters the hub (`doors::HubSpace`), by the
  endpoint it reaches (the store's ten SPARQL ids and the four `urn:sparql:*` forms), so the
  spelling of the IRI does not matter. The hub's own sub-requests never pass there, so **gonk's
  own queries are never capped by it**: when an anonymous caller opens a page, the ledger's queries
  run under the store's 5 s base. Measured on a RocksDB store restored from the live archive
  (361,607 quads, release build, `examples/door-budget-cost.rs`): the heaviest ledger read is the
  listing at 76–87 ms cold (`items`, and `items status=all limit=100000` at 82 ms), `next` 28 ms,
  and the SPARQL page's nine sample queries 2–8 ms each.
- **Who**: a request with no live passkey session. The door marks its capability
  (`urn:iki:gonk:door:anonymous-budget:<ms>`, a door marker no grant may name), and an HTTP caller
  cannot narrow the capability the door computes, so it can neither forge the mark nor drop it.
  A signed-in caller gets the store's base. The socket is the owner (root). QUIC callers are
  authenticated by certificate and get their grant's budget.
- **And inside an anonymous SCRIPT RUN** (ledger #1175): a run evaluates the script's query in a
  sub-request under the run's attenuated capability, which no longer carries the marker, so the
  run's wrapper (`crate::script`) carries the budget on the task and the store's overlay in the
  hub stamps every evaluation made within the run. See
  [Scripts](#scripts-saved-queries-and-plans-ledger-939).
- **The stamp OVERWRITES.** A caller's own `budget=` is kept only when it is an inline whole number
  no larger than the door's, so it can lower the budget and nothing else. A larger, zero, malformed
  or by-reference value is replaced. (`ikigai-sparql` reads a by-reference `budget=` as no budget
  and falls to its ceiling. `ikigai-store` refuses one. The stamp relies on neither.)

**How large an answer may be** (ledger #970, #993). Since `ikigai-store` 0.2.9 every answer is
bounded in size too: **100,000 rows and 16 MiB** of serialized answer for a capability holding no
`urn:cap:store:answer:<rows>` / `urn:cap:store:answer:bytes:<bytes>` grant, up to 10,000,000 rows
and 1 GiB for one that does, the ceiling for root. Past either bound the query is refused
(`InvalidArgument` on `query`, "refused, not truncated"), never cut short, and `max_rows=` /
`max_bytes=` can only lower it. **An anonymous caller gets the base**, and gonk stamps nothing
lower: the largest answer it can legitimately ask for is the whole ledger graph, measured
2026-10-10 on the 2026-10-09 archive at 13,375 rows and 4,986,073 bytes of SPARQL JSON, inside the
base 7.5 times over on rows and 3.4 on bytes. A signed-in caller gets the base too, unless its
grant in `gonk/grants.json` carries the answer grants, which `grants.json` accepts (they raise a
size toward the ceiling and grant nothing to read). `tests/door_budget_964.rs` pins the refusal on
every route.

⚠ **What it does not bound.** The budget answers the CALLER at 1 s. An evaluation that oxigraph
cannot cancel partway (an aggregate, `ORDER BY`, a join's build side) keeps a core until it
finishes, and the store refuses ALL SPARQL while a quarter of the cores are held that way. That
cap is per store, not per caller, so an anonymous caller that keeps it full also refuses gonk's own
ledger reads and the backup until its runaways end. That is the store's design, and is reported
there.

### Routes

| path | resource | |
| --- | --- | --- |
| `/` | `urn:iki:gonk:page:home` | the first readable ledger |
| `/l` | `urn:iki:gonk:page:ledgers` | every ledger this grant may read, with its open and total counts: the header menu's "All ledgers" |
| `/l/{ledger}` · `/l/{ledger}/items` | `urn:iki:gonk:page:ledger:{ledger}` · `…:fragment:items:{ledger}` | a ledger, page and fragment |
| `/l/{ledger}/item/{id}` · `…/card` | `urn:iki:gonk:page:item:{ledger}:{id}` · `…:fragment:item:…` | one item |
| `/l/{ledger}/doctor` | `urn:iki:gonk:page:doctor:{ledger}` | the ledger's doctor: every problem and its remedy, read-only |
| `POST /act` | `urn:iki:gonk:act` | a form, as one ledger action |
| `/sparql` · `/sparql/results` | `urn:iki:gonk:sparql` · `…:fragment:sparql` | the editor page and its results; with a results `Accept`, the SPARQL 1.1 Protocol (GET, and POST as a read) over `urn:sparql:*` |
| `POST /auth/{op}` | `urn:iki:gonk:passkey:{op}` | passkey ceremonies and sessions |
| `/render-rules` | `urn:iki:gonk:render-rules` | which result cells become controls, as Turtle |
| `/view/{name}` | `urn:iki:gonk:view:{name}` | a named view: whatever its target answers, under your own grant (see "Named views") |
| `/views` · `POST /views/save` | `urn:iki:gonk:page:views` · `urn:iki:gonk:views:save` | the Views page: the named views in force and the views space's status, and with the declaration's write token a form to add, change or remove each (see "The Views page") |
| `/static/{name}` | `urn:iki:gonk:asset:{name}` | `gonk.css`, `gonk.js`, `htmx.min.js` |
| `/browse` | `urn:iki:gonk:page:browse` | the repositories this grant may read |
| `/browse/{iri}` | `urn:iki:gonk:page:browse:{iri}` | the page a browse face renders inside |
| `/k?c={command}` | `urn:iki:gonk:k` | the adapter those faces call — one read, one annotation, or one finding decision (stamped `made=single`) |
| `/queue` · `/queue/rows` | `urn:iki:gonk:page:queue` · `…:fragment:queue` | the review queue, and the section it swaps |
| `POST /queue/decide` | `urn:iki:gonk:queue:decide` | one human decision on one finding |
| `POST /queue/batch` | `urn:iki:gonk:queue:batch` | one batch decline, one decision per ticked finding |
| `/queue/depth` | `urn:iki:gonk:fragment:queue-depth` | the header's live badge, polled every 10s |
| `/layers` · `/layers/{layer}` | `urn:iki:gonk:page:layers` · `…:page:layer:{layer}` | the Layers page: every layer the configured identity reads, and one layer (see "The Layers page") |
| `/layers/~/wait` | `urn:iki:gonk:fragment:layers-wait` | what an open Layers page asks every 3 s, answered at once from gonk's copy |
| `POST /layers/{layer}/say` | `urn:iki:gonk:layers:say:{layer}` | a message or a question, said at the layer door as the configured identity |
| `/layers/{layer}/file/{id}` | `urn:iki:gonk:layers:file:{layer}:{id}` | a file entry's bytes, after its size and digest are checked |
| `/toolbox` · `/toolbox/{…}` | `urn:iki:gonk:page:toolbox` · `urn:toolbox:{…}` | the RDF Toolbox, only when `gonk.toolbox.dist` is set (see "The RDF Toolbox") |

These exist **only on the HTTP door**. The socket and QUIC doors serve exactly the store and
the ledger, as before, plus gonk's own hub resources: the four `urn:sparql:*` forms, and
`urn:iki:gonk:render`, the page
renderer's chunk transform (one `<view:page view="chunk">` document in, its HTML out; a pure
function of its input, cached by the hub on the content-addressed request, which is what
makes a poll of the Queue a cache hit and a decision a one-chunk miss). It is bound in the
hub because the hub holds the process's one cache. `tests/conformance.rs` pins both catalogs.
⚠ It requires nothing, so every caller reaches it — the `/k` adapter forwards a `content=` from
a GET — and its renderer recurses once per level of nesting: through `77ac767` a ~1 KB document
nested a few hundred levels deep overflowed the worker's stack and **aborted gonk** (ledger
#880, S2). It now refuses, before any parser sees it, a document nested more than 16 levels
(`render::MAX_CHUNK_DEPTH`) or carrying any `<!` (a declaration can nest what a tag count
cannot see); gonk's own chunks nest a handful of levels and carry neither.

**The header picks a ledger from a menu of links** (ledger #1888). A button names the current
ledger ("Ledger: default") and opens a panel with one link per ledger the caller may read,
`default` first and then by name, and "All ledgers" last, which goes to `/l`. Without a script it is a `<details>`, and it works the same. Past eight ledgers the
panel has a filter box that narrows the links as you type and says how many match. A caller who
may read one ledger sees its name and no menu. It is links, not a `<select>` that navigates on
change, which fails WCAG 3.2.2. The menu and `/l` are computed from the same set, so nobody is
offered a ledger the grant does not read. The open and total counts are on `/l` only: in the
menu they were one `count` read per ledger on every page, which took a 41 ms page to 57 ms at
12 ledgers; without them it is 47 ms, the stylesheet drawing the menu.

**Every form issues an action the ledger already declares.** `/act` builds the target from
`ikigai-ledger`'s own naming (`urn:iki:ledger:{ledger}:{action}`, or `…:item:{id}`), refuses a
verb the target does not describe, and refuses any field that the verb's contract does not
name. It runs under the caller's capability, so the ledger's checks decide.

### The RDF Toolbox at `/toolbox` (ledger #1933)

The [RDF Toolbox](https://github.com/ikigai-rs/ikigai-toolbox) is turtle-lsp's Validate, Format,
SHACL and SPARQL page, by Abhishek Kundu (MIT), running in the browser tab. gonk serves it at
`/toolbox` when a build is installed and named; with no `gonk.toolbox.dist` line there is no
`/toolbox`, and nothing else changes.

```text
/toolbox                 urn:iki:gonk:page:toolbox   the toolbox on the default ledger's items, or empty
/toolbox/page            urn:toolbox:page            the empty toolbox (turtle-lsp's samples)
/toolbox/{name}          urn:toolbox:{name}          a built file (toolbox.html, app.js, engine.wasm, …)
/toolbox/check/{iri}     urn:toolbox:check:{iri}     the toolbox checking one resource (shapes=, theme=)
/toolbox/input/{iri}     urn:toolbox:input:{iri}     a resource's Turtle face, for the page to read
```

**`/toolbox` opens on our data** (ledger #2033): when you may read the default ledger, it is the
check page over `urn:iki:ledger:items`, the default ledger's items, so the page's input is
`/toolbox/input/urn%3Aiki%3Aledger%3Aitems` and the ledger's Turtle is in the editor when the page
loads. When you may not (a host whose anonymous caller reads no ledger, say), it is the empty
toolbox with turtle-lsp's samples, never an error page. The decision is your capability's, made on
the server: the same read gate as the ledger's own pages. `theme=` passes through. The samples are
always at `/toolbox/page`.

Any other resource opens the same way by name: `/toolbox/check/urn%3Aiki%3Agonk%3Arender-rules`
opens the toolbox on the render rules' Turtle, read under YOUR capability. The toolbox shows a viewer
only what that viewer could read anyway, and a resource you cannot read is a `403`. Nothing is
uploaded; the checking runs in the page.

**Installing the build.** The build is not in the crate (the engine is 8 MB of WebAssembly). Build
it from [ikigai-toolbox](https://github.com/ikigai-rs/ikigai-toolbox) (it needs rustup, git and
node) and copy `dist/` to the configured directory:

```sh
git clone --recurse-submodules https://github.com/ikigai-rs/ikigai-toolbox.git
cd ikigai-toolbox
./build.sh
mkdir -p ~/.local/share/ikigai-toolbox
rm -rf ~/.local/share/ikigai-toolbox/dist
cp -R dist ~/.local/share/ikigai-toolbox/dist
```

```toml
gonk.toolbox.dist = "~/.local/share/ikigai-toolbox/dist"
```

Then restart gonk; the banner's `toolbox` line names the directory. A line naming something that is
not a directory stops gonk at start, naming the key. Files are read live on every request, so a
new build copied over the old one is served at once, with no restart.

**What it adds to the HTTP door, and only there:**

- the `urn:toolbox:*` family and the build directory, bound with ikigai-fs at
  `urn:iki:toolbox:dist:{path}` (a jail at the directory), and `urn:iki:gonk:page:toolbox`, what
  `/toolbox` answers (`toolbox::Opening`). The socket and QUIC doors serve exactly
  what they served before.
- a read on exactly the toolbox's files for the anonymous loopback caller, one
  `urn:cap:fs:read:<dist>/<name>` for each name in `ikigai_toolbox::FILES`
  (`grants::toolbox_read_grants`), because the toolbox reads its build under its caller's
  capability. A signed-in passkey holds it too, since a passkey holds the anonymous grant plus its
  own. Nothing grants an `fs` write or delete. Any other file left in the directory is refused,
  by `/toolbox/…` and by the directory's own name (`/iki/toolbox/dist/…`) alike.
- the toolbox's own Content-Security-Policy (`ikigai_toolbox::TOOLBOX_CSP`) on the `/toolbox`
  rows and no others: it adds `'wasm-unsafe-eval'`, which lets the page's worker compile the
  engine. Every other page keeps the door's default policy.
- a **Toolbox** entry in every page's header, after SPARQL (ledger #2021): an ordinary link to
  `/toolbox`. With no `gonk.toolbox.dist` line there is no entry, so no page offers a door that
  answers `404`.

⚠ A link to `/toolbox` from another site arrives as a foreign navigation, which keeps only the
READ half of a caller's authority, and the `fs` read is not in that half
(`admit::FOREIGN_PAGE_KEEPS`), so the page is refused. Open it from gonk's own pages, or type
the address.

### Links in prose (ledger #989)

An item's body, a comment's body and a finding's body are plain text. Their `http://` and
`https://` URLs become links (the `href` is exactly the URL, and a trailing `.`, `,`, `;`, `:`,
`!`, `?` or unbalanced `)` stays with the sentence), and so does `ledger #N`, linked to
`/l/<ledger>/item/N`. Nothing else changes: no markdown, no HTML, and a `javascript:` URL stays
text. This is done after the transform (`render::linkify`), on elements the stylesheet marks
`data-linkify`. xrust supports neither `xsl:analyze-string` nor the `xsl:variable` and
`string-length()` that a recursive template would need to trim a URL. xrust's text escaping is
what makes the pass safe: a `<` in a body arrives as `&lt;`, so a URL can neither open a tag nor
leave its attribute.

### Claims, lifecycle state and the doctor (ledger #775)

`ikigai-ledger` 0.5.0 gives every item a **lifecycle state** (one value, moved by a
compare-and-set; no state is `filed`), **leased claims** (`lease=30m`; an expired lease is not
freed, it is taken over), and a read-only **doctor** of five stored checks. The item page shows the
state and the claim: its holder, its kind, and its lease and expiry. The claim form takes an
optional lease. `/l/{ledger}/doctor` lists what the doctor found (orphaned, abandoned,
lease-expired, unknown-state, two-states), with each remedy as text. It repairs nothing.

**A claim's kind is this server's to stamp, never the caller's** (`admit::claim_kind`). A claim
whose request carries a passkey principal is a `person`'s. A QUIC client's, an agent's, an
anonymous caller's and the owner socket's are a `machine`'s (Brian, 2026-10-09). The ledger
reports a machine claim on an item that is not in flight as orphaned, and leaves a person's hold
alone. The stamp reads the request's `principal`, which is sound only because every door
OVERWRITES that argument:

- `ikigai-web` stamps HTTP writes from the session cookie and drops a caller's `?principal=`. The
  `/act` form adapter forwards the door's principal to the action it issues.
- `ikigai-quic` stamps the connection's client IRI after removing a client's own.
- The socket stamps nothing, so its door now REMOVES any `principal` a caller sends
  (`doors::HubSpace::naming_nobody`). Before this, an owner process could have posed a claim as a
  person's.

`tests/claim_kind_775.rs` sends a forged passkey principal and `kind=person` through each door of
a real server and reads back what was stored.

### Browsing a repository at this port

```sh
open http://127.0.0.1:1062/browse                             # the roots this grant may read
open http://127.0.0.1:1062/browse/urn:repo:ikigai-core:tree   # straight into one
```

**The header carries a `Browse` link** whenever the caller may read at least one configured
root, next to the ledgers and `SPARQL`, and it lands on `/browse` — the list. One link rather
than one per root: seven repository names in a header stop being a header, and there is no
natural first root for a single link to point at. Both the link and the list are built from
the roots the caller may READ (`ikigai-browse`'s own two checks: a grant naming the root, or
the all-roots wildcard), so a caller is never offered a door that answers it a 403, and a
caller who may read nothing sees no link at all.

The page is a gonk page — same header, same sign-in control — with one region that loads
`ikigai-browse`'s own HTML face into it. **Everything after that first paint is the face's
markup and the face's affordances**: crumbs, directory entries, file views, the annotate
form, Explain and Explain With, and whatever a later `ikigai-browse` adds. gonk renders none
of it and knows about none of it; it supplies the door and the identity.

Those affordances are written `hx-get="/k/source {iri} [k=v …]"` and
`hx-post="/k/sink urn:iki:annotation"` — a command, in the path. ⚠ **This door cannot parse
that back into a resource**: it is the `ikigai-web` library, which percent-decodes the path
and then rebuilds a target IRI from it, and a command has spaces in it, which no IRI may. So
the command travels as one query value here —

```sh
curl 'http://127.0.0.1:1062/k?c=source%20urn:repo:ikigai-core:file:README.md%20as=text/html'
```

— and `web/gonk.js` folds the path spelling into that query form in the browser, on
`htmx:configRequest`, in one line that knows about commands and nothing about buttons. (The
standalone `ikigai-web` server parses the raw request-target itself, which is why the same
affordances work there untouched.)

★ **A line selected by its number fills the annotate quote** (ledger
[#658](http://localhost:1060/l/default/item/658)). A file view gives every line `id="L{n}"` and a
gutter self-link `href="#L{n}"`; clicking the number selects the line (`#L42` in the URL), and
`web/gonk.js` puts that line's text into the view's `form.browse-annotate input[name=exact]`, on the
gutter click, on `hashchange` and when a deep-linked view arrives. A quote the person has TYPED is
never overwritten. It is a hook on browse's own selection, not a new one: with scripting off the
`#L{n}` deep link and the gutter links work exactly as before, and the quote is typed by hand.

**What a browser may do here is entirely its grant.** `/k` runs every request under the
per-request capability above — so a cross-site `POST` mints nothing, a rebound `Host` reads
nothing, another page's link reads and never derives (ledger #880), and the anonymous loopback caller, which holds ledger tokens only, cannot read a
repository at all, let alone spend inference on explaining one. A signed-in identity can do
exactly what its grant names, and browsing needs a role beyond a ledger's grant.

### "On GitHub" (ledger #1907)

A browse page that shows a root, a directory or a file carries an **On GitHub** link above the
region, to the same path on GitHub: `tree/{ref}/{path}` for a directory, `blob/{ref}/{path}` for a
file. On a file view a selected line (`#L42`, a typed `#L10-L20`, or the line an annotation card's
`#annotation-{id}` names) carries onto the link. It opens like every other link on these pages, in
the same tab, and it follows the region: when a click moves the region to another directory or
file, `web/gonk.js` asks for that view's link and swaps it in, and a view that is neither (an
explanation, the pull-request listing) shows none.

A pull request's page (`urn:repo:{root}:pr:{n}`, its explanation and its review) carries the same
link, to the pull request on GitHub: `pull/{n}` in the root's repository (ledger #2117). It names
no ref, so it has no note. The number is the one browse shows, which `gh` resolved; a clone whose
`gh` base repository is not its origin (a fork with an `upstream` remote) names the base with a
`gonk.browse.github` line.

**The ref.** The HEAD commit, when a `refs/remotes/origin/…` ref contains it: a permalink, so
GitHub shows exactly what the checkout holds. Otherwise the local tree is ahead, and the link
names origin's default branch (`origin/HEAD`, else GitHub's own `HEAD`) with a note saying so.
"Origin has it" is as of the last fetch or push: gonk runs no network `git`.

**The repository** is the root's `remote.origin.url` in any form GitHub hands out
(`git@github.com:owner/repo.git`, `ssh://git@github.com/owner/repo`,
`https://github.com/owner/repo(.git)`, with or without userinfo), normalized to
`https://github.com/owner/repo`; userinfo never reaches a page. A root whose origin is elsewhere,
or that has none, names its repository in the config home:

```toml
# repeatable; <root>=<owner/repo, or any URL form above>
gonk.browse.github = "core=ikigai-rs/ikigai-core"
```

A line naming a root this server does not serve, a repository that is not on GitHub, or a root
twice stops the server at start. With no GitHub origin and no line, there is no link.

The same facts are resources, under the root's browse grant like every browse row:

| name | what it is |
|---|---|
| `urn:iki:gonk:github:{root}` | the repository, the ref, whether it is a permalink, and the root's place in its repository, one `key value` per line (or `none {reason}`); read after `urn:repo:{root}:state`, so it is cached until the root's HEAD, refs or config change |
| `urn:iki:gonk:github:{root}:tree[:{path}]`, `…:file:{path}` | one view's page on GitHub (`href`, `ref`, `permalink`, `kind`, `note`), the path in browse's own spelling; `as=text/html` is the link the page shows |
| `urn:iki:gonk:github:{root}:pull:{n}`, `…:pull:{n}:file:{path}` | a pull request on GitHub (`pull/{n}`), or one file in its Files tab (`pull/{n}/files#diff-{sha256 of the path}`, the anchor GitHub gives each file there); `href`, `kind`, `number`; `as=text/html` is the link |

```sh
# a mount prefix is replaced by `urn:`, so an alias carries the whole name across
ikigai --mount "urn:gk:=$HOME/.ikigai/gonk.sock" \
  -c 'source urn:gk:iki:gonk:github:ikigai-gonk:file:src/github.rs'
```

### A root's git history as weekly series (ledger #1631)

The delivery dashboard's git source (ledger [#1630](http://localhost:1062/l/default/item/1630)):
commits per week, and test functions added and removed per week, per language, for every
browse root.

```sh
ikigai --mount "urn:iki:gonk:=$HOME/.ikigai/gonk.sock" \
  -c 'source urn:iki:gonk:history:ikigai-cli:weekly as=text/csv'
```

| name | what it is |
|---|---|
| `urn:iki:gonk:history:{root}:weekly` | the series at the root's HEAD now: reads `urn:repo:{root}:state` for the HEAD and answers the name below with the same arguments |
| `urn:iki:gonk:history:{root}:weekly:at:{sha}` | the series as at one commit (a full id): a constant, so cached as long as the kernel keeps it |

Arguments: `weeks` (1 to 520, default 52; the window ends with the week holding the commit's
own committer date) and `as` (`text/plain`, the default, with the method stated under the
table; `text/csv`, one row per week, every week present; `text/turtle`, an RDF Data Cube
dataset with one observation per week and one measure property per column, named
`urn:iki:gonk:measure:{column}`).

**The method** is in each resource's description and on its text face: commits are non-merge
commits by committer week (weeks begin Monday 00:00 UTC); a test added or removed is an added
or removed diff line that begins `#[test]` or `#[tokio::test` in a `.rs` file, `def test_` or
`async def test_` in a `.py` file, or `Deno.test(` (`.only(`, `.ignore(`) in a TypeScript or
JavaScript file.

**Cacheable per HEAD, without a thread of its own.** A commit id names an immutable history,
so the series as at a commit never changes, and a new commit is a NEW NAME rather than a cut.
The HEAD-relative name hangs from `state`, which the root watch cuts on every change to the
root's refs (and on every working-tree edit, since that thread is coarse): after an edit that
moves no HEAD only the `git status` probe reruns. Measured over ikigai-cli's history on plasma
(520 commits in 52 weeks): about 0.3 s cold, about 10 µs warm, about 25 ms after a cut
(`cargo run --release --example history-cost -- ikigai-cli=<path>`).

**Bounded, and the bound refuses.** Both `git log` passes share a 30 s budget; past it the
child is killed and the read is refused as a timeout, never answered with a partial count.

**Authority is browse's.** Reading a root's history is reading the root: both names declare
`urn:cap:browse:read:*` and enforce `urn:cap:browse:read:{root}`, so the anonymous loopback
caller reads none of it, as it reads no browse row.

### Provisioning a browsing identity: the `--browse` roles

`passkey invite` and `client add` take `--browse <read|derive>`: the repositories as a ROLE,
the way `--ledger` and `--browse-graph` are roles, so an operator names what an identity may
DO and the tool writes the tokens. Each role holds the one before it:

```text
--browse read     urn:cap:browse:read:*     read the repositories: files, trees, git state,
                                            annotations, the archive listing. --root <root>
                                            (repeatable) names roots instead of every root
                  urn:cap:store:read:graph:urn:iki:browse:graph:default
                                            the browse graph's quads, through
                                            urn:iki:store:graph-* (no file contents)
--browse derive   urn:cap:annotate          mint annotations, publish a finding. DELETING an
                                            annotation also needs the read on ITS root
                                            (browse 0.17.0), which the role carries
                  urn:cap:net:<mount host>  EXPLAIN and REVIEW: reach the mounted model,
                                            which spends its inference
```

```sh
ikigai-gonk grants --browse derive            # print what it would write, on THIS server
ikigai-gonk passkey invite brian --ledger default=delete --browse derive
ikigai-gonk client add box --browse read --root ikigai-core
```

- **`derive` is the dangerous one, and it is one word on purpose.** Spending inference used to
  be three tokens an operator assembled by hand; now it is a flag `--help` names and a grant
  visibly carries.
- **The net scope is read from this server's own `gonk.mount`**, never typed: the host of a
  `quic://` target (`urn:cap:net:127.0.0.1` for `quic://127.0.0.1:4433`, the peer's name for a
  remote one), `localhost` for a socket. With no mount, `derive` is refused: explain and review
  derive through the mounted peer, so there is nothing to grant. A `--root` that is not a
  configured `gonk.browse.root` is refused too, since a token naming it would match nothing.
  So these commands read `config.toml` (or `--config`) whenever `--browse` is given.
- **`derive` always carries `urn:cap:annotate`, and that keeps the review trigger safe.** A
  headless reviewer needs the net grant WITHOUT the publish token, and arming refuses any
  grant that can publish (below). Since no role mints one without the other, nothing these
  commands write can arm the trigger.
- **Not in either role:** the browse graph's WRITE door (raw quads, `--browse-graph write`;
  `ikigai-browse` writes its own archive and annotations without it), and `urn:cap:exec:gh`,
  which the pull-request layers need. A grant that should reach those still names them in
  `grants.json` by hand, and the file is re-read on every request and checked fail-closed
  either way: the whole-dataset store tokens and the `urn:cap:net:*` / `urn:cap:exec:*`
  offering wildcards are refused however they got there.

**Rewriting a grant says what it moves.** A grant name is shared by every certificate and
passkey enrolled under it, so writing different scopes under an existing name is refused
unless `--force` — and the refusal names every scope the rewrite would remove and add. With
`--force` the same list is printed after the write. Re-enrolling a hand-widened grant used to
narrow it silently, which turned a feature off with no signal (ledger
[#435](http://localhost:1060/l/default/item/435)); now it reads:

```text
ikigai-gonk: grant `brian` already exists in …/gonk/grants.json with different scopes — nothing was written. Replacing it would NARROW it and widen it:
    removes  urn:cap:net:localhost
    removes  urn:cap:exec:gh
    adds     urn:cap:net:127.0.0.1
Use --force to replace it, which changes every identity enrolled under it
```

A reordering of the same scopes is no change, and is written without `--force`.

⚠ **A click on Explain spends inference.** The per-face token ceilings
(`gonk.explain.*.max_tokens`) bound ONE call; what bounds them over time is one window on the
mount, `gonk.mount.calls_per_hour` (2,000 by default; [explaining](#explaining-what-it-browses)
says how it counts). A directory-grain explanation fans out
per child inside `ikigai-browse` — each child's own explanation, recursively, with unchanged
children as archive hits — so one click on a large directory is many model calls, and the
door cannot see that from outside. `/k` issues exactly one kernel request per HTTP request
and prefetches nothing; the rest is the resource's economics, not the door's.

### The review Queue: a human publishes, or nothing does

```sh
open http://127.0.0.1:1062/queue                      # what is pending, in triage order
open 'http://127.0.0.1:1062/queue?state=published'    # what was published, and who rated it what
open 'http://127.0.0.1:1062/queue?group=file'         # decide in batches — a kind the contract declares
open 'http://127.0.0.1:1062/queue?summary=unconfirmed' # the walk: declines nobody evidently meant
```

Since `ikigai-browse` 0.5.0 a review pass no longer mints annotations. It produces **pending
findings** at `urn:iki:finding:{id}`, and `Sink urn:iki:finding:{id} decision=publish` — gated
by `urn:cap:annotate` — is the only path into the `urn:iki:annotation:` family. Brian's rule,
2026-09-19: *"Nothing gets published to Gonk except by the human."* This page is the face for
that act, and the interlock it creates is the reason the git-event trigger above can land
complete and deliberately unable to publish anything.

⚠ **A queue is not a gate.** Nothing in it blocks a commit, a push or a merge. The word reads
like a gate to anyone who has used one, so the page says so in as many words.

★ **The page asks a human only about the SERIOUS findings** (ledger
[#496](http://localhost:1060/l/default/item/496)). Brian, 2026-09-21: *"the preference is to
highlight issues that need addressing, so narrowing the squishy stuff is the priority"* — and,
the same day, on praise: *"positive signal is still signal and tells us something about the
code."* Both hold, about different places: **minting** keeps every severity, **triage** asks
about `gonk.queue.serious` (default `critical,major`) plus any unrated finding. Everything else
is still minted, anchored and counted — the page says how many it left out and which words they
carry, `?severity=all` lists them, and the header badge shows both numbers (`134 +185`: waiting
for a decision, and minted-not-queued). A decided row is listed whatever its word: it asks
nothing. The words are validated at start against the finding contract's own `severity` set,
so a `gonk.queue.serious` the contract does not declare stops the server naming both lists.
On 2026-09-21 the default hid 185 of 319 pending rows.

★ **The page ORDERS by the judge's verdict, and hides nothing** (ledger
[#696](http://localhost:1060/l/default/item/696), Brian 2026-10-02). `ikigai-browse` 0.16.0 runs a
JUDGE on each serious finding a pass mints — a second call with the context the reviewer lacked,
four narrow answers, a verdict by rule — and attaches the verdict to the finding row. The Queue
draws confirmed findings first, then the ones no judge has looked at, then the unsure, then the
ones a judge could not judge, then the refuted, **folded last** with the judge's four answers and
reasons inside and the decision form intact. The sort is stable, so within one standing the
findings resource's own triage order is kept; the same order holds inside every batch group,
where a folded member keeps its box outside the fold. Every row says where it stands in words with
the judge's tag (`judge: … · judge-v2@<model>`), the list says how many stand where, and the
header badge's numbers do not change: a refuted finding is still waiting for a decision. The
judge is `gonk.review.judge` (default `urn:llm:coder:ask`, browse's own; `"off"` for none), and
its per-call ceiling `gonk.review.judge_max_tokens` (default 400, browse's own; a reasoning judge
such as gpt-oss answers empty at 400 and wants thousands). Its
verdict words are the findings contract's own `verdict` set (browse 0.16.1): gonk reads them from
it and spells none, ordering by POSITION in that set (the first leads, the second is folded and
hidden, any other is uncertain), a mapping a test pins by running browse's own judge rule. A
browse that declares no set stops the server at start, naming the floor. A row is routed by its
LATEST verdict and labeled with that judge's tag, so after a judge or prompt-version change
(judge-v2, ledger #483) a row judged only by the earlier version keeps its verdict, says whose it
is, and is re-judged by the next backfill run; while more than one tag is in play the order line
counts them.

★ **The queue that predates the judge is BACKFILLED on an operator's word**, never on start:

```text
urn:iki:gonk:judge:backfill   Source  where the run stands (as=application/json for the numbers)
                              Sink    content=start | content=stop
```

`ikigai -c 'sink urn:iki:gonk:judge:backfill content=start'` over the owner-only socket walks
every serious pending finding that exists when it starts, one at a time, through browse's
`urn:repo:{repo}:judge-finding:{id}` with the configured judge. Exists comes first, so a
finding already judged under that judge's tag costs no model call. A stop takes effect after the
call in flight, and the next start resumes past everything judged. The run waits while a review
pass is in flight, however it was started (the queue's passes, the page's Review button, a person
on the socket: the browse family counts every review Source while it runs), or while the armed
review queue has requests waiting. A finding whose reviewed
version cannot be recovered is counted with browse's reason, shown on its Queue row as "could not
judge", and asked again only on the next run. The run uses exactly the browse read and
`urn:cap:net:<the mounted peer's host>`; starting it needs a browse read and a net grant, like
any derivation. With `gonk.review.judge = "off"` it refuses to start. The Queue page shows where
the run stands once it has run.

★ **"Real, reproduced" is recorded on a publish** (ledger #696). The decide form carries a
"reproduced" box beside Publish (its value is the finding contract's own `reproduced` word; the
note says how), and a published finding not yet marked offers a folded "record a reproduction"
form: `decision=publish reproduced=… revises=<the publish>`, a revision that keeps the outcome and
the rating, stamped `made=single` at the door like every decision. A reproduced publication says
so on its row, in words. The mark travels only with a publish: a box ticked before pressing
Decline is dropped, as a reason word picked before pressing Publish is. A refused submit keeps the
tick and the note, and a refused reproduction comes back open with its note.

★ **The Queue reads each root once per change, not once per view or poll** (ledger #667, and
ledger [#947](http://localhost:1060/l/default/item/947) for the page). The header badge, the Queue
page and its batch and walk views read every root's findings through ONE memo
(`queue::Reads`, held beside the watch's epochs): per root, per caller's grant and per view, what
the last read answered. A root is read again only when it has moved since: the watch cut its
narrow thread (a file changed), a write through the browse family named it (a review pass, a
judge's verdict, an annotation), a decision named one of its findings, or a write through the
store's door could have reached the browse graph (that one moves every root). A poll or a page
after no change reads no findings at all, and a page opened after a poll reads only what moved.
When several roots must be read they are read eight at a time (`queue::PARALLEL_READS`), and a
request that needs a read another request is already making waits for it and takes its answer
rather than starting a second.

Why it matters, measured 2026-10-09 on a copy of the live dataset (a backup restored into a
scratch store, 48 roots, 3,600 pending findings, a signed-in grant with `urn:cap:browse:read:*`):

| | before | after |
|---|---|---|
| `/queue`, the first after a badge poll | 50.8–51.9 s | 1.6–2.0 s (the rows' first render) |
| `/queue`, again | 50.8 s | 68 ms |
| the badge, cold (after a restart) | 49.4 s | 13.3–13.6 s |
| the badge, after no change | 5 ms | 5 ms |

⚠ **What still costs.** One root's findings read costs ~0.85 s on that dataset whatever the root
holds, 0 findings or 855, because `ikigai-browse` 0.18.0's listing loads every finding in the
archive and keeps one repository's. So a cold badge is still ~13 s, the first view of a tab
(published, declined, a batch kind) is seconds, and each root that moves costs its 0.85 s on the
next poll. A listing that reads one repository's findings is browse's to offer, and would make
every one of those numbers small.

⚠ **What an operator sees.** The page is as fresh as the epochs: a finding written to the store
by something that is none of this server's doors is not listed until its root moves (in
production nothing else holds the store), and a PR finding's supersession, which browse decides
from `gh pr diff` on every read, is as fresh as its root's last move. Both were already true of
the badge. The page still bounds what it DRAWS: the first 50 rows, saying "showing the first 50
of N" with a link to all of them (up to 500); the cost was never the drawing.

★ **The badge never polls over itself.** Its span carries `hx-sync="this:drop"`: a tick while
the last poll is still in flight is dropped. htmx's default queues the last tick instead, so a
poll slower than its ten-second interval ran back to back for as long as the page was open.

⚠ **Severity is self-reported by the model**, and a gate on the word makes the word
load-bearing: [#449](http://localhost:1060/l/default/item/449) measured a prompt asking for
"major or worse" moving the serious share 27% → 62% by re-labelling. Two defences. Nothing gonk
renders or sends reaches a pass — the prompt is browse's, and no hint, banner or form copy here
can tell a model that only serious words get read. And `urn:iki:gonk:review:depth` reports the
**serious share of what this run has minted** (`serious_share_percent`, with the histogram
`findings_by_severity`), counting derived passes only: it was 27–33% on the incumbent model and
42% on q8 ([#491](http://localhost:1060/l/default/item/491)), and a jump with no model or prompt
change is the label inflating — re-examine the gate, do not celebrate the number.

**The header carries a `Queue` link** only when the caller may read at least one root **and**
holds `urn:cap:annotate` — a link to a page of things you cannot decide is worse than no link.
For the same reason the decision form is drawn only for a caller who could submit it: an offer
you refuse teaches people to ignore refusals. An **anonymous loopback caller can do nothing to
a finding in either direction** — it cannot read the queue (that needs `urn:cap:browse:read:*`)
and cannot publish one (that needs `urn:cap:annotate` as well), and
`tests/queue.rs::an_anonymous_loopback_caller_can_neither_read_nor_decide_a_finding` measures
both rather than asserting the reasoning.

★ **The severity menu, the decision buttons and the state nav are rendered from the
CONTRACT** — `Meta urn:iki:finding:{id}` and `Meta urn:repo:{repo}:findings`, read through the
kernel — never from a list in this crate. `ikigai-browse` deliberately serves one constant to
the Sink's `one_of`, to the review prompt and to its own menu so the three cannot drift; a
fourth copy here would be the one that went stale in silence, and the refusal would arrive at
the click. `tests/queue.rs::no_severity_word_is_written_down_in_this_crate` holds the page code
and the stylesheet to it, with the contract as the oracle. ⚠ When a contract cannot be read,
**no form is rendered at all** rather than a fallback menu.

**Both ratings are kept.** The model's proposal rides on the finding; the human's final rating
rides on the decision node. That is what makes *"is this reviewer calibrated?"* a query rather
than an impression, and it is why `published` and `declined` are states on this page rather
than clutter swept off it.

★ **Decide in batches** (ledger [#506](http://localhost:1060/l/default/item/506), `ikigai-browse`
0.12.0). The machine PROPOSES groups of pending findings — the kinds are the findings face's own
`group` set, read from the contract like every other menu here — and the page shows one kind at
a time, each group's members listed and ticked, each box one click to untick. Nothing is decided
until the button: a batch is one ordinary `Sink urn:iki:finding:{id} decision=decline
reason=<word>` per ticked member, under the caller's own capability, reported as "Declined N of
M" with each failure named. **A batch decline needs a reason word**; the group's suggestion is
pre-selected, never applied unpressed, and a batch with no word is refused whole before anything
is written. The kind whose groups carry a declined TWIN is the exception (Brian, 2026-09-25):
every one of its groups sits in one form and each member is pre-set to its own twin's word — a
twin with no word leaves that member's picker empty and the batch refused until it is picked.
Members are filtered to the serious set under the default scope and the counts are gonk's; a
doc file's comment-shaped and whole-file groups are the same proposal and are shown once.
**Publish is not batchable** — a bulk publish is the one act that would write to what other
readers see.

⚠ **A decision is kept, and the page says what changing one would be.** An identical repeat is
a no-op; anything that would change the record without naming it is refused, naming what is on
file. There is **no Delete on a finding** — declining is how a human removes one, and the
decline is the record. Undoing a *publication* is `delete urn:iki:annotation:{id}`, a separate
visible act under the same capability.

★ **A decision can be revised, and a decline nobody evidently meant stops steering** (ledger
[#653](http://localhost:1060/l/default/item/653), `ikigai-browse` 0.14.0). Measured 2026-10-01:
the 43 declines that pending recurrences pointed at carried no reason word, and 39 were made in
same-second bursts — yet each marked its repeats and pre-ticked them in a recurrence batch. Now:

- **The door stamps how a decision was made.** `POST /queue/decide` and `/k`'s finding Sink
  forward `made=single`; `POST /queue/batch` forwards `made=batch batch=<the member's group key>`.
  A form naming `made` or `batch` is refused — the browser never chooses its own provenance, as
  it never chooses the author.
- **Pre-tick on CONFIRMED evidence only** (Brian, 2026-10-01, amending the 2026-09-25 rule).
  browse computes `confirmed` on every decision: a wordless decline made in a batch, or inside a
  burst of declines with no provenance on record, reads `false`. A repeat whose twin is
  unconfirmed starts unticked, and its row says why in words.
- **The mark says so.** A recurrence mark, a twin line and a decision line carry "unconfirmed: no
  word, made in …" in text, with a link to the walk.
- **The walk** (`?summary=unconfirmed`, offered when the findings contract declares it) lists the
  unconfirmed declines that still steer a pending finding, grouped by the burst or batch they
  were made in, oldest first — browse's answer, per readable root. Each carries two small forms
  posting to `/queue/decide` with `revises=<its decision>`: confirm or reverse (with a rating, a
  word and a note), and withdraw (a note only — the Sink refuses a rating beside it). Every
  revision is a new decision, made singly, and the old one is kept; a revised decline leaves the
  list.
- **The pending form offers only the words that can START an answer.** The contract's
  `decision` set gained a withdrawal word that is refused on an undecided finding; which words
  those are is read from the input's own summary, never from a list here.

**The intray depth is on this page and nowhere else.** `ikigai-browse` does not expose it and
deliberately did not add it: that number belongs to the trigger above, which lives in this
repo. Without it *"nothing has happened yet"* and *"39 still queued"* render identically, which
is exactly the silent absence this page exists to prevent — so the line has four shapes, and
three of them are not a number: no queue configured, the queue is empty, N waiting, and *the
queue could not be read*.

⚠ **That count is read from the directory, not through the kernel**, and the reason is a gap
worth naming: the depth lives behind `Source urn:space:{name}`, which requires
`urn:cap:space:read`, and this server mints that token for **nobody** — so there is no
capability any page could run under that would be allowed to ask. Either the depth becomes a
resource of gonk's own with its own floor, or the space's read token becomes mintable; until
one of those, the page counts files the way the startup banner already does.

## The render rules are a resource

A SPARQL result set is terminal by default: numbers you read out of the table and type
somewhere else. gonk turns some of its cells into controls — an item links to that item, a
`repo:` label asks the ledger for that repo's open items — and **what becomes a control is
decided by a rule table you can read, diff and replace**, served at `/render-rules` as Turtle:

```sh
curl http://127.0.0.1:1062/render-rules
```

Why a resource rather than a config key, or a list of column names in the binary: **a SPARQL
result set carries whatever variable names its author chose.** A renderer that special-cases a
column called `number` works for the queries shipped with it and fails for the query you write
tomorrow. So the rules match on what a cell IS — its term kind, the shape of its value, a value
prefix — and the table is data about how to read *this deployment's* data, which belongs in the
graph rather than in the code. Drop your own file at `<config home>/gonk/render-rules.ttl` and
gonk serves and obeys that one instead; a table it cannot obey stops the server at startup,
because a rule that silently did nothing would look exactly like one in effect.

| the rule shipped | matches | what the cell becomes |
| --- | --- | --- |
| `rule:item-iri` | a `urn:iki:ledger:{ledger}:item:{id}` value, under any column | a link to that item |
| `rule:item-number` | a bare integer, under a column named `number`, `item`, `n`, … | a link to that item |
| `rule:repo-label` | a literal starting `repo:` | a link that runs that repo's **open** items |

⚠ **A bare integer is genuinely ambiguous, and that is why one rule reads a column name.**
`SELECT ?number ?title (COUNT(?comment) AS ?comments)` renders `244 … 3`: a result set carries
no provenance, so the renderer cannot see that one integer came from `ledger:number` and the
other from a `COUNT`, and linking every integer would send "3 comments" to item #3. Checking
that item 3 exists does not help — it does. So that rule alone consults the name, its default
covers the obvious spellings, and the list lives in the table where you can extend it.

⚠ **Substitution is escaped for the grammar it lands in, by the renderer, never by the rule.**
Values come out of the store and the store holds text other people wrote. A `render:link`
template percent-encodes every substitution; a `render:query` template escapes it for the
SPARQL string-literal grammar, so a label containing a quote and a closing brace is *content*
and cannot change the shape of the query it runs. A rule author writes `"{value}"` and has no
way to opt out.

The vocabulary is `urn:iki:gonk:render#` and it is deliberately **not** in `ikigai-vocab`: it
says nothing about work, only about how one HTML face renders a result cell. The shipped table
is its own documentation — `web/render-rules.ttl` is a commented file, and it is the file
`/render-rules` serves.

## Filing and reading work over HTTP

Every resource of [`ikigai-ledger`](https://github.com/ikigai-rs/ikigai-ledger) is reachable
by the mechanical mapping `ikigai-web` uses: the path's segments become the IRI, the method
the verb, query parameters the arguments, the body the content, and `Accept` the
representation. `as` and `content` are the transport's own names, so a query parameter
spelled either is ignored.

```sh
curl -X POST --data-binary $'Publish the store\n\nIt needs a README.' http://127.0.0.1:1062/iki/ledger/append
curl http://127.0.0.1:1062/iki/ledger/items
curl http://127.0.0.1:1062/iki/ledger/next
curl -H 'Accept: text/turtle' http://127.0.0.1:1062/iki/ledger/items
curl -X POST 'http://127.0.0.1:1062/iki/ledger/close?item=1'
```

| method | path | resource |
| --- | --- | --- |
| `POST` | `/iki/ledger/append` | `Sink urn:iki:ledger:append` |
| `GET` | `/iki/ledger/items` | `Source urn:iki:ledger:items` |
| `GET` | `/iki/ledger/acme/next` | `Source urn:iki:ledger:acme:next` |
| `DELETE` | `/iki/ledger/item/01m2h…` | needs the delete grant — anonymous callers are refused |

⚠ These resource paths are for programs, not browsers. `ikigai-web` negotiates `Accept`
against the faces a resource declares, and the ledger's own resources serve no `text/html`, so
opening `/iki/ledger/items` in a browser shows the plain-text listing (a browser's `Accept` ends
in `*/*`); an `Accept` that names only faces the resource does not serve is answered `406`. The
pages above are the browser's way in.

A path nothing serves answers `404` (`no endpoint resolved for urn:…`, the IRI the path became).

A `DELETE` answers `200` with the ledger's own sentence (`deleted #1 …`, `#1 resumed`), the way
a `POST` does; an empty answer would be a `204`, and repeating a `DELETE` that succeeded is a
`204` for a while rather than a `404`. Deleting something that was never there is a `404`
carrying the ledger's message. Since gonk links `ikigai-web` 0.1.57 (ledger #1378); before it
every successful `DELETE` was a bodiless `204`. ⚠ A `DELETE`'s body reaches nothing, so a
Delete that takes its target as `content` (removing a label) is not reachable this way: send
it with the cli, or through the page's own form.

A write whose client asks `Expect: 100-continue` (curl does for a large body, and a JVM HTTP
client can for any) is sent the interim `100 Continue` at once and then answered as usual.
Since gonk links `ikigai-web` 0.1.60 (ledger #1408); before it the door never sent the `100`,
so such a client waited out its own expect timeout (curl 1 s, a JVM client 3 s) on every POST.

⚠ **There is no `urn:iki:ledger:comments`**, so `/iki/ledger/comments` is one of those paths.
Comments are read inside the item they belong to — `GET /iki/ledger/item/244` (`Source
urn:iki:ledger:item:244`) carries them, in Turtle too — and written through the one comment
resource the ledger binds, `Sink urn:iki:ledger:comment`. A listing face across items would be
[`ikigai-ledger`](https://github.com/ikigai-rs/ikigai-ledger)'s to add; this server binds the
ledger's resources and hand-writes none of its own over them, so it does not paper over the gap.

## Filing roborev findings

[roborev](https://github.com/kenn-io/roborev) reviews each commit with a coding agent. Keep
running it, give it a client certificate that may write one ledger, and add one `[[hooks]]`
entry to its config (`~/.roborev/config.toml`, or a repository's `.roborev.toml`): every
finding of every completed review becomes a ledger item here, which can be triaged, labeled,
linked, claimed and closed like any other, and which shows up on browse's page for the file it
is about.

```sh
ikigai-gonk client add roborev --ledger default=write    # then restart gonk
```

```toml
[[hooks]]
event = "review.completed"
command = "ikigai-gonk roborev file --quic 127.0.0.1:1062 --cert-dir ~/.config/ikigai/gonk/quic/clients/roborev --ledger default --root {repo_name} --repo-path {repo} --job {job_id} --sha {sha} --agent {agent} --findings {findings}"
```

`client add` writes the certificate directory (`client.crt`, `client.key`, and the
`server.crt` to pin) under `gonk/quic/clients/roborev/`; when roborev runs as another user, in
a sandbox or on another machine, move that directory to where it can read it, and point
`--cert-dir` and `--quic HOST:PORT` (gonk's QUIC door; it follows the HTTP port, 1062 by
default) there.

roborev replaces each `{…}` with a single-quoted value before `sh -c` runs the line, so
nothing in a finding can escape into the shell. `--root` is the **browse root name** gonk
serves the repository under (`gonk.browse.root = "<name>=<path>"`); `{repo_name}` is right
when the two match, and otherwise write the name in. `--dry-run` prints what would be filed
(each append's arguments, the key among them) and touches nothing.

**The door, and whose identity files.** With `--quic HOST:PORT --cert-dir DIR` the command
files at gonk's QUIC door as the client whose certificate is in `DIR`: mutual TLS, then the
grant `client add` enrolled it under. That grant is narrow (`--ledger default=write` writes one
ledger, and a write to any other is `Denied`), revocable (`ikigai-gonk client remove roborev`),
and the client's own: **every item is authored by the client**
(`urn:iki:host:gonk:client:<fingerprint>`, shown as `roborev`), not by gonk's owner. A
certificate gonk does not trust, or one enrolled under no grant, is refused at the door, and the
command says so. Without `--quic`, the command files over the running gonk's owner-only socket
(`gonk.socket`, default `~/.ikigai/gonk.sock`; `--socket PATH` or `--config PATH` names
another), whose caller is the user who runs gonk, `author roborev`: a convenience for a hook
running as that user, on that machine, outside any sandbox, which carries the owner's whole
authority over every ledger. **When both are configured, `--quic` wins** and the socket is not
dialed. ⚠ Through ledger #1687 the command filed over the HTTP door as an anonymous loopback
caller; that caller only READS now, which is right, so the command answered `403` and filed
nothing (ledger #2075). `--gonk` is now refused with exit 1, naming `--quic` and `--socket`:
drop it from a hook line written before. `--dry-run` touches no gonk at all.
A gonk that is not running, or a refusal, stops the command with exit 1 and says which; roborev
logs a hook's failure and output in its daemon log, and never retries.

**What a finding becomes.**

| roborev | ledger item |
| --- | --- |
| `problem` | the title (its first line, cut near 100 characters), and the body's first paragraph in full |
| `fix` | `Fix: …` in the body |
| severity | the labels `roborev` and the severity, and the priority: critical `0`, high `1`, medium `2`, low `3` |
| `location` (`file:line`) | `about urn:repo:{root}:file:{path}` — the join browse's file page makes. The line stays in the body. An absolute path under `--repo-path` is made relative; a location that names no file inside the repository gives no file `about` rather than a guessed one |
| the commit | `revision`, and `commit:` in the body |
| the job | `roborev job: N (roborev show N)` in the body |
| a panel's reviewers | `reported by:` in the body |
| — | `key urn:roborev:finding:{hash}`, the idempotence key — also filed as an `about`, so items from before keys existed join the same way — and the author: the enrolled client over `--quic`, `roborev` over the socket |

**Low findings are skipped by default** (`--min-severity medium`): roborev renders them on a
passing review too, and one ledger item per nit buries the findings worth a person's time.
`--min-severity low` files them; `high` files less.

**Filed once, race-free.** The key hashes the root, the file's path and the problem's text with
its whitespace collapsed, and each finding is ONE keyed append (`append key=…`,
`ikigai-ledger` 0.4.0): the ledger checks the key and files in the same store update, and a key
already taken — by an open, closed or deleted item — answers that item (`already` here) and
files nothing. So the same hook payload run twice files once, roborev carrying a finding
forward verbatim (a rerun, or a later commit's review) files nothing new, and **two hooks filing
the same finding at the same instant file it once** (eight concurrent hooks, one item:
`tests/roborev.rs`). The answer is read through the ledger's JSON face, never its plain text.
⚠ Two limits, both stated: the same defect described in **different words** is a new key and
a second item, and the key is deliberately not roborev's job id, because a rerun keeps its job
id and produces new findings, so (job, index) would skip a real one. Items filed before the
keyed append carry the key only as an `about`; [backfill their keys](#upgrading-the-bridges-backfill-the-keys)
once, or the first keyed run files them again.

**What it files nothing for.** `review.completed` also fires for roborev's `fix` and `task`
jobs, whose output is prose rather than a review: the command says so and exits 0. A review
with no findings files nothing. A review that opens like roborev's rendering but does not
parse back to it exactly (format drift in a new roborev) is refused with exit 1 and nothing
filed, rather than filed as a guess.

**Concurrent hooks are safe.** roborev runs every hook in its own goroutine, so two reviews
can file at once, and the keyed append files each finding once however many race (above).
⚠ **A panel still files more than once:** each member's completion fires
`review.completed` (only the bookkeeping events are hook-suppressed), and the synthesis fires
once more with the findings merged and reworded. A hook cannot tell a member from a synthesis
(there is no job-type variable), so a panel files every member's findings and the
synthesis's, deduplicated only where the words match. Point this hook at single-agent reviews
if one item per defect matters more than seeing every reviewer's wording.

## Filing gonk's own findings

gonk's review pass mints **pending findings**, and a person publishes or declines each one at
the [Queue](#the-review-queue-a-human-publishes-or-nothing-does). A published finding is a fact
about the code, and fixing it is work, so it belongs on the ledger. This command files them, the
way `roborev file` files roborev's:

```sh
ikigai-gonk findings file ikigai-gonk --dry-run          # read and plan; files nothing
ikigai-gonk findings file ikigai-gonk                    # file them into `default`
ikigai-gonk findings file ikigai-gonk --ledger reviews --reproduced
```

The argument is the browse root's NAME. It speaks to a RUNNING gonk over its owner-only socket
(`--socket PATH`; unset, the one `config.toml` or `--config` names, else
`~/.ikigai/gonk.sock`), because reading findings needs `urn:cap:browse:read:*`, which no
anonymous HTTP caller holds. Each finding is one line:

```text
filed    #1462  Slicing the first byte panics on an empty word.
already  #1388 (closed)  urn:iki:finding:645d521aab16830f076e0857
skipped  1fa92632a54c273633bc10fe  minor is not in gonk.queue.serious (critical,major); --severity all files it

3 published finding(s) on ikigai-gonk: 1 filed, 1 already filed, 1 skipped
```

**What is filed: published findings, never pending ones.** A pending finding is a model's claim
nobody has looked at, and the review-value experiment (ledger
[#723](http://localhost:1062/l/default/item/723)) measured how much of that is worth a person's
time: a per-commit reviewer found less than half the serious bugs per token of an unled audit,
and about half of its serious claims were coin flips. Filing every pending guess would make the
ledger a second review queue with no decline button. The Queue decides; the ledger holds what was
decided. By default only the **serious set** is filed (`gonk.queue.serious`, the words the Queue
asks a person about, judged on the row's `effective_severity`, which is the person's word when
they re-rated it): a published remark below it is often praise or a note worth keeping on the
file's page, not work. `--severity all` files every published finding; `--reproduced` files only
those published with the "reproduced" mark (a person showed the defect happen).

**What a finding becomes.**

| finding | ledger item |
| --- | --- |
| its text | the title (its first line, cut near 100 characters), and the body in full |
| the file and line, the quote, the severity (and the model's proposal when a person re-rated it), the reviewer, the decision's time and note, the judge's verdict | lines in the body |
| `urn:iki:finding:{id}` | `key`, the idempotence key, and an `about` |
| the file (or pull request) it annotates, and the annotation its publication minted | `about`, so browse's page for the file lists the item |
| — | the labels `finding`, `repo:<root>`, the severity word, and `reproduced` when it was marked |

**Filed once.** Each finding is ONE keyed append (`key=urn:iki:finding:{id}`): the ledger checks
the key and files in the same store update, and a key already taken (by an open, closed or
deleted item) answers that item (`already`) and files nothing. A finding's id hashes its pass and
its quote, so it is stable, and running this after every triage session, or from a timer, files
only what was published since. ⚠ An item filed by hand before this command existed, with some
other key (the gonk Book's "Review every commit" chapter used `key=finding:{id}`), is not that
key, and the finding is filed again beside it. No priority is set: the severity is a label, and
ranking is the ledger's `next` policy's call.

**Why a command, and not a step inside the publish.** The Queue's publish runs under the deciding
person's grant, which holds `urn:cap:annotate` and need not hold any ledger's write token; which
ledger a root's work belongs in is a setting nobody has written; and one click with effects in two
graphs leaves no way to publish without filing. So the decision and the filing stay two acts, and
the key makes the second one free to repeat.

## Importing from kata

[kata](https://github.com/kenn-io/kata) keeps issues in its own database. `kata export` writes
it out as JSONL, and `ikigai-gonk kata import` files that file's issues into a ledger here, as
a client enrolled to write that ledger:

```sh
ikigai-gonk client add kata --ledger default=write       # once; then restart gonk
kata export --output kata.jsonl          # with kata's daemon stopped, or --allow-running-daemon
ikigai-gonk kata import kata.jsonl --ledger default \
    --quic 127.0.0.1:1062 --cert-dir ~/.config/ikigai/gonk/quic/clients/kata
```

```text
kata export version 27: 4 issue(s), 4 link(s)
filed     #1       kata 0001  Stop the loop on an empty queue
commented #1       kata comment 01K6Z0000000000000000000C1
…
closed    #2       kata 0002
deleted   kata 0004  not imported
linked    #3 parent #1

3 filed, 0 already there; 2 comment(s), 1 close(s), 3 link(s) added; 1 deleted issue(s) and 1 link(s) skipped; not read: event 1, sqlite_sequence 1
```

`--ledger` defaults to `default`; `--project NAME` imports one kata project's issues. Without
`--quic`, the import runs over the owner-only socket instead; `--socket PATH` (or `--config
PATH`) names it when it is not `gonk.socket` of this user's config.

**`--dry-run` reads for real and writes nothing.** It asks the ledger for each issue's key, as
the real run does, so it says `already` for an issue that is there, reports exactly the
comments, closes and links a real run would add (`would commented`, `would closed`, `would
linked`, and a tally that says `would be`). ⚠ The one thing a read cannot tell: an issue whose item was DELETED
answers like one never filed, so the dry run says `would file` where the real run leaves it
alone.

**The door** is `roborev file`'s. With `--quic HOST:PORT --cert-dir DIR`, the QUIC door as the
enrolled client in `DIR`, whose grant must write the ledger (an import also reads it, to
converge on what is already there; `write` includes read). Every item, comment, close and link
is then authored by that client, and kata's own author stays in the text (the provenance
line, and each comment's ending). Without `--quic`, the owner-only socket, as the user who runs
gonk, with kata's authors as the ledger's `author`. **When both are configured, `--quic` wins.**
⚠ Through ledger #1687 it imported over the HTTP door as an anonymous loopback caller, which
only reads now, so an import answered `403` (ledger #2075); `--gonk` is now refused with exit
1, naming `--quic` and `--socket`.

**The mapping.** `ikigai-ledger` took its model from kata, so most of this is one to one:

| kata | ledger item |
| --- | --- |
| `title` | the title (whitespace collapsed to one line) |
| `body` | the body, followed by a provenance line: `Imported from kata <project>#<short_id> (<uid>), filed <created_at> by <author>; owner <owner>.` |
| `author` | `author` over the socket (over `--quic`, the client is the author, and kata's author is in the provenance line) |
| `priority` 0–4 (absent = unset) | `priority` 0–4, the same scale (absent = unset) |
| labels | `labels` |
| comments | `comment`, with the comment's author over the socket, ending `(kata comment <uid>, <created_at> by <author>, for <teammate>)` |
| `status` closed, `closed_reason` | `close` with the same reason (`done`, `wontfix`, `duplicate`, `superseded`, `audit-no-change`), and `Closed in kata at <closed_at>.` as its note |
| links `parent`, `blocks`, `related` | `link` with the same type, same direction (`parent` from the child, `blocks` from the prerequisite), made after every issue has an item |
| `uid` | `key urn:kata:issue:<uid>`, the idempotence key, and the same IRI as an `about` |
| `deleted_at` set (soft-deleted) | not imported, nor its comments, labels or links |

kata has no item kind, so none is filed.

**What is lost.** The ledger stamps its own filed time and mints its own number, so kata's
`created_at`, `short_id` and `uid` survive only as the text above; a faithful import that keeps
them as data is ledger [#774](http://localhost:1060/l/default/item/774). Not carried at all:
`owner` as a claim (it is in the provenance line), `updated_at`, `metadata`, assignment
expiry, recurrences, who added a label or a link and when, kata's event history, and every
other export kind (sync bindings, federation state, claims, purge logs), which are counted
on the last line instead.

**Running it again, or twice at once.** Each issue is ONE keyed append: the ledger checks the
key and files in the same store update, so an issue already there is not filed again, and two
imports of one export at the same instant file each issue once (eight concurrent imports, one
item: `tests/kata.rs`). The run then reads the item by its key (`item:key:urn:kata:issue:<uid>`,
in the ledger's JSON face) and CONVERGES on it, addressing every write `item=key:…`: a comment
whose `kata comment <uid>` marker is in none of its comments is added, an issue closed in kata
whose item is open is closed, and a link the item does not carry is made. So the same export
imported twice changes nothing, and a later export adds the comments, closes and links that
are new. An issue whose item was deleted in the ledger is left alone (`deleted … left alone`):
its tombstone keeps the key, and an import does not undo a deletion. ⚠ An issue's title, body,
labels and priority are written once, when it is filed; a later edit in kata does not reach the
item. And only the FILING is atomic: the convergence reads the item and then writes, and the
ledger has no keyed comment, so two imports racing on the same new comment can both add it.
Items imported before the keyed append carry the key only as an `about`;
[backfill their keys](#upgrading-the-bridges-backfill-the-keys) once, or the next import files
them again.

## Upgrading the bridges: backfill the keys

`roborev file` and `kata import` have filed through the keyed append since `ikigai-ledger`
0.4.0 (ledger [#810](http://localhost:1060/l/default/item/810)). An item either filed BEFORE
that carries its key only as an `about` IRI (`urn:roborev:finding:…`, `urn:kata:issue:…`) and
no `ledger:key`, so the keyed append cannot see it and the first keyed run would file it again.
The ledger sets a key only at filing (it has no resource that sets one later), so this command
writes them, over a RUNNING gonk's owner-only socket (root; the store has one writer, the
server, so nothing can open it beside a live gonk):

```sh
ikigai-gonk ledger backfill-keys --ledger default --dry-run   # read and plan; writes nothing
ikigai-gonk ledger backfill-keys --ledger default             # write the keys
```

`--socket PATH` names the socket; unset, it is the one `config.toml` (or `--config`) names,
else `~/.ikigai/gonk.sock`. Per bridge IRI, the **lowest-numbered** item about it without a key
gets it (the first filing); every other unkeyed item about the same IRI is a **duplicate** —
what the old check-then-append race made — and is listed with the line that closes it, never
closed for you:

```text
would key  #12  urn:roborev:finding:0f3c…
duplicate #19 of #12  urn:roborev:finding:0f3c…
    close it: sink urn:iki:ledger:close item=19 reason=duplicate

dry run, nothing written: 41 key(s) would be written, 0 item(s) already keyed, 1 duplicate(s) listed, 0 skipped
```

Each key is ONE conditional update (`FILTER NOT EXISTS` on the item already having a key and
on the key being taken), so it never puts two keys on an item or one key on two, even against
a bridge filing at the same moment; the run reads the keys back and reports any it lost that
way instead of claiming them. If some other subject already holds an IRI's key (a keyed run
filed it again after the upgrade, or a deleted item's tombstone), nothing is keyed and the
unkeyed items are listed as its duplicates. An item about two bridge IRIs is skipped and named.
It does not touch `modified`, and it does not key a deleted item's tombstone. A second run
writes nothing (`0 key(s) written`).

`findings file` is the third bridge (ledger [#1502](http://localhost:1062/l/default/item/1502)):
a published finding someone filed by hand, `about` `urn:iki:finding:{id}` with no key, is keyed
the same way, so the next `findings file` answers `existing` instead of filing it again.
⚠ Only an UNKEYED item: the gonk Book's old hand step filed `key=finding:{id}`, and since a key
is set once, such an item is reported as `skipped` (already keyed, not its bridge IRI) and
`findings file` files that finding beside it. Close one of the two by hand.

**The order, at an upgrade:** dry run, read the duplicates, run it, then let the bridges run
again. Running it after a keyed run is safe too — that run's re-filings are listed as the
duplicates' holders rather than keyed twice — but it is the order that files nothing extra.

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
# ⚠ no trailing colon
mount = "prefer urn:iki:annotation=/Users/you/.ikigai/gonk.sock"
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

★ **A mounting client caches nothing it gets from here, and that is deliberate.** Golden
threads are kernel-local and do not cross a wire (ledger
[#92](http://localhost:1060/l/default/item/92)), while an answer's expiry does. So an answer
that left the socket or QUIC door cacheable would arrive with nothing that could ever cut it,
and a long-lived client — `ikigai mcp`, the daemon, `ikigai serve`, `ikigai-web` — would serve
it until it restarted. Measured on plasma before the fix: a watched `urn:repo:{root}:tree` read
through a `prefer` mount of this socket came back `[cached]` and went on listing the tree as it
was after a file was added under the root, while gonk itself answered fresh. Every answer that
rests on a thread now crosses those two doors uncacheable (`doors::for_the_wire`); the hub
keeps its own entry, its expiry and its threads, so a mounted read costs one round trip to a
hub cache hit and never a recompute. The HTTP door is unchanged — it turns an expiry into
`Cache-Control` and an `ETag` a browser revalidates. `tests/doors.rs` and
`the_retirement_mount_lines_reach_gonk_fresh_and_mint` in `tests/browse.rs` drive a real
mounting client and fail without it.

### SPARQL as resources: `urn:sparql:*`

gonk binds the query face the ecosystem's clients already speak — `urn:sparql:select`, `:ask`,
`:construct` and `:describe` — over the store it holds (ledger
[#836](http://localhost:1060/l/default/item/836)). One more mount line puts it in front of any
ikigai process; `ikigai-web` reads the same line as `web.mount`:

```toml
mount = "prefer urn:sparql:=/Users/you/.ikigai/gonk.sock"
```

```sh
ikigai --connect ~/.ikigai/gonk.sock --plain -c 'source urn:sparql:select as=text/csv query="SELECT ?g (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g"'
```

**The default dataset is the UNION of every named graph the caller may read** — the graphs
`urn:iki:store:graphs` lists for that caller's grant — so a bare `{ ?s ?p ?o }` matches a
triple in any of them and `GRAPH ?g` binds each one. That is what the dev server's face
answered, so the queries written against it (`claude/class/review-queries.md`) keep their
meaning. It is **not** what `urn:iki:store:select` answers: the store's broad door reads the
store's own default graph, which is empty here (ledger
[#378](http://localhost:1060/l/default/item/378) — the same text, two datasets). Every form's
description says which, in one sentence.

- **`graph=`** narrows the dataset to the graphs named — commas or whitespace between them, as
  `urn:sparql:*` has always spelled it — and each one needs
  `urn:cap:store:read:graph:<iri>`. A graph the caller cannot read is **Denied whether or not
  it exists**, so the face is not an existence oracle.
- **It is a mapping, not a second query engine.** Each form is one hop onto
  `urn:iki:store:graph-{form}` under the caller's own capability, which confines the dataset by
  construction. `FROM` / `FROM NAMED` are refused (the store's rule: `graph=` is the dataset), and
  an `as` the form cannot answer in is refused rather than substituted.
- **Sealed.** No `urn:sparql:update` is bound, and nothing here reaches the store's write doors;
  an update passed as `query` is not a query.
- **The corridor is the door.** The socket's root reads every named graph — today the browse
  graph and the ledger's; a QUIC client reads the graphs its grant names; the HTTP door's
  anonymous caller reads its ledgers' graphs.
- **Faces:** SPARQL results JSON (default), XML, CSV and TSV for SELECT and ASK; Turtle
  (default) and N-Triples for CONSTRUCT and DESCRIBE. `ikigai-sparql` also offered N-Quads,
  TriG, RDF/XML and JSON-LD; the store serves neither, so they are not declared.

## Retiring `ikigai-dev-server`: the cutover

gonk serves the browse family for every ikigai repository and, since ledger
[#836](http://localhost:1060/l/default/item/836), the `urn:sparql:*` query face, so nothing a
dev-server client relied on is missing here except what is listed below. The retirement is a
change to four config-home lines, a restart of the processes that read them, and stopping one
LaunchAgent — **not a data migration** (ledger [#244](http://localhost:1060/l/default/item/244),
[#411](http://localhost:1060/l/default/item/411), [#312](http://localhost:1060/l/default/item/312)).

### What was measured, and what differs

The same commands over both sockets, read-only, on 2026-10-07 — dev server `ikigai-dev`
(browse 0.3.2) at `~/.ikigai/dev.sock`, gonk (browse 0.18.0) at `~/.ikigai/gonk.sock`. Per
root: `tree`, `tree:{dir}`, `file:{path}`, `hash`, `hash:{path}`, `state`,
`explain-versions[:{path}]`, `annotations[:{path}]`, `explain:{path} version={tag}` and a
contract (`describe`); then `urn:repo:style`, `prs`, `pr:{n}`, the facades with and without
`dir=`, the annotation family, and SPARQL. The bytes are compared after dropping the REPL's
cache trailer.

| shape | gonk vs the dev server | why |
| --- | --- | --- |
| `tree`, `file`, `hash`, `state`, `prs`, `pr:{n}`, `style` on the six shared roots | identical bytes | the same working trees, read the same way |
| `explain:{path} version={tag}`, `pr:{n}:explain version={tag}` | identical bytes | the archive entries are identical (below) |
| `explain-versions`, `annotations` | gonk lists MORE | every dev entry is present, plus what gonk derived and reviewed since |
| `describe` on browse rows | gonk's contract is larger | browse 0.18.0 against 0.3.2: new arguments, none removed |
| `urn:repo:style:layout` | gonk only | added after browse 0.3.2 |
| facades (`urn:repo:status`, `log`, `branch`, `list`, `pr:list`) | identical | both run in a working directory that is not a repository, so both need `dir=` and both refuse without it the same way |
| a capability narrowed to the `claude` grant's scopes, or to none | identical answers and identical denials | the caller's capability crosses either socket |
| `urn:iki:annotation:{id}` read, Meta | identical | the same endpoint |
| **cacheability** | **dev: every read uncacheable. gonk: watched reads cacheable** | gonk watches its roots; that is why the wire fix above had to land first |
| **`urn:repo:folio:*`** | **dev only** | a third party's codebase, deliberately not a gonk root — a disclosure decision. Its 82 archived explanations are not carried |
| **`explain provider=`** | **dev allows `big`, `ollama`, `qwen`, `rapid` besides its tiers; gonk allows its two tiers only** | `browse.allow_model` in `dev.toml`; gonk deliberately passes no allowlist (`src/browse.rs`, `explain_config`) |
| **review ceiling** | **dev 1600 tokens; gonk 800** | `browse.review_max_tokens = 1600` in `dev.toml`; `gonk.explain.review.max_tokens` is unset |
| `urn:sparql:{select,ask,construct,describe}` | **both; the same answers on the same data** | measured below: every class query identical row for row. The default dataset is the union of the caller's readable graphs on both |
| `urn:sparql:update` | **dev only** | the dev server bound a write over its shared store; gonk binds no write under `urn:sparql:` |
| `urn:sparql:*` with `FROM <g>` | **dev IGNORES it and answers the union; gonk refuses** (400 at 8642) | name the graphs in `graph=` instead |
| `urn:sparql:*` with an `as` of the other family (`application/sparql-results+json` on a CONSTRUCT) | **dev substitutes its default; gonk refuses** (400 at 8642) | `ikigai-web` maps `Accept` to `as=` without looking at the query form, so a client listing a results type FIRST for a CONSTRUCT is refused after the cutover |
| `urn:sparql:construct`/`describe` faces | **dev: six RDF syntaxes; gonk: Turtle and N-Triples** | the store serves two; Turtle stays the default |
| the `urn:ikigai:vocab` graph (489 quads) | **dev only** | no class query reads it (measured below); a census over the union counts it on the dev server and not here |
| `urn:annotation`, `urn:annotation:{id}` | bound by NEITHER | browse 0.3.0 renamed the namespace to `urn:iki:annotation`; the config line that mounts it routes nothing today |
| `urn:rdf:*`, `urn:llm:*`, `urn:system:exec` | dev binds them | no config-home line routes any of them to the dev server, so the cutover does not move them: a cli host keeps its own |

The archive needs no migration. Every dev-server quad outside `folio` and the vocabulary —
1,147 of them: 96 explanations, 3 reviews and 14 annotations with their selectors, over
`ikigai-browse`, `ikigai-cli`, `ikigai-core`, `ikigai-devtools`, `ikigai-emacs` and
`ikigai-web` — is already in `urn:iki:browse:graph:default`. Their N-Triples matched one for one
(1,147 against 1,147, none on either side alone). The dev store's 2,374 default-graph quads are
those 1,147, `folio`'s 738 and the vocabulary's 489. Its 6 MB on disk is RocksDB's, not the
archive's. The root names agree because of the 2026-09-17 rename, so every carried IRI is
already the IRI a gonk read builds.

**SPARQL parity, measured read-only on 2026-10-07.** The eight queries in
`claude/class/review-queries.md` and a census, against the live dev server's `urn:sparql:select`
and against a scratch gonk (this tree) loaded from the newest backup. Three comparisons:

| comparison | the eight class queries | census |
| --- | --- | --- |
| dev vs gonk, **the same data** (the dev server's 2,374 triples copied into one scratch graph, queried with `graph=`) | identical, row for row | identical |
| the same data **without the vocabulary's 489** | identical — no class query reads `urn:iki:vocab` | 1,885 against 2,374 |
| dev vs gonk's union (the live browse graph, 348,072 quads, and the ledger's, 12,306) | gonk answers more on 1–5 (its archive is larger) and the same on 6–8 | 2,374 against 360,378 |
| gonk's union vs the browse graph alone | identical — the ledger graph changes no class query | differs by the ledger's 12,306 |

So the vocabulary is not a dependency of any real query. **Proposed, not decided:** if schema
joins (`?e a/rdfs:subClassOf* ik:Endpoint`) are wanted here, load `ikigai-vocab`'s Turtle into a
graph of gonk's own (`urn:ikigai:vocab`, read-granted with the browse graph) at startup, rather
than folding it into the union for every caller; nothing needs it today.

### The procedure

Each step is the hub's, typed by an operator, in this order. **Repoint before stopping**:
a `prefer` mount to a server that is gone falls back quietly, so the other order shows up as a
REPL and an `ikigai-web` that have lost `urn:repo:` with nothing saying why.

```sh
# 0. gonk WITH the wire fix above and `urn:sparql:*` — before any line moves. From a checkout
#    at origin/main:
cargo install --locked --force --path ~/git-personal/ikigai-gonk
just -f ~/git-personal/ikigai-devtools/justfile reregister --only gonk
#    a mounted read must now be uncacheable at the client — the old binary says `cached`:
ikigai --prefer "urn:repo:=$HOME/.ikigai/gonk.sock" --plain \
  -c 'source urn:repo:ikigai-gonk:state' -c 'cache urn:repo:ikigai-gonk:state'
#    expect the last line: not cached

# 1. a backup, through the running server
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:gonk:backup'
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:gonk:backup:status'

# 2. nothing left behind: every subject the dev archive holds, outside folio and the
#    vocabulary, that gonk's browse graph does not. Read-only on both live servers.
#    ⚠ CSV lines end in CRLF: without the `tr` every IRI differs and the answer is "all of them".
ikigai --connect ~/.ikigai/dev.sock --plain -c 'source urn:sparql:select as=text/csv query="SELECT DISTINCT ?s WHERE { ?s ?p ?o FILTER(isIRI(?s) && !CONTAINS(STR(?s), \":folio:\") && !STRSTARTS(STR(?s), \"https://ikigai-rs.dev/ns\")) }"' \
  | tr -d '\r' | grep '^urn:' | sort -u > /tmp/retire-dev.s
ikigai --connect ~/.ikigai/gonk.sock --plain -c 'source urn:iki:store:select as=text/csv query="SELECT DISTINCT ?s WHERE { GRAPH <urn:iki:browse:graph:default> { ?s ?p ?o } }"' \
  | tr -d '\r' | grep '^urn:' | sort -u > /tmp/retire-gonk.s
wc -l < /tmp/retire-dev.s                          # 141 on 2026-10-07
comm -23 /tmp/retire-dev.s /tmp/retire-gonk.s      # expect NOTHING

# 3. the config home: a copy first, then the edit below
cp ~/.config/ikigai/config.toml ~/.config/ikigai/config.toml.bak-$(date +%Y%m%d-%H%M%S)-retire-dev
```

If step 2 prints anything, the dev server derived or annotated something after 2026-10-07.
Everything it holds is machine-generated and regenerable — Brian, 2026-09-19, ledger
[#433](http://localhost:1060/l/default/item/433) — so the choice is to let gonk re-derive it on
demand, or to carry it with *Importing an archive from another host's store* above, which
stops gonk for the copy. Decide before step 6; after it the source is offline.

**The edit.** These lines, today:

```toml
# The dev server (ikigai-dev, dev.toml) serves the browse family; the
# annotation family spans a second URN prefix, hence two lines.
mount = "prefer urn:repo:=/Users/brian/.ikigai/dev.sock"
# no trailing colon on urn:annotation: covers the bare mint IRI AND the slug family
mount = "prefer urn:annotation=/Users/brian/.ikigai/dev.sock"
mount = "prefer urn:iki:annotation=/Users/brian/.ikigai/dev.sock"
# The web process's own sparql route to the shared browse store (web.mount
# is read only by ikigai-web - cli hosts keep their local sparql space).
web.mount = "prefer urn:sparql:=/Users/brian/.ikigai/dev.sock"
```

become:

```toml
# gonk (dev.ikigai-rs.gonk) serves the browse family; the dev server retired
# 2026-10 (ledger #244, #411). The annotation family spans a second URN prefix,
# hence two lines. No trailing colon on urn:iki:annotation: it covers the bare
# mint IRI AND the slug family.
mount = "prefer urn:repo:=/Users/brian/.ikigai/gonk.sock"
mount = "prefer urn:iki:annotation=/Users/brian/.ikigai/gonk.sock"
# The web process's own sparql route (web.mount is read only by ikigai-web - cli
# hosts keep their local sparql space). gonk serves urn:sparql:* over the union of
# the graphs the caller may read (ledger #836).
web.mount = "prefer urn:sparql:=/Users/brian/.ikigai/gonk.sock"
```

- `urn:repo:` and `urn:iki:annotation` move to `gonk.sock`, unchanged otherwise. The second
  keeps **no trailing colon**: `urn:iki:annotation:=` would not claim the bare IRI a Sink mints
  under, and every new annotation would fail to route, silently.
- `urn:annotation` is **deleted, not repointed**: neither server binds anything under it, so it
  routes nothing today and would route nothing tomorrow.
- `web.mount` is **rewritten to `gonk.sock`, not deleted**: gonk binds `urn:sparql:*`
  ([SPARQL as resources](#sparql-as-resources-urnsparql)), so 8642's `/sparql` keeps answering
  — now from gonk, with the same default dataset (the union) and the differences in the table
  above.
- ⚠ **What 8642 reads changes, and it is the hub's decision, not this line's.** `ikigai-web`
  issues every request under ROOT and the socket door admits root, so `/sparql` will read every
  named graph gonk holds — the browse graph AND the work ledger — where the dev server held only
  the browse archive. Measured 2026-10-07, though: that exposure **is already live**. The
  `mount = "prefer urn:iki:store:=…/gonk.sock"` and `urn:iki:ledger:` lines are composed by
  `ikigai-web` too, and `web.bind = "0.0.0.0:8642"`, so `GET /urn:iki:ledger:items` and
  `GET /urn:iki:store:select?query=…` already answer the whole ledger on the LAN interface. The
  rewrite adds a friendlier door onto data that is already exposed. Narrowing it — a capability
  ceiling on `ikigai-web`, or a narrower gonk door for it — is a decision for before or with the
  cutover, and is not made here.
- `web.bind` stays: it is the 8642 door's own, and whether that door outlives the dev server is
  its own decision ([#411](http://localhost:1060/l/default/item/411)).
- Two comment paragraphs above the `gonk.browse.root` lines stop being true at the same moment —
  the one beginning "⚠ The `mount = "prefer urn:repo:=…/dev.sock"` line above still points the
  REPL at the DEV SERVER" and the one beginning "⚠ gonk starts with an EMPTY explanation
  archive". Delete both.

```sh
# 4. restart what reads the `mount` lines. Not gonk (it reads only gonk.* keys, and step 0
#    restarted it), not dev.ikigai-rs.inference (it runs with --no-config-mounts), not the cms
#    (it reads no `mount` key).
#    ⚠ personal is the EventKit host: be at the screen.
just -f ~/git-personal/ikigai-devtools/justfile reregister --only web personal
#    and every running `ikigai mcp` (each Claude Code session's: /mcp, then reconnect ikigai),
#    and an `ikigai --daemon` if one is running. A REPL or a one-shot reads the file at start.

# 5. verify through the config home's own mounts — no --connect, no --prefer
ikigai --plain -c 'source urn:repo:ikigai-gonk:state'
#    a root only gonk serves: the dev server answers "no endpoint resolved", so success here
#    means the line moved
ikigai --plain -c 'source urn:repo:ikigai-core:state' -c 'cache urn:repo:ikigai-core:state'
#    expect: a commit hash, `clean`, and then `not cached`
ikigai --plain -c 'source urn:repo:ikigai-core:explain-versions:crates/ikigai-core/src/verb.rs'
#    expect a code-v1@qwen3-coder:30b row: an entry the dev server derived, served from gonk
ikigai --plain -c 'source urn:iki:annotation:00000000-0000-0000-0000-000000000000'
#    expect "no annotation …" (browse answered); "no endpoint resolved" means the line is wrong
curl -s 'http://127.0.0.1:8642/urn:repo:ikigai-gonk:tree'
#    ikigai-web, restarted in step 4: a tree listing (today it says "no endpoint resolved")
curl -s -G 'http://127.0.0.1:8642/sparql' -H 'Accept: text/csv' --data-urlencode 'query=
  PREFIX oa: <http://www.w3.org/ns/oa#> PREFIX dct: <http://purl.org/dc/terms/>
  SELECT ?creator (COUNT(?a) AS ?notes) WHERE { ?a a oa:Annotation .
  OPTIONAL { ?a dct:creator ?creator } } GROUP BY ?creator'
#    review-queries.md §3, through web.mount: gonk answers SEVERAL creator rows (the dev server
#    answered one, `qwen3-coder:30b,14`). "no endpoint resolved" means web.mount is wrong

# 6. stop the dev server, and keep it from coming back at the next login. Typed as two
#    commands on purpose — never `bootout … && …` (CLAUDE.md 9h).
launchctl bootout gui/$(id -u)/dev.ikigai-rs.dev
mkdir -p ~/Library/LaunchAgents-retired
mv ~/Library/LaunchAgents/dev.ikigai-rs.dev.plist ~/Library/LaunchAgents-retired/
launchctl print gui/$(id -u)/dev.ikigai-rs.dev     # expect an error: no such service
#    then step 5 again, the /sparql probe included: every answer is the same, because
#    nothing was routed there.
```

The dev server's store, `~/.ikigai/browse-store`, stays on disk: it is the rollback, and it holds
`folio`'s archive, which nothing else does.

⚠ After the config edit, `reregister --check` — and so the health watcher — reports
`stale-config` for every unit started before it that was not restarted (the cms, inference). They
read nothing that changed. Either restart them too with `--only cms inference`, or expect that
mail once.

### Rollback

```sh
cp ~/.config/ikigai/config.toml.bak-<the stamp from step 3>-retire-dev ~/.config/ikigai/config.toml
mv ~/Library/LaunchAgents-retired/dev.ikigai-rs.dev.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.ikigai-rs.dev.plist
just -f ~/git-personal/ikigai-devtools/justfile reregister --only web personal
#    and reconnect every `ikigai mcp`, as in step 4
```

Nothing is lost going back — gonk keeps everything it derived — but anything explained or
annotated through the new lines is in gonk's archive and not the dev server's, so the REPL stops
seeing it until the next cutover. Step 0's gonk does not need rolling back: an uncacheable
answer is never wrong, only one round trip slower.

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
banner reports `quic  udp 0.0.0.0:1062`. The QUIC door's port **follows the HTTP door's**:
`--port 1070` (or `gonk.port = 1070`) puts it on UDP 1070 too, and `client add` prints the
`--connect` line with that port (pass it `--port` or `--quic-bind` when the server is started
with flags rather than config). Until ledger #816 the QUIC default was UDP 1060 whatever
`--port` said, so a scratch gonk with an enrolled client collided with a live one.

Copy that directory to the other machine; it holds `client.crt`, `client.key` and the
`server.crt` to pin, laid out as the cli's `--cert-dir` expects. There, with a cli built with
QUIC:

```sh
cargo install --locked ikigai-cli --features quic
ikigai --connect quic://gonk-host:1062 --cert-dir ~/gonk-laptop -c 'source urn:iki:ledger:items'
```

or, to put the ledger into that machine's own ikigai processes:

```toml
mount = "prefer urn:iki:store:=quic://gonk-host:1062 /home/you/gonk-laptop"
mount = "prefer urn:iki:ledger:=quic://gonk-host:1062 /home/you/gonk-laptop"
```

**To keep a client's private key on the client**, let it generate its own identity and
enrol only the certificate: `ikigai cert generate --dir ~/gonk-laptop` there, copy its
`client.crt` here, and `ikigai-gonk client add laptop --cert client.crt --ledger
default=write`; then copy the bundle's `server.crt` back over the one in `~/gonk-laptop`.

**A client's lifecycle** (ledger #816, and #864 R7):

```sh
ikigai-gonk client list                      # every bundle and enrolled fingerprint, its grant, and whether it is admitted
ikigai-gonk client add laptop --ledger default=delete --force   # a NEW GRANT; the identity is kept
ikigai-gonk client add laptop --rotate       # a NEW IDENTITY under the SAME grant; the old fingerprint's enrolment moves to it
ikigai-gonk client remove laptop             # the bundle and its enrolment (grants.json is left alone)
ikigai-gonk client remove --fingerprint <fp> # an enrolment with no bundle
```

⚠ `--force` replaces a client's GRANT and never its key pair. Until ledger #864 (R7) it did
both: the operator ran the `--force` the grant refusal told them to, the client's deployed
certificate silently stopped being trusted at the next restart, and its old fingerprint
stayed enrolled beside the new one. `--rotate` is the explicit replacement, with `--cert` to
import a certificate the client generated itself. With no `--ledger`/`--browse` flags it keeps
the client's authority: the old certificate's enrolment moves to the new one, same grant, and
`grants.json` is not touched. Through `642a265` it left the new certificate enrolled under
nothing, so the rotated client was refused at its next connection (ledger #918). With scope
flags, the grant is rewritten as for any `client add` (refused without `--force` if it
changes). An enrolment left behind by the old behavior shows in `client list` as
`enrolled, NO bundle`; remove it by fingerprint.

**Every QUIC request is attributed.** The server names each connection's client
`urn:iki:host:gonk:client:<fingerprint>` (`client add` prints it; the fingerprint is the
lowercase hex SHA-256 of the certificate's DER) and mints that name into the
session's capability (`urn:cap:principal:<iri>`, ikigai-core 0.1.93, ledger #1077). The access
log writes it as the `principal` of every QUIC request, reads included; a write that names no
`author` is attributed to it; and a write may name it as its `author`, and no other principal
(`crate::admit`). A client that carries a narrower capability of its own — `ikigai mcp --grant
…`, or an `ikigai serve` forwarding an agent's attenuated grant — is named all the same,
because core's `clamp` keeps the session's principal and never takes one the client carries.
(Through ikigai-quic 0.1.40 the name rode in the capability as an ordinary scope and the
intersection dropped it: every such request logged `principal=-` and its writes recorded no
author, ledger #879. From 0.1.41 to core 0.1.93 it rode beside the capability, as an argument
`ikigai-quic` still stamps.) One cost: a principal is part of the cache key at every depth, so
two clients reading one resource under one grant do not share a cached answer.

**The name follows every host's convention, `urn:iki:host:{name}:client:<fingerprint>`**
(ledger #1183, 2026-10-10), with this host's name, `gonk`. Before that, gonk minted
`urn:iki:gonk:client:<fingerprint>`, and that spelling is a **permanent alias**: never minted
again, never rewritten where it is stored (ledger authors, the access log, `ikigai-log`
segments, whose hash chain a rewrite would break), and always recognized. An `author` in
either spelling is principal-shaped, so a client may name ITSELF either way and can name no
other machine in either; the item page shows both spellings of one fingerprint as one actor,
in the current form. Enrollment is unchanged: `clients.json` is keyed by fingerprint.

What admission means today: a client holding a trusted certificate and an enrolment is
admitted until it is removed (`client remove`, or its entry deleted from `clients.json`),
which takes effect on its next CALL, on the connection it already holds; its certificate stays
trusted at the TLS handshake until a restart. Both files are re-read on every call that arrives
(ledger #1710): a removed grant or enrollment refuses the call, and a narrowed grant answers it
under the narrower authority. A connection only narrows, so a WIDENED grant reaches a client
when its connection is next opened, and the door closes every connection at ten minutes old
(the client reconnects on its next call, unnoticed) so that is never longer than ten minutes.
Through `9d4e722` a standing connection (Hermes's MCP mount, a worker) kept the authority it
connected with until gonk restarted. There is no certificate authority, no expiry and no automated trust
distribution — the server certificate is pinned by copying it.

**`default` applies only to an enrolled client that names no grant** (ledger #1825). A
`"default": "<grant>"` in `clients.json` is the grant for an entry of `clients` that names none
(`{"<fingerprint>": {"label": "spare laptop"}}`); an entry like that in a file with no
`default` is refused with the file. It is never the grant for a certificate with no entry:
removing a client's entry refuses it at its next call and its next connection whatever
`default` says, though its certificate is still trusted at the handshake. Through 0.1.x a
removal under a `default` moved the client to the default grant instead, and a `default` alone
opened the QUIC door; now a `default` alone admits nobody. ⚠ Adding a client still takes a
restart: trusted certificates are read at startup, and the `ikigai-quic` release gonk builds on
has no trust list it can re-read per handshake.

**A remote WORKER** (ledger #1143, ikigai-ledger 0.8.0) is a client that takes work from a
ledger and hands it back, and never decides that the work is done:

```sh
ikigai-gonk client add builder --worker default     # the same as --ledger default=worker
```

The grant is the ledger's published worker grant and nothing more:

```text
urn:cap:ledger:read:default
urn:cap:store:read:graph:urn:iki:ledger:graph:default
urn:cap:ledger:claim:default
urn:cap:ledger:comment:default
urn:cap:store:write:graph:urn:iki:ledger:graph:default
```

It may read items, claim one (`sink urn:iki:ledger:claim item=N`), renew its lease
(`renew=true`), comment (`comment handback=true` hands the item back to its dispatcher, and
`next` stops offering it), and release its claim (`delete urn:iki:ledger:claim item=N`). It may
not close or reopen an item (the dispatcher's verdict, `urn:cap:ledger:close:{L}` or `write`),
file, label, link, defer or edit one: each is a `Denied` naming the grant that would do. A claim
is recorded with the client's principal (`urn:iki:host:gonk:client:<fingerprint>`), so another
worker holding the same five lines cannot release it, whatever holder name it spells.
⚠ The store write token is raw quad authority on a bare kernel; behind this server's doors a raw
`urn:iki:store:graph-update` against a ledger graph is refused without the `--ledger-graph`
grant, which `--worker` never mints (see "Raw store writes"). `tests/worker_1143.rs` drives all of
this through the QUIC door of a real gonk.

**An AUDITOR** (ledger #1611) sees what a ledger deleted and can change nothing:

```sh
ikigai-gonk client add box --ledger default=audit      # passkey invite takes the same flag
```

```text
urn:cap:ledger:read:default
urn:cap:store:read:graph:urn:iki:ledger:graph:default
urn:cap:store:read:graph:urn:iki:ledger:graph:default:deleted
```

It reads the ledger, and the graveyard through the SPARQL faces (`urn:sparql:select
graph=urn:iki:ledger:graph:default:deleted`, or `/sparql` with the same `graph`): each deleted
item's archived content, beside its tombstone in the ledger's own graph. It cannot purge, delete,
file, comment or close, and it cannot restore an item or clear the graveyard by hand: it holds
no write token for either graph. Before it, the graveyard's read token came only with `purge`, so
an auditor had to be able to destroy what it audited. `tests/audit_role_1611.rs` drives it through
the QUIC door of a real gonk.

The ledger roles, and what each adds:

| `--ledger <ledger>=` | holds | adds |
| --- | --- | --- |
| `read` | — | items, an item, `next`, the ledger list |
| `audit` | `read` | READ the graveyard (`urn:iki:ledger:graph:<ledger>:deleted`); writes nothing |
| `worker` | `read` | claim (take, renew, release) and comment, a hand-back included; not close |
| `write` | `worker` | every ledger write: file, close, reopen, label, link, defer, edit |
| `delete` | `write` | Delete an item: its quads move into the graveyard (written, not read) |
| `purge` | `delete` and `audit` | purge an item's content from both graphs, leaving its tombstone |

Two lines combine: `--ledger default=delete --ledger default=audit` deletes and sees the
graveyard, and cannot purge.

`ikigai-gonk grants <ledger> <read|audit|worker|write|delete|purge>` prints the token list for
one ledger, for writing `grants.json` by hand.

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
mount line may claim (a task layer is mounted by binding it, below, and by nothing else): a mount is composed in front of the local spaces and its catalog is
served through every door, so what these doors can serve stays a property of the manifest
plus one namespace. Every resource under the prefix declares the mount's exact net token,
`urn:cap:net:{host}`, beside whatever the peer declares, because that is what the mount checks
on every call: the manifold offers a mounted resource to exactly the callers the mount serves
(ledger #1204; `urn:llm:config`, which declares nothing on the peer, used to be offered to an
anonymous caller and then denied).

**The peer's door must be ikigai 0.1.56 or later** (ledger #1378). From cli 0.1.56 gonk's
mounts are QUIC clients that ping: a PING every 5 s of quiet, and a connection given up after
15 s of silence, so a peer that has gone is found in seconds rather than at the call deadline. A
door older than 0.1.56 reads no packets and sends no ACKs while it resolves a call, so a pinging
client loses any call to it that stays silent longer than 15 s, and a model call often does (the
61 s rollup below). So upgrade the peer first, then this gonk. gonk's own QUIC door answers
pings from the same release (`tests/quic_keepalive_1378.rs`).

**What a derivation costs, and what bounds it.** Each grain has a provider and a `max_tokens`
ceiling (`gonk.explain.*`), and the ceiling is the only bound on one call — a thinking model
with no ceiling burns the budget on reasoning and returns nothing. Over time the bound is the
**archive**: an explanation is keyed by `(path, content-hash, version-tag)` in this dataset
and derived once per content version, so the second read of an unchanged file is a store
read. Measured on a scratch repository against `quic://127.0.0.1:4433`: the file grain
derived in **10.2s** on `qwen3-coder:30b` and came back from the archive in **13ms**; the
directory rollup derived in 61s on a 70B model, because a rollup asks the default tier.

**And over time, one window on the mount** (ledger #308): at most `gonk.mount.calls_per_hour`
calls an hour reach the model, **2,000** unless the config says otherwise, every caller and
every family together. The archive helps once per content version and not at all against a
caller walking new versions, or asking `urn:llm:ask` directly, which is neither archived nor
bounded by the ceilings above; the window bounds both. It counts at the mount because that is
where spend leaves: an explanation, a review pass, a pull request's layers, the judge and a
natural-language draft all reach the model there, and an archive hit never does. A call is
counted when it is invoked, after the kernel has checked the caller's grant, so a caller
without the net grant spends none of it; a `describe` is never counted, and neither is a read
that calls no model (ledger #1576): `urn:llm:config`, `urn:llm:models`, `urn:llm:select` and a
provider's `:model`, `:up` and `:installed` still go through the mount and still need its net
grant, but spend nothing. Every other name under `urn:llm:` counts, a name the peer gains later
included, because a spend ceiling fails closed. That matters from the cli release carrying
ikigai-cli PR 513, which stops caching a mounted answer on this side: browse reads a
provider's `:model` on every explain, so each would otherwise spend a call. Past the window a call
is refused with ``retry after N ms: rate-limited: `urn:iki:gonk:mount:calls` is capped at 2000 per
3600s`` — a typed `Error::RetryAfter` since ikigai-throttle 0.2.0 (ledger #1546; it was a plain
`Error::Endpoint` while core had no typed rate-limit error), which the HTTP door answers `429 Too
Many Requests` with `Retry-After`, and the access log writes `outcome=retry-after`. Why 2,000: the
busiest hour on record is 728 derivations (2026-10-02, the review-value experiments), and a
long file's review is several calls, so the ceiling does not bind the heaviest work this
server has done and does bind a runaway. ⚠ It is a ceiling, not a meter: on a metered peer, set
your own (`0` refuses every call). The banner's `llm` line names it.

A model swap on the peer re-keys the archive by itself: the version tag folds the model
identity `ikigai-browse` resolves through `urn:llm:{provider}:model` at explain time, which
is why gonk sets no model label. A `provider=` argument may name only the tiers this server
already asks with — widening that set would let anyone who may explain point this server at
any backend the peer holds, and that is not a caller's decision.

**Who may spend** is [the doors table](#the-three-doors), and the short form is that an
anonymous HTTP caller holds no browse grant at all, so it can neither derive an explanation
nor read an archived one, while the socket door's root can do both. A signed-in or
certificate identity may derive when its grant was minted with `--browse derive`
([the roles](#provisioning-a-browsing-identity-the---browse-roles)), whose net scope names
this mount's host.

## A ledger bound to a task layer (ledger #1190)

A **layer** is a shared space on a layer host ([`ikigai-layers`](https://github.com/ikigai-rs/ikigai-layers),
served by an `ikigai serve` door): a task layer is a tuplespace where work is offered, taken
atomically, and reported on. One `gonk.layer` line binds one of this server's ledgers to one
task layer, in one role:

```toml
# ours: offer ledger `bobby`'s items into `tasks-bobby`, and claim what is taken
gonk.layer = "tasks-bobby offer quic://ikigai-rs.dev:4434 ~/.config/ikigai/gonk/layers ledger=bobby collab=bobby"
# theirs: take offers from it into their own ledger
gonk.layer = "tasks-bobby take quic://ikigai-rs.dev:4434 ~/.config/ikigai/gonk/layers ledger=ikigai"
```

The certificate directory is this gonk's own identity at the layer door (`client.crt`,
`client.key`, and the `server.crt` it pins); the door's enrollment names the participant it is
(`plasma-gonk` in the edge's `collabs/bobby.toml`). `collab=` names the collaboration whose
participants graph (`urn:collab:{name}:participants`) says whether a taker is a person
(`foaf:Person`), an agent acting for one (`prov:SoftwareAgent`, a collaborator's Claude) or a
machine (`ik:Machine`), which is the KIND of the claim the mirror takes for them: `person`,
`agent` or `machine` (anything else is `machine`, the ledger's default).

The layer host must serve `urn:layers:me` (ikigai-layers 0.2.2 or newer; an `ikigai serve`
door from cli 0.1.55 on): that is how this gonk knows which offers and takes in the layer are
its own. A host that does not is named in every pass that needs it, never guessed around.

**What it mounts.** `urn:layer:{layer}` and everything under `urn:layer:{layer}:`, then
`urn:layers:` and `urn:collab:`, as override mounts on that one door, and nothing else: a
`gonk.mount` line still refuses every prefix but `urn:llm:`. Every answer through them is
uncacheable, because a layer's state changes on the layer host's schedule and golden threads
do not cross a mount. Nothing is dialled at start; a door that is down costs the mirror and
nothing else. Every call through the door runs under a 15-second deadline (a feed read gets its
own wait on top), so a host that accepts a call and never answers fails the pass in seconds,
naming the host, rather than holding it for the transport's five-minute idle timeout.

**What it refuses.** A layer whose model (`ik:layerModel` in its definition) is not
`tuplespace`: messages never touch a ledger. It is read when the binding first runs, and a
wrong model refuses the binding for the life of the process, said once on stderr. Also two
lines for one layer, two offer lines for one ledger, and bindings on two different doors.

**Which ledger an offer line may name (ledger #1745).** An offer publishes the title and body
of every open, unclaimed item of its ledger to another party within one pass, so one wrong word
in the config must not offer the wrong ledger. At start, the server refuses:

- **`ledger=default` in the offer role, always.** The default ledger is this server's own work,
  and its items are not written to be shared.
- **A ledger not named for its collaboration** (`ledger=bobby collab=bobby` is the convention)
  unless the line confirms it by repeating its name: `confirm-ledger=<ledger>`, e.g.
  `… ledger=work collab=bobby confirm-ledger=work`. An offer line with no `collab=` needs it
  too. `confirm-ledger=` on a take line is refused.

When a binding starts, `serve` says how many open, unclaimed items it would offer, before its
first pass offers any.

**What an offer may carry (the content gate, ledger #1745).** Before an offer is dropped, its
title and brief (the item's body) are checked, and an item is NOT offered when either carries a
credential shape (a GitHub token, `ghp_` `gho_` `ghu_` `ghs_` `ghr_` `github_pat_`; an `sk-` API
key; an AWS access key id, `AKIA` `ASIA`; a PEM private key; a `Bearer` token) or matches a line
of the operator's deny file:

```toml
gonk.layer.deny = "~/.config/ikigai/gonk/layer-deny.txt"
```

One pattern per line, matched as a case-insensitive substring; blank lines and `#` lines are
skipped. It is read at start, applies to every offer binding, and a file that is named and
cannot be read stops the server. Keep the words there, not in any public text: they are often
words that must not appear in public at all. A withheld item gets one comment saying why (the
KIND of credential, or the deny file's line number, never the matched text), and is offered once
an edit removes it. This guards against an accident on our side; it matches shapes, so the
dispatcher still decides what a shared ledger holds.

### The offer role: the mirror

A pass is level-triggered and idempotent (twice is once), and runs on every event in the
layer's feed, on every write to the ledger (a cut listener on `urn:iki:ledger:{ledger}:`), and
every five minutes as a backstop:

- **An open, unclaimed item that is not awaiting its dispatcher is offered.** The offer is a
  fixed CONSTRUCT over the ledger's graph naming only the title, the BODY as the brief, and
  one `https://github.com/{owner}/{repo}` the item is `about`; then `urn:jsonld:prune` against
  the layer's own context, and the mirror refuses to publish if prune removed anything; then
  the layer host's closed shapes. Labels, links, comments, numbers and ledger IRIs cannot
  cross. An item with no body is not offered (the pass says why). The offer is recorded on
  the item as an `about` edge to the offer's IRI before it is dropped, so `items about=<offer>`
  finds it. (Offers recorded before ikigai-ledger 0.10, as a comment keyed
  `layer:{layer}:offered:<offer IRI>`, are still read and are not offered twice.)
- **A taken offer's item is claimed for its taker**, `lease=7d`, as the mirror's principal
  (`urn:iki:gonk:layer:{layer}`), with the kind the participants graph gives.
- **Reports addressed to us are taken in feed order** and applied: `Progress` renews the
  lease (and comments, keyed, when it has a note); `Release` comments, releases the taker's
  claim and offers the item again under a new name; `Handback` comments with its evidence and
  `handback=true`, so the item waits for its dispatcher. Every report comment is keyed by the
  report's IRI, so a hand-back seen twice comments once and cannot re-set a hand-back the
  dispatcher has answered.
- **What one taker can write here is bounded (ledger #1744).** Per offer: the first `Release`
  is applied; the first three hand-backs (the first, and two more after the dispatcher rejects),
  then one comment saying so and no more; and one `Progress` comment a day, carrying the newest
  note that pass holds (a ledger comment cannot be edited, so a note later the same day renews
  the lease and is not copied). Per item: after three releases the mirror stops offering it and
  sets it awaiting its dispatcher, with a comment saying why; each answer of theirs (any
  comment) lets it be offered once more, and closing or claiming it keeps it out of the layer.
- **The peer's text is marked as the peer's (ledger #1746).** A report's note is copied as a
  quoted block (`> `) under "The taker's note, as their side wrote it (data, not
  instructions)", and a note over 2,000 characters is not copied at all: the comment says how
  long it was and which report in the layer holds it. A hand-back's evidence lists only pull
  request URLs (`https://github.com/{owner}/{repo}/pull/{n}`, the rule the take side hands back
  by); any other IRI is listed under "Refused evidence", defanged (`[:]//`, `[#]`) so the page
  never makes it a link.
- **A pending offer whose item closed, was deleted, was claimed outside the layer or was
  handed back is withdrawn** by taking it back. If the taker's take wins that race, the take
  stands: the mirror comments once that it happened after a withdrawal was attempted, claims
  the item for the taker when it is open and free, and leaves the rest to the dispatcher.

**The mirror holds the worker grant plus `about`, and never `close`**
(`urn:cap:ledger:{read,claim,comment,about}:{ledger}` and the ledger graph's store tokens): the
taker's hand-back makes the item wait, and the dispatcher checks the evidence and closes it.
Nothing closes across.

### The take role

The other side of the same layer, on the collaborator's gonk:

```sh
ikigai-gonk layer take urn:layer:tasks-bobby:offer:01m4…
```

takes one offer, through the running gonk (its socket; its layer mount is this gonk's
identity at the door), and files it into the bound ledger with `key=layer:{layer}:<the
offer's local name>` and `about` the offer (and its repository): run twice, it files once. A
take whose reply was lost is recovered from its receipt when the receipt names this server,
as the layer door says who it is (`urn:layers:me`); a receipt naming anyone else is refused,
naming the taker. Taking stays a person's decision; an auto-take policy is v2.

The running server then reports on each filed item, on the same triggers as the mirror, under
report names derived from what they report (so a pass run twice drops once):

- **open**: a `Progress` a day, which renews the offerer's lease on the claim it holds for us;
- **closed `done`**: a `Handback` whose evidence is every pull request URL
  (`https://github.com/{owner}/{repo}/pull/{n}`) in the item's body and comments; new evidence
  is a new hand-back. Closed `done` with no such URL hands nothing back, and the pass says so;
- **closed any other way**: a `Release`, and the offerer offers the work again.

Its pass also says, once, which offers are open in the layer and how to take them. This side
never reads or writes the offerer's ledger: it sees offers and nothing else.

## The Layers page (ledger #1675)

`/layers` is where a person reads layer traffic and posts to it, beside the ledgers: every layer
the configured identity reads at its layer door, with what is new since that viewer last looked,
and one layer per page. A channel's newest 50 entries come newest first (who, when, the kind, a
brief's or an issue's title, repository and severity, a file's name, media type and size) with a
form to post a message or a question, optionally in reply to one; a task layer shows its
pending tuples and its recent takes, read-only. An open page updates itself.

```toml
gonk.layers.view = "quic://ikigai-rs.dev:4434 ~/.ikigai/layers-client/brian"
```

One line, read at start: the layer host's door, and the certificate directory of the identity
the page reads and posts AS (`client.crt`, `client.key` and the `server.crt` it pins, as
`ikigai --cert-dir` lays them out). Without it the page says it is not configured and shows
this line.

- **It speaks as that identity and nothing else in gonk can.** Its door is mounted into no
  kernel: no socket, QUIC, script or model request reaches it, and it sends only names under
  `urn:layer:`, `urn:layers:` and `urn:collab:`. A `gonk.layer` binding's certificate
  directory is refused (that is this server's own identity), and the page posts only when the
  door's `urn:layers:me` says the identity is a person. It never offers `out` or `take`, and
  offers `say` only on a channel the identity's grant (read from `urn:layers:me` and each
  collaboration's manifest) holds it on.
- **Who may use it: a signed-in passkey holding the page's own tokens.** Reading (the page, its
  poll, a file) needs `urn:cap:gonk:layers:read`; posting needs `urn:cap:gonk:layers:say`.
  gonk mints neither for an anonymous caller, so put them in the grant of YOUR passkey in
  `grants.json` (the grant `ikigai-gonk passkey invite` named):

  ```json
  { "brian": ["…the scopes it holds…", "urn:cap:gonk:layers:read", "urn:cap:gonk:layers:say"] }
  ```

  The header shows **Layers** to a caller who holds the read token. A post is a `POST` and is
  held to the door's same-origin rule like every write here; a page on another site or
  another localhost port is refused, and its navigations keep neither token.
- **One long poll at the door.** The first page opened starts a follower that holds one
  `urn:layers:feed after=<cursor> wait=50` for every layer the identity follows, on a QUIC
  connection of its own (a door answers one call per connection at a time, so a post must
  not queue behind a 50-second wait), and reads only what an event says is new
  (`entries after=<last read>`; never from 0 on every load). An open page asks gonk's copy
  every 3 seconds (`/layers/~/wait`, answered at once, so an open tab holds none of the
  browser's six connections to gonk between asks) and gets the new entries, a sentence for
  its live region, and the banner when the door's state changed. If an ask fails (a network
  error, gonk restarting) the page says "Reconnecting" once and asks again less often, up to
  a minute apart; after a restart it says to sign in again, since gonk keeps sign-ins in
  memory. An unreachable door is one banner over what was read before.
- **Unread** is per viewer (the signed-in passkey) and per layer: the last feed position that
  viewer was shown, kept in `<data home>/layers-view/cursors.json`. Opening a layer, or a live
  update reaching its open page, marks it read; the overview counts the events after it.
- **A file is downloaded through gonk only after its size and its `sha256:` digest match the
  bytes** (the door does not check them); a mismatch is a `409` and nothing is sent. It is
  served as `application/octet-stream`, never its stated type, under a sandboxing CSP, and the
  link's `download` attribute names the bare file name. ⚠ `Content-Disposition: attachment`
  is not sent yet: the HTTP transport (`ikigai-web` 0.1.63) cannot set a response header from a
  resource; ikigai-rs/ikigai-cli PR 516 adds it, unpublished.
- Bodies are shown as written, the way gonk shows every body (escaped text, links made
  clickable), not rendered as Markdown.
- **A ledger reference in an entry links by context** (ledger #1856). A qualified one
  (`bobby#2`, `bobby #2`, `bobby:#2`) links to `/l/bobby/item/2` when this gonk holds `bobby` and
  the viewer may read it, whoever wrote it; a ledger it does not hold stays text. A bare
  `ledger #N` links to `default` only in an entry from this host's own side: the page's own
  identity, and whoever `gonk.layers.local` names. In anyone else's entry it names THEIR
  ledger, so it stays text, with a title saying so.

  ```toml
  # participants' names (urn:collab:participant:{name}) or principals' IRIs; default: nobody
  # but the page's own identity
  gonk.layers.local = "hub, plasma-gonk"
  ```
- To look at it without a layer door or a passkey, `cargo run --example layers-preview` serves
  the page over a scratch layer host in process (its anonymous caller is handed the two tokens,
  which the real server never does) and prints the URL; `bobby` posts every 30 seconds.

## Reviewing on a git event

A commit can put its changed files in front of the review pass. **The queue is the bound**:
a commit touching forty files drops forty requests, and they come back out one at a time.
Nothing decides a file was not worth reviewing.

```toml
# bind the queue at urn:space:reviews
gonk.review.space = "reviews"
# gonk.review.grant = "reviewer"     # the grant a pass runs under; naming it arms NOTHING
# gonk.review.arm = true             # ⚠ ARM it: review on every drop. See below
# gonk.review.root = "~/.ikigai/spaces"
# gonk.queue.serious = "critical,major"   # what the Queue asks a human about (the default);
                                          # the rest are minted and counted, not queued
# gonk.review.judge = "urn:llm:coder-next:ask"   # the judge on each serious finding a pass
                                          # mints (default urn:llm:coder:ask); "off" for none
# gonk.review.judge_max_tokens = 4000   # the judge's per-call ceiling (default 400)
```

```text
urn:space:{name}              the queue — rd (list/read), out (drop), take (claim, atomic)
urn:iki:gonk:review:pass      one tuple -> one review pass
urn:iki:gonk:review:depth     how deep the queue is, and whether anything is draining it
```

### ⚠⚠ Arming it: two facts, and the interlock is arithmetic

**Nothing is published to gonk except by a human**, and since `ikigai-browse` 0.5.0 that is
a capability fact rather than a policy. A review pass writes **pending findings** at
`urn:iki:finding:{id}` and `review` no longer declares `urn:cap:annotate` at all;
`Sink urn:iki:finding:{id} decision=publish` is the only path into the `urn:iki:annotation:`
family and it still demands the token. So a reviewer holding browse-read and a narrow net
grant **cannot publish** — not "is not supposed to", *cannot*. That is what made an
unattended drainer safe, and it is why arming this is a decision about a grant.

Arming takes **both** of:

```toml
# an authority written into grants.json
gonk.review.grant = "reviewer"
# and the word that says to use it
gonk.review.arm = true
```

`arm` without a usable grant **stops this server**. Naming a grant without `arm` does what
it has always done: it is read, checked and printed on the banner, so an operator can see
the authority before anything uses it.

The grant is exactly:

```json
"reviewer": [
  "urn:cap:browse:read:*",
  "urn:cap:net:127.0.0.1",
  "urn:cap:store:read:graph:urn:iki:browse:graph:default",
  "urn:cap:store:write:graph:urn:iki:browse:graph:default",
  "urn:cap:exec:gh"
]
```

⚠ **Without `urn:cap:annotate`.** A reviewer that may publish is the interlock gone, and
this server refuses to start rather than arm one. The signed-in person's grant is where that
token belongs.

⚠ `urn:cap:net:` takes the **host of the mounted peer**, narrow. `urn:cap:net:*` is the
OFFERING wildcard `ikigai-browse` declares and is refused as a grant — and
`urn:cap:net:localhost` does **not** reach a peer at `127.0.0.1`. The kernel satisfies
browse's wildcard with ANY net grant, so the exact host is checked where the call leaves: the
mount refuses a scoped caller whose grant does not name the host it dials (`mount::net_grant`,
ledger #805), and arming checks the same token, so a reviewer granted for the wrong host stops
the server instead of dead-lettering every pass. Take the host from `gonk.mount`.

★ The list above is not maintained by hand: it is read off the review's own contract on the
running kernel (`trigger::reviewer_grant_shape`), and the startup refusal prints it as JSON
to paste. The previous version of that helper was a list of constants, and it went stale and
silent the day browse 0.5.0 dropped the publish token from `review` — it would have told an
operator to write exactly the grant this design excludes.

### What an armed trigger costs, and what a stuck one looks like

**Passes are serial: one per gonk process.** The reactor reads its filesystem notifications
on one thread and runs each pass inline, so forty dropped files are forty passes back to
back — and because that is a property of a dependency's thread shape rather than a promise,
this server **refuses** a second concurrent pass instead of quietly paying for it.

⚠ **Nothing bounds wall clock.** Forty files at a minute each is forty minutes, and the only
thing that makes that visible is the depth:

```sh
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:gonk:review:depth'
```

and the **live badge on the Queue link** in the header, which polls the same resource every
ten seconds. It reads four ways, and three of them are not a number: not configured, empty,
counted, unreadable.

★ **A queue that is armed and not empty with nothing in flight is STUCK.** `watch()` catches
up at startup and then lives on a thread nothing else observes, so a watcher that dies while
gonk lives drains nothing and says nothing until a restart — and gonk runs no log. A depth
that stops falling is the whole symptom; the readout says `NONE IN FLIGHT` and the badge
turns red. The fix is to restart this server.

★ **A restart in the middle of a pass REQUEUES that request — on an armed server.** The reactor
moves a tuple into `<space>/.processing/` when a pass starts and out when it ends, so a restart
in between leaves it there. Since `ikigai-intray` 0.1.36 a reactor recovers `.processing/` when
it starts, and gonk asks it to move what it finds back into `inbox/` (`Interrupted::Requeue`),
because a review pass is idempotent: a finished one is an archive hit the second time, and an
unfinished one mints only pending findings. The catch-up then reviews it. The banner's `review`
line says what recovery found at this start, or why it was skipped (another live reactor on the
same spaces root holds the lease).

⚠ **An UNARMED server recovers nothing.** It runs no reactor, so a request a stopped armed run
left in `.processing/` stays there until an armed start. The readout counts `.processing/`
(`in_processing` and `lost` in the JSON face) and names a claimed request no pass is running as
lost — saying that its next start requeues it on an armed server, and that NOTHING RECOVERS IT
on an unarmed one — and on an armed server the badge turns red.

### Filling the queue from a hook

```sh
ikigai-gonk review request <repo> <path>
```

opens no store, binds no door and dials nothing: it writes one file into the queue and
exits. It works while gonk is down, which is the property a `post-commit` hook needs — a
commit must never wait on this server, let alone on a model. Dropping a request spends
nothing and needs no grant: **a tuple is a request, not an authority.**

Identical requests collapse. The drop is content-addressed and a request is exactly
`(repo, path)`, so the same file committed three times while the queue waits is one entry —
and the pass reads the file's content hash from the live tree when it runs, so what gets
reviewed is the current content, exactly as it would be for someone clicking **review**.

⚠ That is also why a request carries **no commit SHA**: a SHA would make every drop unique
and the queue would grow rather than collapse. The trade is deliberate.

A `post-commit` hook that asks for a review of every file a commit touched
(`.git/hooks/post-commit`, executable; put the repository's `gonk.browse.root` NAME in `root`):

```sh
#!/bin/sh
# Ask gonk to review every file this commit touched. Never fails the commit.
root=my-repo
git diff-tree --root --no-commit-id --name-only -r HEAD |
  while IFS= read -r f; do
    [ -f "$f" ] || continue        # a deleted path has nothing to review
    ikigai-gonk review request "$root" "$f" >/dev/null || true
  done
exit 0
```

⚠ **`--root` is not optional.** `git diff-tree` compares a commit with its parent, and the
FIRST commit of a repository has none: without `--root` it prints nothing for that commit, and
the hook exits 0 having asked for nothing. That is the commit that adds every file, so the gap
is the whole repository, and nothing says so. `--root` compares a parentless commit with the
empty tree, which lists every file it added. `--no-commit-id` drops the commit's own id from
the output, so only paths reach the loop. A merge commit lists nothing either (diff-tree shows
a merge only when asked how: `-m` lists its files against each parent, so pipe through
`sort -u`). git runs the hook in the repository's top directory, which is where the listed
paths are relative to, and ignores its exit status: the `|| true` and `exit 0` are for a person
reading it. With `gonk.review.arm` unset the requests are kept and nothing drains them, so the
hook is safe to install before a reviewer is armed.

### Draining it, by hand, over the socket

```text
source urn:space:reviews                                # what is waiting
source urn:space:reviews tuple=<id> | urn:iki:gonk:review:pass
delete urn:space:reviews tuple=<id>                     # once its findings are yours
```

⚠ The later stages of a pipeline are **bare IRIs** — `| urn:iki:gonk:review:pass`, not
`| source urn:…`, which fails with `invalid IRI: No scheme found`.

The read is non-destructive, so a pass is spent only when a person asks for it, and the
request stays queued until they say it is done. The queue's three tokens
(`urn:cap:space:{out,read,take}`) are minted by nobody either, so neither network door can
reach it — this is the owner-only socket's work. ⚠ `urn:iki:gonk:review:depth` is the one
reading of this tree a network caller can get, and it is deliberately a different authority:
it declares `urn:cap:browse:read:*`, because a queued request names a file in a browse root
and even a count is a statement about those roots.

**"Is this file already queued?" is a query, not a scan.** A request is Turtle, so the
intray's associative match selects over it:

```text
source urn:space:reviews match="PREFIX ik: <https://ikigai-rs.dev/ns#>
                                ASK { ?s ik:repo \"ikigai-gonk\" ; ik:path \"src/k.rs\" }"
```

returns the ids of the requests that match, and `delete … match=<the same ASK>` claims the
first of them. That is the reason a request is RDF rather than a line of text — a non-RDF
tuple can never be selected by a template.

**The trigger and the button are one call with two causes.** A pass issues
`Source urn:repo:{repo}:review:{path}` with `as=application/json` and nothing else — the
same IRI the **review** button sends, so both land on one archive entry and one set of
minted annotations. `ikigai-browse` holds that from its side
(`the_button_and_a_trigger_are_one_call_with_two_causes`) and `tests/trigger.rs` holds
gonk's. Nothing here assembles a prompt, post-processes a finding, or writes an annotation.

### What this server will not read

`ikigai-intray`'s reactor used to take its authority from a `cap` file beside the space, and
that file **minted** a capability rather than narrowing one — from a directory anything that
can drop a request can also write. Since 0.1.24 gonk supplies the reactor's authority itself
(`with_host_authority`), so the crate never reads that file at all: authority here is a grant
in `grants.json`, checked the same way a certificate's and a passkey's are.

gonk still refuses to start beside a `cap` file, and refuses to **arm** beside one it would
now ignore — under the host seam such a file does nothing, and an operator who wrote one
believes they have bounded a reviewer they have not.

**Nor will it fire what the `handler` file names.** That file sits in the same directory, and
by default the reactor fires whatever IRI it names, under the reviewer's authority. Since
`ikigai-intray` 0.1.41 gonk decides that itself too (`with_host_handler`, ledger #887): the
review space fires `urn:iki:gonk:review:pass` and nothing else, a `handler` file naming any
other IRI gets the tuple REFUSED — dead-lettered into `error/` with a note naming the target,
and a line on the server's stderr — and no other space fires at all. The file is still
written when this server is armed and removed when it is not, as the marker other readers of
the tree use to tell a reactive space from a plain one.

## Scripts: saved queries and plans (ledger #939)

A SPARQL query or a plan, published at a name, is a resource like the ledger is: fetched
without running, run as a READ, run for its EFFECTS with a record of who ran it, and versioned
by its content. gonk mounts [`ikigai-script`](https://github.com/ikigai-rs/ikigai-script) in
the hub, so every door reaches the same scripts, and each run names the principal the door
minted: the passkey at HTTP, the client certificate at QUIC, nobody at the owner's socket.

```text
urn:script:{name}                    Source Exists Sink Delete  fetch WITHOUT running, publish, retire
urn:script:{name}:version:{digest}   Source Exists              one immutable, content-addressed version
urn:script:{name}:compiled           Source                     the head prepared for its evaluator
urn:script:{name}:result             Source                     run it as a READ: its answer, cached
urn:script:{name}:runs               Sink                       run it for EFFECTS: answers the run's IRI
urn:script:{name}:run:{id}           Source Exists              one run: who, which version, under what, when, outcome
urn:script:catalog                   Source                     every script the caller may read
urn:plan:{eval,validate,requires}                               the plan doors a plan script reaches
```

A person who may publish `open-by-label` publishes it over HTTP, signed in; anyone on this
machine then runs it, because it is public:

```sh
ikigai-gonk passkey invite brian --ledger default=write --script open-by-label=publish
```

```text
$ curl -X POST -b 'gonk_session=…' --data-binary @open-by-label.rq \
    'http://localhost:1062/script/open-by-label?language=sparql&public=true'
urn:script:open-by-label:version:sha256:4f0c…

$ curl 'http://localhost:1062/script/open-by-label/result?label=triage&as=text/csv'
number,title
1,first triage item
3,second triage item
```

where `open-by-label.rq` declares its one parameter in its leading comment and names the graph
it reads with `FROM`:

```sparql
# Open items carrying a label, lowest number first.
# @param label xsd:string -- the label an item must carry
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>
SELECT ?number ?title
FROM <urn:iki:ledger:graph:default>
WHERE {
  ?item a ledger:Item ; ledger:status ledger:open ; ledger:label ?label ;
        ledger:number ?number ; dcterms:title ?title .
}
ORDER BY ?number
```

A plan (`language=plan`, an `ik:Process` graph in Turtle) composes resources instead of
querying the store: `tests/scripts_939.rs` holds one (`TRIAGE`) that joins
`urn:iki:ledger:items labels=<label>` and `urn:iki:ledger:next`. It is validated against the
vocabulary's shapes before it is stored, and what it needs is derived from its steps'
contracts. A query's parameter values are bound into the parsed algebra, never spliced into the
text; see the library's README for the parameter grammar, the faces (`as=`), and the rules a
query's dataset must follow.

### Who may do what

| | anonymous HTTP | signed-in HTTP | QUIC (a machine) | socket (the owner) |
| --- | --- | --- | --- | --- |
| run and read a PUBLIC script | **yes** (`urn:cap:script:{run,read}:public`) | yes | only if granted (`--script public=run`) | yes |
| run and read a named script | no | per grant (`--script <name>=run`) | per grant | yes |
| publish, replace, retire | no | per grant (`--script <name>=publish`) | **no**: drafts only (`state=draft`) | yes |
| see a draft | no | its own | its own | every draft |
| what a run may touch | the least of: the runner's grant, what the publisher held at publish, and the ceiling below | | | |

- **A script grant names one script, or a namespace of them.** Since ikigai-script 0.3.0
  `urn:cap:script:{act}:{name}` covers one script and `urn:cap:script:{act}:{namespace}-*` every
  name beginning `{namespace}-` (`team-*` covers `team-report` and `team-a-report`, never
  `teammate` or `team`); an exclusion (`urn:cap:script:{act}:-{name}`) takes one back out. There
  is still no "publish anything" grant below root: `urn:cap:script:{act}:*` grants nothing. Grant
  the name or the namespace first (`--script <name>=<read|run|publish>` or
  `--script team-*=publish`, on `passkey invite` and `client add`). `public` is the one name that
  is not a script: `--script public=run` is the pair the anonymous caller holds, every script
  marked `public=true`.
- **A publish declares nothing; the text derives it.** A query needs the store's read token
  for each graph it names, and a plan whatever its steps' contracts require. **A publisher must
  hold all of it**, or the publish is refused naming the tokens: a script can never run with
  more than its publisher held. Reading a graph you cannot read by publishing a script that
  does is the case this exists for.
- **A machine drafts, a person publishes.** The QUIC door is the one whose callers are
  machines (a certificate, not a passkey), and it refuses a publish or a retire whatever the
  grant says: an agent may save a draft, and a person publishes it. One grant table serves
  both identity doors, so the rule lives at the door.
- **Drafts are their author's.** Every door here names its caller, so an author sees their own
  draft, and to anyone else holding the grant it is absent (`404`, exactly as a name nobody
  wrote). The socket names nobody: the owner there is root and sees every draft, and a narrowed
  socket client (`ikigai mcp --grant …`) is recorded as `urn:script:principal:unstamped`, whose
  drafts only root sees.
- **The record names the door's principal.** `urn:script:{name}:run:{id}` (`as=application/json`
  or `text/turtle`, PROV-O) carries the passkey's IRI, the certificate's
  `urn:iki:host:gonk:client:<fingerprint>`, or `unstamped` from the socket.

### The ceiling: the ledgers' reads, and nothing else

Every run is held to `urn:cap:ledger:read:*` and `urn:cap:store:read:graph:urn:iki:ledger:graph:*`
(`script::CEILING`). **Scripts here are read-only**, and a script that writes is refused when it
is published: a SPARQL UPDATE, or a plan with a `Sink` or `Delete` step.

The ceiling also names the EXACT read members of every ledger this server holds
(`urn:cap:ledger:read:{L}` and `urn:cap:store:read:graph:urn:iki:ledger:graph:{L}`,
`script::ceiling_over`; ledger #1220). A runner holding exact grants does not need them. The
owner does: root holds no exact grant, and a plan step declares only a family (`next` declares
`urn:cap:store:read:graph:*`, and the ledger's name is not in its IRI), so a plan the owner
published and ran over the socket was refused, `capability does not grant
urn:cap:store:read:graph:*`. The list is read from the store's graph list before each root run, so
a ledger filed since the last run is in it. `tests/plan_ceiling_1220.rs` runs a root-published,
root-run one-step plan over `urn:iki:ledger:library:next`, and over a ledger filed after startup.

⚠ **Why not "writes through the ledger's own doors".** `ikigai-ledger` writes every append and
comment as a store update under the CALLER's capability, so the ledger's write door needs the
ledger graph's store write token, the same token a raw update of that graph needs. A ceiling that
admits one admits the other. gonk tells them apart where a request ARRIVES (the raw-write and
author rules below), and a script's steps never arrive anywhere. For the same reason **a plan
that writes runs only as root** at `urn:plan:eval`, at every depth: below root a plan step could
write a ledger graph raw, or name another person as a comment's `author`, past both rules. A
read-only plan runs for anyone, under their own capability, which is exactly the requests they
could make one at a time.

### What else is decided here

- **Languages: SPARQL and plans.** No Lisp evaluator is bound, so `language=lisp` (the
  library's default when none is given) is refused at publish. `urn:script:eval` is the
  library's ad hoc door for Lisp and is the owner's here; a plan typed ad hoc runs at
  `urn:plan:eval`.
- **The validator is served to no caller.** Plans are validated by `urn:shacl:validate`, which
  is linked for the plan doors alone, and every door answers it as unbound. Since ledger #1176
  it is ikigai-shacl's subset validator (the `rudof` feature off): it covers every construct in
  the vocabulary's shapes, refuses any other by name, and recurses over neither graph. The rule
  predates that and stays: a caller's shapes are not the vocabulary's, and a build that does
  compile rudof (the test suite, through a dev-dependency) has a validator that can overflow
  its thread on deep enough DATA, which would abort the process holding the store.
- **The transports' own arguments are not a script's.** `ikigai-web` stamps a write with
  `received`, `client`, `principal` and `content-type`, and `ikigai-quic` stamps `principal` on
  everything. Since ikigai-script 0.3.0 a run ignores those four names and a script may not
  declare a parameter by one of them (refused at publish), so they pass through gonk's doors
  untouched; through 0.2.x a script refused them as undeclared and gonk stripped them at the
  door (ledger #1196). Who a run is for is the capability's principal, never an argument.
- **A result is cached in the hub**, under the store's write threads (and the script's own,
  which a republish cuts), so the same public read from a thousand callers is computed once.
  Measured in process: a cold `open-by-label:result` in about 1.2 ms, the next in about 30 µs.
- **An anonymous run is bounded in time and in rate** (ledger #1175). Every SPARQL evaluation
  it makes takes the anonymous door's budget (`gonk.http.anonymous_sparql_budget_ms`, 1 s by
  default), as the anonymous caller's own SPARQL does; a signed-in run gets the store's 5 s base.
  The marker the door puts on an anonymous capability does not survive into the run (its
  capability is attenuated to what the script derives), so gonk's wrapper around the run carries
  the budget on the task, and the store's overlay stamps every evaluation made within it, a plan
  script's ledger reads included. And anonymous runs, all of them together, are limited to **30
  a minute** (`gonk.script.anonymous_runs_per_minute`, `0` for none; one `ikigai-throttle`
  `RateLimit` window, `budget::AnonymousRuns`), refused past it with the time to retry; an
  answer from the hub's cache is not counted, and reading a script's source is not a run.
  Before this, an anonymous run's query took the store's base and could be sent without limit.
  ★ **What the refusal looks like** (ledger #1273, #1216 part 1, fixed with ikigai-throttle 0.2.0
  and ikigai-web 0.1.66): over HTTP it is `429 Too Many Requests` with `Retry-After: N` (whole
  seconds, rounded up, never 0), body ``retry after N ms: rate-limited:
  `urn:iki:gonk:anonymous:runs` is capped at 30 per 60s``, and the access log line says
  `outcome=retry-after`. The window refuses with core 0.1.108's typed `Error::RetryAfter` (class
  `retry-after`), which the door's error table maps to 429 and the header. Until then it was a
  `500 Internal Server Error` with no header: `ikigai-throttle` refused with a plain
  `Error::Endpoint` because core had no typed rate-limit error, and gonk could not say otherwise
  alone. ⚠ A `RetryAfter` is not transient: nothing in gonk re-issues a refused run; the client
  owns the wait the header names. The QUIC door and the socket never meet this window: their
  callers are never anonymous (a certificate, the owner), so nothing there is counted or refused
  by it.
- **An ad hoc plan and a draft's dry run are runs too** (ledger #1210). An anonymous
  `urn:plan:eval` keeps the caller's capability, marker and all, but its steps are sub-requests
  the hub's front door never stamps, so a read-only plan whose step sent its own query to
  `urn:iki:store:graph-select` ran it at the store's 5 s base, and plans were not counted:
  measured through a scratch gonk's HTTP door, an expensive query took 5.02 s through a plan
  against 1.01 s sent straight to the store, and 45 plans in 0.9 s all passed. The dry run of
  `urn:nl:sparql:check` (reachable anonymously as `/k?c=source urn:nl:sparql:check content=…`)
  was the same hole: 5.05 s, 35 of 35 passed. Both now enter the same window as a script's run
  and run within the door's budget.
- **The atoms are plain files** in `~/.ikigai/scripts` (`gonk.script.dir`), the library's
  `DirBackend`: one directory per script, a compare-and-set head, digest-checked versions, a
  file per run. Not in the RDF store, because the records ARE authority (a version stores what
  its publisher held) and a store graph is reachable by the per-graph grants this server mints.
  So the backup takes them separately, beside the store's archive, and restores and verifies
  them on their own ([Backup and restore](#backup-and-restore), ledger #1175).
- **Not built:** triggers (ledger #937), a script as an endpoint, a stepper, signed elevation,
  and ledger writes from a script (above).

## Natural language: a query drafted from a sentence (ledger #1138)

gonk mounts [`ikigai-nl`](https://github.com/ikigai-rs/ikigai-nl) beside the scripts, so a
person can ask for a query in a sentence and get a DRAFT script back. The model is a
**drafter, never an actor**: every step runs under the caller's own grant, the draft is
checked mechanically before it is kept, and it is saved as `state=draft`, which only its author
sees until a person publishes it like any other script.

```text
urn:nl:grounding        Source   what a drafter may know, under the CALLER's grant: the actions it
                                 may take, the ik: vocabulary, the shape of each graph it may read,
                                 its published scripts as worked examples
urn:nl:sparql:check     Source   a SPARQL text, checked as a draft is: parsed, its derived grant
                                 against the caller's, its predicates and classes against the
                                 graphs, a dry run under a small LIMIT
urn:nl:prompt:{name}    Source   the drafting prompts (sparql, sparql-repair), as templates
urn:nl:sparql           Sink     ground, draft through the mounted model, check, repair (at most 3
                                 attempts), save a DRAFT script with its provenance
urn:ikigai:vocab        Source   the ik: vocabulary the grounding states
```

```sh
ikigai-gonk passkey invite brian --ledger default=write --nl draft --script titles=publish
```

```text
$ curl -X POST -b 'gonk_session=…' --data-binary 'the titles of the open items, by number' \
    'http://localhost:1062/nl/sparql?save=titles'
urn:script:titles:version:sha256:… (a DRAFT of urn:script:titles: review it, then publish it with state=published)

PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
…
preview (LIMIT 5):
?number	?title
…

$ curl -b 'gonk_session=…' 'http://localhost:1062/script/titles'            # review it
$ curl -X POST -b 'gonk_session=…' --data-binary @titles.rq \
    'http://localhost:1062/script/titles?language=sparql'                    # publish it
$ curl -b 'gonk_session=…' 'http://localhost:1062/script/titles/result'     # run it
```

The body of the POST is the ask (`ask=` works too). `focus=<iri>` focuses the draft on one graph
(`graph=` is a deprecated alias in `ikigai-nl` 0.3.0, kept for one release),
`as=application/json` answers every attempt with what the check said of it, and
`as=text/turtle` the drafting's PROV-O. The saved text opens with that provenance as comment
lines (the ask, each attempt and its model, the grounding's and the prompts' digests), so the
draft carries where it came from into the version a person publishes.

### Who may do what

| | anonymous HTTP | signed-in HTTP | QUIC (a machine) | socket (the owner) |
| --- | --- | --- | --- | --- |
| `urn:nl:grounding`, `urn:nl:sparql:check`, the prompts | **yes**, under its own grant | yes | yes | yes |
| draft (`urn:nl:sparql`) | **no**: refused before any model call | with `--nl draft` | with `--nl draft` | yes |
| where a draft is saved | | a `--script <name>=publish` name or namespace | the same, and a machine's draft is its own (only the owner sees it) | anywhere |

- **Drafting spends the mounted model, so it takes the grant that reaches it:**
  `urn:cap:net:{host}` for the host the `gonk.mount` line dials. `--nl draft` mints exactly
  that token, on `passkey invite` and `client add`, and is refused on a server with no mount.
  gonk DECLARES it on `urn:nl:sparql` (`nl::DraftGuard`), so the kernel refuses a caller
  without it before the grounding is computed or the model is asked, and the manifold offers
  the drafter to exactly the callers who may draft. ⚠ It is the same token `--browse derive`
  carries, and it reaches `urn:llm:*` directly through every door, as derive's does: the
  authority is "may spend the mounted model". No anonymous grant carries it.
- **What a draft can READ is the caller's own grant.** The grounding lists only graphs the
  caller may read (a graph it may not read contributes nothing, not even its name), and the
  check's dry run reads through the store's graph-scoped door under the same grant. Drafting
  widens nothing: the anonymous grounding is what anonymous could already read.
- **Where a draft is SAVED is a script grant, by name or namespace.** Name the draft with
  `save=` and hold that name's publish grant, or hold a namespace: `ikigai-nl`'s default name is
  `nl-sparql-<digest>`, which no per-name grant can know ahead of time and
  `--script nl-sparql-*=publish` covers (ikigai-script 0.3.0). Without a grant covering the name
  the valid draft is answered **unsaved**, the query in the answer, for
  the person to publish by hand under a name they hold. A published script is never replaced
  by a draft: that is refused before any model is asked.
- **A machine drafts, a person publishes**, as for every script here: the drafter only ever
  saves `state=draft`. A machine's draft is the machine's alone, so only the owner's socket
  sees it to publish it.
- **No mount, no drafter.** Without a `gonk.mount` line the grounding, the check and the prompts
  are bound and `urn:nl:sparql` is not, the way no explain is bound without a peer.

### The bounds

- **The spend: at most 3 model calls per ask** (`ikigai-nl`'s default `max_attempts`): the
  first draft, then a repair for each refusal, each repair prompt carrying exactly what the
  check refused. Drafting also stops the first time the model repeats a draft, and says so:
  `drafting stopped: the model repeated attempt N`, the repeat marked as one (`repeatOf`,
  `prov:alternateOf`) rather than counted as a failure (`ikigai-nl` 0.4.0). A draft that
  never validates is answered as **failed**, every attempt listed, no query, nothing saved.
- **The spend per call: 2048 tokens a draft** (`ikigai-nl`'s `DEFAULT_MAX_TOKENS`, sent as
  `max_tokens=` since `ikigai-nl` 0.2.0, ledger [#1203](http://localhost:1060/l/default/item/1203);
  gonk takes the library's default). A drafted query is well under a thousand tokens, so the
  bound is about time: a model that runs past it is cut off, its answer fails the check, and the
  attempt is repaired or answered as failed. A reasoning model can still spend the whole bound
  thinking and return nothing, so a non-reasoning model is the better `gonk.nl.ask`.
- **The grounding's size: it fits, measured on a ledger the size of the live one.** `ikigai-nl`
  refuses a grounding over 1 MiB (never truncates it). `tests/nl_1138.rs` seeds 750 items and
  composes browse as the live gonk does; the ledger graph's SHAPE is bounded by its partitions
  and samples, not its item count, so what grows a grounding is the caller's manifold. Release
  build, in process, `ikigai-nl` 0.3.0, the WHOLE grounding (`urn:nl:grounding`, every part)
  over one browse root:

  | caller | no focus, JSON | no focus, Turtle | `focus=urn:iki:ledger:graph:default` |
  | --- | --- | --- | --- |
  | anonymous | 276 KB | 292 KB | 5.5 KB |
  | a person (`--ledger default=write --nl draft`) | 286 KB | 304 KB | 5.5 KB |
  | the same with `--browse read` | 383 KB | 392 KB | 5.5 KB |
  | root (the socket) | 456 KB | 482 KB | 5.5 KB |

  ⚠ The manifold part grows by about 100 KB per browse root, so the whole unfocused grounding
  does NOT fit on a gonk with the live root count: over 51 roots it was 4,886,597 bytes as JSON
  on `ikigai-nl` 0.2.1, whose drafter read it whole and so refused every unfocused draft. **A
  draft no longer reads it**: since 0.3.0 the drafter and the check read only the vocabulary,
  the graphs and the examples (ledger [#1243](http://localhost:1060/l/default/item/1243)): 109 KB
  as root over 51 roots, and
  `tests/nl_1138.rs::an_unfocused_draft_over_the_live_root_count_fits_the_bound` drafts unfocused
  over 51 roots. A direct unfocused read of `urn:nl:grounding` there still needs `focus=` or
  `parts=`. The whole grounding is
  computed in about 20 ms cold; the parts are cached, but the grounding itself is recomputed on
  every read (about 2 to 3 ms for JSON, 9 to 15 ms for Turtle) because its examples come from
  `urn:script:catalog`, which is live (the library's README says why).

### What else is decided here

- **Which model drafts:** `gonk.nl.ask` (default `urn:llm:ask`, the peer's default backend), a
  door under `urn:llm:` like every provider here.
- **One deadline for a whole draft** (ledger #1203): a draft makes up to three model calls (a
  draft, a repair, an escalation), and all of them together get **300 s**
  (`nl::DRAFT_DEADLINE`), the bound the QUIC door's transport puts on ONE call
  (`mount::CALL_TIMEOUT`). Each call runs under what is left, and a call that would start after
  it is refused as a timeout. It matters most at the HTTP door, which has no request deadline of
  its own: before this a draft there could hold its request for three times 300 s. The bound
  rides the request's task into the mount (`mount::within`), because a mounted call blocks its
  thread and a deadline on the future could not interrupt it.
- **Worked examples are the published SPARQL scripts the caller may read.** Publish a few
  good queries and drafting improves for everyone who can read them.
- **Nothing new is linked for the network.** `ikigai-nl` has no model client: the model is the
  mount, so the outbound surface is unchanged.
- **Proven with a stub model.** `tests/nl_1138.rs` runs the whole story through a real child
  gonk with a deterministic `urn:llm:` peer on a socket: a sentence to a saved draft, a bad
  first draft repaired, a hopeless one refused at the bound, the draft hidden from another
  person, published and run, anonymous refused with the model never called, and a QUIC
  machine drafting. A real model drafts differently; the check and the repair are the part
  that does not.
- **A draft's calls count against the mount's hourly window** (`gonk.mount.calls_per_hour`,
  ledger #308) with every other call that reaches the model.
- **Not built:** `urn:nl:script` (a plan, then Lisp; ledger #959) and a page for drafting (it
  is the mechanical route today).

## Named views (ledger #2006)

A **view** is a name under `urn:iki:gonk:view:` that forwards to a name gonk already serves: a
ledger listing, a saved script's `urn:script:{name}:result`, a SPARQL view. Views are DATA: an
operator declares, changes and removes them while gonk runs, and nothing restarts.

| resource | verbs | |
| --- | --- | --- |
| `urn:iki:gonk:views:declaration` | Source · Sink | the declaration, a graph in the store of the same name: Turtle in the `ik:` arrangement vocabulary |
| `urn:iki:gonk:views:check` | Source | the rule a declaration must pass (a SPARQL ASK; true refuses it) |
| `urn:iki:space:dynamic:views:status` | Source | which declaration is in force, how many have bound, and the last refusal and why |
| `urn:iki:gonk:view:{name}` | any | a view: answers what its target answers |

A declaration is one `ik:Alias` over an empty `ik:EndpointSpace`, one `ik:RewriteRule` per view,
each marked `ik:resolveAt "root"` (core 0.1.110): the target is resolved from the hub's root
under the CALLER's capability. A `"prefix"` rule maps a whole namespace of views at once.

```turtle
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

<urn:ikigai:space:_:1> a ik:Alias ;
    ik:maxHops 8 ;
    ik:rewrites <urn:ikigai:space:_:1:rule:1>, <urn:ikigai:space:_:1:rule:2> ;
    ik:space <urn:ikigai:space:_:2> .

<urn:ikigai:space:_:1:rule:1> a ik:RewriteRule ;
    ik:ruleKind "exact" ;
    ik:logical "urn:iki:gonk:view:next" ;
    ik:canonical "urn:iki:ledger:default:next" ;
    ik:resolveAt "root" .

<urn:ikigai:space:_:1:rule:2> a ik:RewriteRule ;
    ik:ruleKind "exact" ;
    ik:logical "urn:iki:gonk:view:open" ;
    ik:canonical "urn:iki:ledger:default:items" ;
    ik:resolveAt "root" .

<urn:ikigai:space:_:2> a ik:EndpointSpace ;
    ik:doors rdf:nil .
```

**Add, change or remove a view** by writing the WHOLE declaration again, through its own Sink
(over the owner's socket, or as a QUIC client holding the write token):

```sh
cat views.ttl | ikigai --mount "urn:gk:=$HOME/.ikigai/gonk.sock" -c 'sink urn:gk:iki:gonk:views:declaration'
ikigai --mount "urn:gk:=$HOME/.ikigai/gonk.sock" -c 'source urn:gk:iki:space:dynamic:views:status'
ikigai --mount "urn:gk:=$HOME/.ikigai/gonk.sock" -c 'source urn:gk:iki:gonk:view:open state=open'
```

To remove one view, leave its rule out; to remove them all, declare the alias with no
`ik:rewrites`. `source urn:gk:iki:gonk:views:declaration` reads back what is declared (as
N-Triples). Over HTTP a view is `/view/{name}` (`/view/open?state=open`): the arguments are
the caller's, forwarded unchanged, so a view names a TARGET, not a query. HTTP negotiates
`Accept` against that target's faces, including quality values; an explicit `?as=` wins.
A browser visiting a ledger listing view gets readable plain text, as with other non-HTML
resources. If none of the target's faces is acceptable, the response is `406 Not Acceptable`.
A view onto a saved script (`urn:script:{name}:result`) is runnable anonymously only if the
script is published `public=true`: a view grants nothing, so an anonymous `/view/{name}` onto
a named script is refused with `urn:cap:script:run:{name}`, as running the script directly
would be. Republish with `sink urn:gk:script:{name} language=… public=true` and the script's
source piped in. A view cannot target an HTML page such as `/l/{ledger}`: those pages live in
the HTTP door's own kernel, and a view resolves from the hub's root.

What holds, and what each costs:

- **The authority to change the views is the write token for their graph**,
  `urn:cap:store:write:graph:urn:iki:gonk:views:declaration`, and nothing else: no new
  capability. A ledger writer is refused, by the declaration's own contract and by the store's
  `urn:iki:store:graph-update` beneath it. Grant that token to a QUIC client and it may change
  the views; reading the declaration takes `urn:cap:store:read:graph:urn:iki:gonk:views:declaration`.
- **A view grants nothing.** Its target is read under the caller's own capability, so a
  caller who may not read the target is refused exactly as if it had asked for the target.
- **A view never shadows a name gonk serves.** The views space is the LAST member of the hub,
  so a name anything else answers never reaches it; and the check refuses a declaration that
  names anything outside `urn:iki:gonk:view:` at all.
- **A refused declaration changes nothing**: the views in force keep answering, and the
  status names the reason (the check, a malformed declaration, an unknown endpoint).
- **Validate before bind, fail closed.** gonk starts with an empty views space; with no
  declaration the status says so ("no views are declared"), and gonk serves everything else
  as before.
- **Every change cuts every cached view** (version one's coarse cut, core ledger #2034), and a
  write to a view's target invalidates that view.
- ⚠ **Write through `urn:iki:gonk:views:declaration`, never `urn:iki:store:graph-update`
  directly.** Both reach the same graph under the same token, but only the declaration's own
  Sink cuts the thread the rebuild listens on: a raw graph write is heard at the next
  declaration write or the next start.
- ⚠ **No composed views yet.** A door in a declaration names an endpoint and a pattern and
  cannot fix an argument, so nothing gonk links composes anything an alias does not; the
  views' endpoint registry is empty. A composition with fixed arguments is a saved script, and
  a view onto its `urn:script:{name}:result` is the composed view.
- ⚠ **A view is invisible to discovery** (ledger #2065): `urn:kernel:catalog` and `/act` do
  not list view names, and a view's `Meta` describes core's forwarder.
- The rebuild runs under the host's root capability (ledger #2048: a narrowed listener does
  not hear a declaration it could not cache). `tests/views_2006.rs` holds all of it.

### The Views page (ledger #2099)

The header's **Views**, beside Layers, is `/views`: every view IN FORCE (its name, exact or
prefix, its target and where it resolves), each exact name a link to `/view/{name}`, and the
status door's state: the declaration in force, when it bound, the last refusal and why, and
whether the declaration in the store is the one in force. The header offers it to a caller whose
grant reads the declaration (`urn:cap:store:read:graph:urn:iki:gonk:views:declaration`); anyone
else is not shown the link, and `/views` answers them a `403` naming that token.

With the write token as well (`urn:cap:store:write:graph:urn:iki:gonk:views:declaration`) the
page has a form per view (its name after `urn:iki:gonk:view:`, its target, exact or prefix; Save
and Remove) and one to add a view. Without it the page has no forms at all, and a post made
anyway is refused by the declaration's own contract and the store beneath it. Neither token is
in any grant the commands write: add them to a passkey's grant in `gonk/grants.json` by hand.

- **A save rewrites the WHOLE declaration**, built by core (`Topology::to_turtle`) from the
  views in force with one added, changed or removed, and writes it through the declaration's own
  Sink under your capability. Then it waits (up to five seconds) for the views space to report
  that declaration as in force or refused, and the page it answers with says which and why.
- **A refused declaration changes nothing.** The page lists what is in force, so after a refusal
  it still lists the old views and says the stored declaration was refused, and why; the next
  save starts from the views in force and replaces the refused text.
- **A form carries the digest of the declaration it was drawn from.** If the declaration has
  changed since (another tab, another person, the socket), nothing is written and the page says
  so.
- **The page never edits what it cannot show.** An arrangement other than one alias over an empty
  endpoint space (written over the socket) is listed, and the page says to change it there.
- Through the page every name is under `urn:iki:gonk:view:`, so gonk's check cannot refuse a
  save; a refusal the page reports comes from core (a declaration it cannot build).
  `tests/views_page_2099.rs` holds all of it.

## The three doors

| door | reaches it | runs under |
| --- | --- | --- |
| **HTTP**, `127.0.0.1:1062` | any process on this machine | anonymously, the READ tokens of the ledgers in `gonk.http.ledger` (default: `default`; never write, ledger #1687) and the run and read of PUBLIC scripts; signed in, that plus the passkey's grant; nothing for a non-loopback peer, a foreign `Host`, or a cross-site write |
| **socket**, `~/.ikigai/gonk.sock` | this user only (`0600`, peer UID checked) | root — the socket's owner can already read the dataset's files — or whatever an owner process narrowed itself to (`cap seal`, `ikigai mcp --grant`), which the transport passes through. One admission rule here, the raw-write rule below; no `Host`/cross-site checks and no author rules |
| **QUIC**, `udp 0.0.0.0:1062` | a certificate under `gonk/quic/clients/` | the grant its fingerprint maps to in `gonk/clients.json`; refused when it maps to none. Every request is named `urn:iki:host:gonk:client:<fingerprint>` |

**Anonymously, the HTTP door is loopback only and grants little.** It can list, file,
comment, close, claim, label and link in its ledgers, and run and read a script its publisher
marked public. It cannot delete or purge, cannot reach another ledger, cannot publish a script,
and cannot touch the store's whole-dataset doors (`urn:iki:store:select`,
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
| `urn:iki:store:graph-*` over the BROWSE graph (`urn:cap:store:read:graph:urn:iki:browse:graph:default`) | **no** | only if the grant names it — `--browse read`, or `--browse-graph read` alone | yes (root) | only if the grant names it |
| `urn:iki:store:select` and the other broad doors (`urn:cap:store:read`) | **no** | **no** — this server hands the broad tokens to nobody | yes (root) | **no** — refused in `grants.json` |
| RAW writes to a LEDGER graph — `urn:iki:store:graph-update` naming `urn:iki:ledger:graph:*` (`urn:cap:gonk:raw-write:graph:<graph>`, ledger #878) | **no** — `403`, though its ledgers' own writes work | only if the grant names it — `--ledger-graph <ledger>` | yes (root) | only if the grant names it — `--ledger-graph <ledger>` |
| `urn:sparql:{select,ask,construct,describe}` (`urn:cap:store:read:graph:*`, then each graph's token) — by default the UNION of the graphs the caller may read | its ledgers' graphs | + the graphs the passkey's grant names | every named graph (root) | the graphs the grant names |
| `urn:script:{name}:result` and `:runs` — run a script ([scripts](#scripts-saved-queries-and-plans-ledger-939)); a run touches at most the runner's grant ∩ what the publisher held ∩ the ledgers' reads | **PUBLIC scripts only** (`urn:cap:script:run:public`) | + each name the grant names (`--script <name>=run`) | yes (root) | only the names the grant names, or `public=run` |
| `urn:script:{name}` Sink and Delete — publish, replace, retire (`urn:cap:script:{write,delete}:{name}`) | **no** | each name the grant names (`--script <name>=publish`) | yes (root) | **drafts only** (`state=draft`): a machine drafts, a person publishes |
| `urn:plan:eval` — a plan typed ad hoc, run under the caller's own capability | read-only plans | read-only plans | any plan (root) | read-only plans |
| `urn:shacl:validate` — linked for the plan doors | **no** — not served | **no** | **no** | **no** |
| `urn:repo:{root}:{explain,review}`, `pr:{n}:{explain,review}` — **spends model tokens** (`urn:cap:net:{host}`; ⚠ the two reviews no longer require `urn:cap:annotate` — browse 0.5.0 — so a pass writes pending findings and cannot publish) | **no** | only if the grant names it — `--browse derive` | yes (root) | only if the grant names it |
| `urn:llm:*` on the mounted peer (`urn:cap:net:{host}`) | **no** | only if the grant names it | yes (root) | only if the grant names it |

**Deriving is the privileged act, and it is one capability away from every door.** Explaining
a file, reviewing one, or explaining a pull request calls a model on the mounted peer, which
costs the operator tokens and — where the peer is metered — money. `ikigai-browse` declares
that as `urn:cap:net:*` (the offering wildcard) on every derivation, and `declared =
enforced`, so a caller with no net grant is refused before dispatch and never appears in
front of a model. **This server mints exactly one net grant**: `--browse derive` on
`client add` and `passkey invite`, which names the host of this server's own `gonk.mount`
(`urn:cap:net:127.0.0.1` for a peer at `quic://127.0.0.1:4433`) and always comes with
`urn:cap:annotate` — see [the roles](#provisioning-a-browsing-identity-the---browse-roles).
The wildcard itself is refused in `grants.json`, like `urn:cap:exec:*`. ★ And the host is
**enforced**, not only named: the mount refuses any scoped caller whose grant does not hold
`urn:cap:net:<the host it dials>` exactly, so a hand-written grant for another host (or one
written before `gonk.mount` moved) is refused with the token it lacks rather than reaching
the peer through the kernel's prefix match on the wildcard (ledger #805).

⚠ **A net grant is the authority to spend that peer's inference, not only to explain.** The
mount serves the peer's whole `urn:llm:` namespace through every door, so the same grant that
derives an explanation can `source urn:llm:ask` directly — and a direct ask is neither
archived nor bounded by the `gonk.explain.*` ceilings, because those are arguments this
server passes to an explain, not a policy the mount enforces. What bounds it is the peer's
own ceiling (`ikigai serve quic://… --cap urn:cap:net:localhost` grants inference and nothing
else) and the fact that reaching the mount at all takes a grant nothing here mints.

★ **The browse-graph row is new, and it is the one thing the graph decision bought.** Until
2026-09-16 browse's quads lived in the store's DEFAULT graph, which has no IRI — so no
`urn:cap:store:read:graph:` token could name them and every query over an annotation or an
archived explanation needed root. They now live in `urn:iki:browse:graph:default`, and that
token is mintable:

```
ikigai-gonk grants --browse-graph read      # print the token
ikigai-gonk passkey invite reader --browse-graph read
ikigai-gonk client add box --ledger default=read --browse-graph read
```

What it carries is **quads in one graph** — every annotation, every archived explanation,
every review finding, through `urn:iki:store:graph-{select,ask,construct,describe}`. That is
the archive **without the spend**: reading an explanation through `urn:repo:{root}:explain`
requires the `urn:cap:net:*` that deriving one does, and this route requires none. What it
does not carry is the browse family: no file contents, no tree, no `gh`, no deriving, and no
other graph. ⚠ And it is not given to the anonymous HTTP caller, whose grant stays exactly
`gonk.http.ledger`'s ledgers — signing in is how the HTTP door spells "a caller who may".

★ **And since `ikigai-store` 0.2.5 the two read tokens together RUN THE LEDGER↔BROWSE JOIN** —
the query the graph decision was for, below root at last. A scoped read takes a SET of graphs:

```
ikigai-gonk client add box --ledger default=read --browse-graph read
```

```sparql
# urn:iki:store:graph-select, graph="urn:iki:ledger:graph:default urn:iki:browse:graph:default"
SELECT DISTINCT ?item ?file ?note WHERE {
  GRAPH <urn:iki:ledger:graph:default> { ?item ledger:about ?file }
  GRAPH <urn:iki:browse:graph:default> { ?a ik:annotates ?file ; oa:bodyValue ?note }
}
```

Three things about it are worth knowing before you write one, and each is a test here
(`a_scoped_token_reads_browse_and_both_tokens_run_the_join`):

- ⚠ **The set is ONE `graph` value, separated by whitespace.** `graph=A&graph=B` is a repeated
  named argument, and the HTTP door REFUSES it: `400 invalid argument `graph`: given twice; an
  argument takes one value, so name it once` (ikigai-web 0.1.63+, ledger #387). Through the
  0.1.62 door the last value won silently, so the repeated spelling asked for one graph and the
  join came back empty with nothing said; that was upstream of this server, and the test that
  pinned it went red when the fix arrived, as it was meant to
  (`the_join_runs_through_the_http_door_under_a_signed_in_grant`).
- **A graph you hold no token for refuses the WHOLE read**, naming every missing
  `urn:cap:store:read:graph:` token — never an answer computed over the half you do hold.
- **The join is UNCACHED, and the ledger's own scoped read is not.** A multi-graph read is
  covered only if every member is, and browse's graph is written by the sharer. So the join is
  a fresh query every time by construction; the ~1000× on the ledger's hot read is untouched.

⚠ **`SELECT DISTINCT`** wherever a triple could be in both graphs: oxigraph's merged default
graph is a bag, so a triple present in two members matches once per member.

Three things make that table true rather than aspirational. `ikigai-gonk grants` mints
per-ledger and per-graph tokens only, so nothing this server writes into a grant names browse
(the family), exec or the whole dataset. `grants.json` is refused at startup if any grant names a whole-dataset store
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
pages in front, and its pages never cache; every ledger read
a page makes is a hub read.

**And that arrangement is itself a resource.** `Source urn:kernel:topology` (core 0.1.78) under
`urn:cap:kernel:inspect` renders the chain a door resolves in, as Turtle, with every space
named: the hub is `urn:iki:gonk:space:hub`, an `ik:Fallback` whose ordered `ik:layers` are the
store, the ledger, the render transform and — when configured — the browse family
(`urn:iki:gonk:space:browse`, browse's own patterns with gonk's cache overlay invisible in front
of it), the facades and the backups. The socket and QUIC doors render **exactly the hub's
graph**: the forwarding space claims the hub's identity rather than one of its own, because it
holds exactly the hub's doors. The HTTP door renders `urn:iki:gonk:space:door:http`, two
layers — `urn:iki:gonk:space:pages`, then the hub — and a name neither binds is the end of the
chain, which the paper's §12.5 reachability check reads as unreachable when it runs over this
graph as a path query. (It used to end in a third, a not-found catch-all rendered as an
`ik:Limit` over the empty family; that existed only because `ikigai-web` once answered an
unbound name with a 500, and it is gone now that the library answers 404.) The one `ik:OpaqueSpace` this server can render is a
`gonk.mount`: a remote whose arrangement lives in another process, which `ikigai-resolve` does
not yet forward. `tests/topology.rs` sources the resource through every door and walks it.

### Raw store writes

A ledger grant carries its ledger graph's store WRITE token
(`urn:cap:store:write:graph:urn:iki:ledger:graph:{name}`), and it has to: `ikigai-ledger`
writes every append, comment and close as a `urn:iki:store:graph-update` sub-request under
the CALLER's capability, so without the token the ledger itself fails. That one token was
therefore two authorities — "may use the ledger" and "may write the graph's triples" — and
every door treated it as both. Any ledger writer, the anonymous loopback grant included,
could write any triple into its ledger's graph: an `author` naming someone else's passkey,
which the item page renders as that person (ledger #878, the residual of #864 R4).

**The rule** (Brian's option (b), 2026-10-09): **every door refuses a raw store write to a
ledger graph** unless the caller's capability is root or carries the explicit raw grant for
that graph, `urn:cap:gonk:raw-write:graph:<graph>`. The refusal is a `403` on the HTTP door and
`Denied` on the QUIC door and the socket, and its access line names the stamped principal like
any other (`owner` on the socket). The socket joined on 2026-10-09 (ledger #899): its owner is
root and passes, but an owner process that NARROWED itself to a ledger grant (`cap seal`,
`ikigai mcp --grant` mounted on the socket) carries that grant's store token and nothing
more, and through `ed43af3` it could write its ledger graph raw there. What counts:

- `urn:iki:store:graph-update` whose `graph` is `urn:iki:ledger:graph:{name}` or its
  graveyard `urn:iki:ledger:graph:{name}:deleted` (the prefix compared ignoring case);
- `urn:iki:store:update` and `urn:iki:store:load`, which write the whole dataset: refused
  outright unless the capability is root. At a network door they need the store's broad
  token, which no grant here may carry, so there this changes nothing observable — it keeps
  the rule from being stated wrong. On the socket an owner process COULD seal itself to that
  token, and is refused like any narrowed capability.

**What is unchanged.** The ledger's own writes — `append`, `comment`, `close`, `link`,
`label`, editing an item — work for the same caller with the same grant, because the ledger
issues its `graph-update`s inside the hub kernel, which this check never sees. Reads
(`urn:iki:store:graph-select`, `graph-ask`, `urn:sparql:*`) are not writes. The BROWSE
graph's write token (`--browse-graph write`, the reviewer grant's shape) is minted only as raw
quad authority, so it is not ambiguous and still writes raw.

**Who can still write a ledger graph raw**

| who | how |
| --- | --- |
| the owner | over the socket (`~/.ikigai/gonk.sock`) at ROOT, the capability a plain `ikigai --connect` carries. Migrations and repairs run here: `ikigai-gonk ledger backfill-keys`, a restore, the gonk Book's socket examples. A process of the owner's that narrowed itself on the socket is held to the rule like a network client (ledger #899) |
| an identity an operator names | `--ledger-graph <ledger>` on `client add` or `passkey invite`, which adds the raw grant for that ledger's graph (not its graveyard — that stays root's, over the socket). On the socket, an owner process sealed to the same tokens (`ikigai-gonk grants --ledger-graph <ledger>`) |

```
ikigai-gonk grants --ledger-graph default     # print the three tokens
ikigai-gonk client add ops --ledger default=write --ledger-graph default
```

**If something now answers `403` or `Denied`** on a raw write it used to make: write through
the ledger's endpoints (`urn:iki:ledger:*`) if the change is one they express; otherwise run
it over the socket at root (not from a sealed session); and only if it must come over a
network door, re-mint that identity's grant with `--ledger-graph <ledger>` (`--force`
rewrites an existing grant, naming what it adds). Nothing in this repository, the bridges (`roborev file`, `kata import`, which append
through the ledger) or the devtools scripts wrote a ledger graph raw over a network door when
this landed.

⚠ **What the socket still allows a narrowed process, stated so nobody believes otherwise.**
The socket runs the raw-write rule and none of the author rules: it names no principal (every
request is `principal=owner`), so "a principal-shaped `author` must be the caller's own" would
refuse the owner's every principal-shaped author. So an owner process sealed to a ledger grant
can still FILE an item whose `author` argument names a passkey — the ledger's own argument on
a new item, the same claim as `author=agent-7`, never a triple on someone else's item.
`tests/raw_store_write_878.rs` pins it (`the_socket_keeps_no_author_rule`). An agent that
should not be able to claim a person needs its own identity on the QUIC door, where the author
rule runs.

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

- **A non-loopback HTTP bind.** `--bind 0.0.0.0:1062` stops the server before it opens
  anything, and says to use QUIC.
- **A second holder of the store.** Another gonk, or an `ikigai` with `store = true`, holding
  the dataset stops this one with the path and the mount lines that fix it.
- **The store's broad tokens in any grant.** `urn:cap:store:read` is every graph and
  `urn:cap:store:write` is `DROP ALL`. A `grants.json` naming either stops the server at
  startup, and a connection or session whose grant names one is refused. The store's narrow
  write door refuses the broad key anyway, so such a grant could not write either.
- **The backup family's tokens in any grant** (`urn:cap:gonk:backup`, `urn:cap:gonk:restore`).
  ★ This list and the two below are ONE decision, `quic::grant_refusal`, and every path that
  turns a grant into scopes asks it: startup, every QUIC connection, every passkey request,
  the reviewer grant, and both writers (`client add`, `passkey invite`). Until ledger #864 the
  backup family was checked at startup only, so a `grants.json` edited while gonk ran put
  `urn:cap:gonk:restore` on a QUIC client, which then built a store at a path it chose.
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
  because the file is re-read per connection — and by `client add` as well as `passkey
  invite` (until ledger #805 a certificate enrolment checked only the broad store tokens).
- **A net grant for a host the mount does not dial.** The mount refuses it by name before it
  dials (see [Explaining what it browses](#explaining-what-it-browses)).
- **A `client add` it would have to undo.** Every refusal — the name, an import over an
  existing bundle, a refused token, a rewrite of an existing grant without `--force`, a
  certificate already enrolled under another grant — runs before a key pair is minted or a
  bundle written, so a refused command leaves nothing behind.
- **A `gonk.mount` line that is not `prefer urn:llm:=<target>`**: another mode, another
  prefix, a `quic://` target with no certificate directory (or one that is not there), a
  socket target WITH one, or two lines claiming the same prefix.
- **A `gonk.explain.*.provider` outside `urn:llm:`**, which nothing in this process could
  resolve, or a `max_tokens` that is not a positive number — a ceiling is the only bound on
  what one derivation costs, so a typo in it must stop the server rather than fall back.

## Backup and restore

**gonk holds the only writer lock, so gonk is the only thing that can export the dataset.**
RocksDB permits one writer per directory; a second process cannot open `~/.ikigai/store` to
dump it, and `cp -r` of a live directory is a torn snapshot — SST files and the write-ahead
log mutate under the copy, and the result may not open, or may open and be quietly short.
Backup is therefore a face this server offers, not an ops script someone runs beside it.

```
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:gonk:backup'           # take one now
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:gonk:backup:status'    # when the last good one was
```

⚠ **`--connect` is part of the command, not decoration.** The backup family is bound behind
the owner-only socket and is not one of the prefixes a `mount` line in the config home claims
(`urn:iki:store:` and `urn:iki:ledger:` are), so a bare `ikigai -c 'source
urn:iki:gonk:backup'` does not reach this server. By default a backup is taken **every 24
hours**, compressed, and the **last five** are kept in `~/.ikigai/backups`.

**The scheduled backup holds a time-budget grant** (ledger #979). Since `ikigai-store` 0.2.8
every SPARQL evaluation runs within a budget, and a scoped caller with no
`urn:cap:store:budget:<ms>` grant gets the store's 5 s base. The timer fires under
`backup::JOB_SCOPES` — the backup and whole-dataset read tokens, not root — and the
whole-dataset query takes longer than that: a backup of the live dataset (361,607 quads,
RocksDB) measured 15 s on a quiet machine and 27 s beside other work, and every run without
the grant was refused at 5.0 s. So the list carries `urn:cap:store:budget:120000`, the store's
ceiling. A backup over the socket runs as the owner (root), which gets the ceiling anyway. To
measure it again: `cargo run --release --example backup-cost -- <archive> <scratch-dir>`.

**And both answer grants** (ledger #993). Since `ikigai-store` 0.2.9 a scoped caller's answer is
bounded at 100,000 rows and 16 MiB, and the backup's answer is every quad in the dataset: the
2026-10-09 archive's is 361,607 rows and about 116 MiB of SPARQL JSON. Under the time grant alone
that backup was refused on bytes after 13.9–23.4 s (the `ORDER BY` sorts everything before the first
row is written), so the list also carries `urn:cap:store:answer:10000000` and
`urn:cap:store:answer:bytes:1073741824`, the store's ceilings, for the time grant's reason. A row
grant raises rows and nothing else, so the job needs both. ⚠ The byte ceiling is about nine times
today's answer, so it is the first bound a growing dataset will reach.

**A backup holds the scripts too** (ledger #1175). They are not in the dataset: they are plain
files in `~/.ikigai/scripts` (`gonk.script.dir`, ledger #939). So every backup writes two archives
under one stamp, `gonk-store-<stamp>.nq.gz` and `gonk-scripts-<stamp>.json.gz` (every file of the
scripts layout, `<name>/head.json`, `<name>/versions/<sha256>.json` and `<name>/runs/<id>.json`, as
one JSON document, paths sorted so an unchanged directory gives identical bytes), and ONE sidecar
recording both (`scripts_archive`, `scripts_names`, `scripts_files` and the digests). The status
names it, `urn:iki:gonk:backup:archive:{name}` reads either archive out, and the rotation prunes
them together. A directory that cannot be read fails the backup before anything is written. A
backup taken before this holds the dataset alone, and the status says so.

### The format is N-Quads, and the easy mistake is Turtle

**CONSTRUCT returns triples.** This dataset is partitioned by named graph — one per named
ledger, its graveyard beside it, and browse's own (`urn:iki:browse:graph:default`,
`src/browse.rs`'s `Graph`) — and that partition is what every per-graph capability is written
against. A backup serialized as Turtle or N-Triples collapses every graph into one: the quad
count still matches, every triple round-trips, and the restored dataset has every tenant's
statements in the default graph with the tenancy boundary gone — which is also, exactly, the
shape a browse-graph migration has to avoid producing.

So an archive is N-Quads, gzipped, and every comparison this server makes about one is
**per graph**. The dump reads every quad of every graph from one snapshot of the store and
writes it with oxigraph's own N-Quads serializer — never string surgery, which matters because
ledger bodies contain newlines, quotation marks and trailing periods, and those are exactly the
rows a hand-rolled writer corrupts.

★ **The dump is streamed** (ledger #1699). It used to be `urn:iki:store:select` with
`ORDER BY`, so the store sorted the whole dataset and answered it as SPARQL JSON, and the backup
held the N-Quads parsed out of that plus a copy of them for `urn:compress:gzip`: about 517 MB
of peak memory for 375,000 quads, 1.8 GB for 1.5 million, and at 3 million the sorted query ran
past the store's 120 s ceiling, so no backup could be taken at all. Now each quad goes straight
into a gzip encoder and what is held is the archive: 129 MB, 233 MB and 276 MB for the same three
stores, in 2.8 s, 11–13 s and 20 s (`examples/backup-memory.rs`, synthetic RocksDB stores, release
build, 2026-10-16). The encoder writes the level and the pinned header `urn:compress:gzip`
writes, and a test pins that an archive is byte for byte what `urn:compress:gzip` gives for its
N-Quads, so two backups of unchanged data are still byte-identical and "has anything changed" is
still a digest comparison. The order is the store's own (a function of the contents on RocksDB),
not the sorted one, so the first backup after this change differs from the one before it even
over unchanged data. The store's time and answer bounds still apply, read from the caller's
grants as the store reads them; the byte bound now counts the N-Quads, not SPARQL JSON.

### A merge is not a restore

Loading an archive into a store that already holds quads is a **union**: it resurrects
everything either side deleted and can never reproduce the backed-up state. So a restore
builds a **new** store directory, and refuses a target that is not empty — or that is the
live store, by name.

```
ikigai --connect ~/.ikigai/gonk.sock -c \
  'source urn:iki:gonk:backup:archive:gonk-store-2026-09-16T172813Z.nq.gz \
   | sink urn:iki:gonk:restore into=/tmp/restored'
```

That leaves a complete dataset beside the live one and tells you the graph set and the
per-graph counts it verified. Adopting it is an operator's act, with the server stopped:
swap the directories and start gonk. The destructive step is visible, manual, and happens
when nothing is writing.

`urn:iki:gonk:restore` is also the **verifier**: it compares the restored dataset's graph set
and per-graph counts against the archive it was built from and fails if they differ. A backup
you have not restored is not a backup, so `tests/backup.rs` restores one on every run.

The scripts archive has its own restore and verifier, `urn:iki:gonk:restore:scripts`, under the
same `urn:cap:gonk:restore`:

```
ikigai --connect ~/.ikigai/gonk.sock -c \
  'source urn:iki:gonk:backup:archive:gonk-scripts-2026-10-10T120000Z.json.gz \
   | sink urn:iki:gonk:restore:scripts into=/tmp/restored-scripts'
```

It checks every path against the layout before it writes anything (a `..`, an absolute path or
a file that is no script's is refused), writes the files into a NEW directory (never the live
one, never one that holds something), and then reads every one back through `ikigai-script`'s own
`DirBackend`: each head, every version it or its history names, every version file against the
digest it is named by, every run. An edited version, or a head naming a version the archive does
not hold, does not verify. `tests/script_backup_1175.rs` restores one on every run and runs a
script from the copy.

### The two most dangerous grants in the system

A backup reads **every** graph, which is precisely the authority the per-graph boundary
exists to avoid handing out; a restore builds a store from bytes the caller supplies. Both
live behind the **owner-only socket**, whose caller can read the dataset's files anyway —
its own door, not a flag on the public one:

| token | what it is | who can hold it |
| --- | --- | --- |
| `urn:cap:gonk:backup` | take a backup, read the status, read an archive | the socket door's root capability, and nothing else |
| `urn:cap:gonk:restore` | build a store from an archive, or a scripts directory from a scripts archive | the same |
| `urn:cap:store:read` | every graph (declared by `urn:iki:gonk:backup`, because it really reads them) | the same |

`gonk/grants.json` is **refused at startup, and on every use,** if it names any of them, exactly as it is for
the store's broad tokens and the offering wildcards: a grant a reader could mistake for a
narrowing must not be silently the opposite. `tests/doors.rs` drives the real HTTP door and
asserts a `403` on every one of these, alongside `urn:iki:store:info` — the posture that had
to survive this feature.

### The schedule, and why it is not `urn:time:schedule`

The job runs in an `ikigai-time` `JobRegistry` inside this process. **The control plane is
deliberately not bound**: `urn:time:schedule` fires an arbitrary target under the registry's
own capability, so binding it behind three doors would be a way to have this server issue any
request as itself. The target set is fixed at startup by `main`; the registry fires under
exactly `urn:cap:gonk:backup` and `urn:cap:store:read`, so a scheduled backup can read every
graph and write the rotation, and cannot write a single quad.

It lives here rather than on the host daemon's scheduler for one reason: **a backup job must
not be able to outlive the thing it protects.** An external scheduler's failure mode is "gonk
is healthy, data is changing, backups stopped, nobody knows". If gonk schedules its own
backup, "gonk is down" also means "nothing is being written", and a missed backup is harmless.

Two things make the schedule trustworthy rather than merely present:

- **A recurring timer's first tick is one whole interval away.** A daily job on a
  `KeepAlive` daemon restarted more often than daily never reaches it — silently, with a
  schedule that looks correct in every readout — and even one restart puts the next backup
  off by up to a day. So the timer is phased to the ROTATION, not the process: startup reads
  the newest sidecar and the first run is due one cadence after that archive was taken, a
  minute out at the soonest (at once, in effect, when there is none or it is already
  overdue). A restart costs a backup nothing.
- **`urn:iki:gonk:backup:status` answers from both sides.** A scheduled job that *hangs*
  reads stale forever and never reads failing, and gonk is not heartbeat-watched. The status
  reports the newest archive actually on disk with its per-graph counts and digests, the
  whole rotation, and the timer's own health — runs, time since the last run, time since the
  last **success**, failures in a row, and when the **next run** is due. A next run that is
  past due says OVERDUE, which is a wedged timer; a job that fires and fails shows as
  "failures in a row". `as=application/json` gives
  the same thing to a machine.

## Configuration

Flags override config wholesale; there is no environment-variable channel. The config home
is `~/.config/ikigai` (or `$XDG_CONFIG_HOME/ikigai`). ⚠ A comment goes on a line of its own: the
`key = "value"` grammar every ikigai process reads has no trailing comment, so one after a
value becomes part of the value.

```toml
# ~/.config/ikigai/config.toml
# or gonk.port = 1062, which always means loopback
gonk.bind = "127.0.0.1:1062"
gonk.socket = "~/.ikigai/gonk.sock"
# gonk.quic.bind = "0.0.0.0:1062"     # unset: QUIC opens on UDP at the HTTP port once a certificate is enrolled;
                                      # set: QUIC must open, or gonk refuses to start
# repeatable
gonk.http.ledger = "default"
# repeatable; unset, no browse family
gonk.browse.root = "core=~/git-personal/ikigai-core"
# repeatable; the GitHub repository a root's "On GitHub" links name when its origin does not
# gonk.browse.github = "core=ikigai-rs/ikigai-core"
gonk.mount = "prefer urn:llm:=quic://127.0.0.1:4433 ~/.config/ikigai/gonk/quic/peers/plasma"
# gonk.explain.file.provider = "urn:llm:coder:ask"     # the tiers, and the per-call ceilings
# gonk.explain.file.max_tokens = 400                   # file 400, dir 600, review 800, pr 600
# gonk.explain.dir.provider = "urn:llm:ask"            # .dir / .review / .pr take the same pair
# gonk.explain.max_prompt_bytes = 16384
# the cadence; "off" (or --no-backup) takes none
gonk.backup.every = "24h"
# gonk.backup.keep = 5                # how many archives the rotation keeps
# gonk.backup.dir = "~/.ikigai/backups"
# gonk.script.dir = "~/.ikigai/scripts"   # where published scripts and their runs live
# gonk.script.anonymous_runs_per_minute = 30  # anonymous runs, all together (0 for none)
# gonk.nl.ask = "urn:llm:coder:ask"   # the model door urn:nl:sparql drafts through (default urn:llm:ask)
# gonk.review.space = "reviews"       # bind the git-event review QUEUE; arms nothing
# gonk.review.grant = "reviewer"      # the grant a pass runs under; naming it arms nothing
# gonk.review.arm = true              # ⚠ and the word that arms it; needs the grant above
# gonk.review.root = "~/.ikigai/spaces"
# gonk.queue.serious = "critical,major" # the severities the Queue page asks a human about
# gonk.review.judge = "urn:llm:coder-next:ask"  # the judge; the Queue orders by its verdict
# gonk.log.access = false             # no access lines (they are ON by default)
# gonk.http.anonymous_sparql_budget_ms = 1000  # an anonymous caller's SPARQL time budget, 1..5000
# gonk.http.session_idle_minutes = 60   # a passkey session with no request for this long ends, 1..720
# gonk.toolbox.dist = "~/.local/share/ikigai-toolbox/dist"  # the RDF Toolbox's build; unset, no /toolbox
```

A `gonk.browse.root` line is what composes `urn:repo:*` and `ikigai-repo`'s facades at all.
The name is spliced into `urn:repo:<name>:…`, so it may not contain `:`, `/`, `{` or `}`, may
not repeat, and may not be `pr` or `style` (those read as the other family's names); the
directory must exist. Each is refused at startup with the line to edit — `ikigai-browse`
would assert instead, which arrives as a panic where the banner should be.

### Checking out the repositories it browses

On a new machine the roots have to be cloned before gonk will start. `ikigai-gonk checkout`
does that and writes the lines:

```sh
ikigai-gonk checkout https://github.com/ikigai-rs/ikigai-gonk.git kata=git@github.com:kenn-io/kata.git --write-config
```

```text
cloned     ikigai-gonk at 5050879  ~/.ikigai/checkouts/ikigai-gonk
cloned     kata at 1c2d3e4  ~/.ikigai/checkouts/kata

browse roots:
  gonk.browse.root = "ikigai-gonk=~/.ikigai/checkouts/ikigai-gonk"
  gonk.browse.root = "kata=~/.ikigai/checkouts/kata"

/Users/you/.config/ikigai/config.toml:
  added      gonk.browse.root = "ikigai-gonk=~/.ikigai/checkouts/ikigai-gonk"
  added      gonk.browse.root = "kata=~/.ikigai/checkouts/kata"
  backup     /Users/you/.config/ikigai/config.toml.1791302400.bak

gonk reads gonk.browse.root at startup only: restart gonk to serve the new root(s).
```

- **Where.** Each URL is cloned into `~/.ikigai/checkouts/<name>` (`--dir` for another
  directory). `<name>` is the URL's last path segment without `.git`, and it is also the
  browse root's name, so `name=url` sets both. It is checked like any root name.
- **Again.** Run it again with the same URLs (or with `--all`, below) and each clone is fetched and its default branch
  **fast-forwarded**, and nothing else. A checkout with local changes (untracked files
  included), on another branch, or with commits that upstream does not have is **refused and left
  exactly as it was**: nothing is reset, stashed, merged or discarded. Each URL is reported on
  its own and the rest still run; any refusal or failure makes the exit status 1. A root gonk
  already serves needs no restart after an update: gonk watches it, and a fast-forward is a
  change on disk like any other.
- **The config.** Without `--write-config` the lines are only printed. With it, the missing
  ones are appended to the config home's `config.toml` (or `--config PATH`): the previous file
  is copied beside it first, no other line is touched, a name already configured for the same
  directory is left as it is, and a name configured for a DIFFERENT directory is a conflict
  that writes nothing for it.
- **No prompts.** git runs with stdin closed and `GIT_TERMINAL_PROMPT=0`, and ssh in
  `BatchMode` unless `GIT_SSH_COMMAND` or `core.sshCommand` already says how to run it, so a
  repository that needs credentials fails with git's message instead of waiting. Credential
  helpers and ssh agents work as usual.

It is a command, like `review request`: it opens no store and binds no door, and the server
itself has no code that fetches anything.

**Restarting is the whole fix, and how is yours.** Adding a root changes only `config.toml`,
which gonk reads when it starts, so a restart of the process is all it takes. The command does
not print a supervisor's command: a launchd label (or a systemd unit, or a terminal) is the
operator's own choice, and one machine's restart line is the wrong one on the next. Under
launchd a config-only change needs only the process restarted; a REPLACED binary or plist is
what needs a re-registration (`bootout`, then `bootstrap`).

### Keeping them current

`--all` updates every checkout already in the managed directory without the URL list:

```sh
ikigai-gonk checkout --all            # or --dir DIR, --config PATH
```

```text
updated    ikigai-gonk 5050879..9b182ff (3 commit(s), fast-forward)  ~/.ikigai/checkouts/ikigai-gonk
REFUSED    kata  ~/.ikigai/checkouts/kata
  it has local changes (1 path(s); `git -C /Users/you/.ikigai/checkouts/kata status`). A managed checkout is fast-forwarded only; nothing was fetched or changed
current    tools at 1c2d3e4  ~/.ikigai/checkouts/tools

coverage of ~/.ikigai/checkouts by the roots in /Users/you/.config/ikigai/config.toml:
  unused     tools  ~/.ikigai/checkouts/tools  no gonk.browse.root points into it: gonk does not browse it (`checkout <url> --write-config` adds its line)
```

- **What it updates.** Every directory under the managed directory, each from its own
  `origin`, under its own directory name. The per-repository lines, the refusals (local
  changes, untracked files included; another branch or a detached HEAD; commits of its own;
  diverged) and the exit status are the ones above: any refusal or failure is exit 1, and the
  rest still run. A directory there that is not a git checkout is refused like any other
  surprise; names starting with `.` are skipped.
- **What it never does.** It clones nothing (a missing managed directory is an error, not
  created), writes no config (`--all --write-config` is refused), and takes no URLs (`--all`
  with URLs is refused). Bare `checkout` with no URLs is an error that names `--all`, rather
  than a quiet `--all`: a command that fetches every repository should say so on its line.
- **Coverage.** After the updates it reports, changing nothing, how the checkouts and the
  config's roots cover each other: `unused` for a checkout no `gonk.browse.root` points into
  (gonk does not browse it), and `missing` for a root that points into the managed directory
  at nothing (gonk refuses to start until it exists). Roots elsewhere are not its business.
  Paths are compared with symlinks resolved, and a root inside a checkout counts as using it.
  Coverage lines are information and do not change the exit status.

**On a timer.** Nothing in the server fetches, so a periodic update is this command run by the
system's scheduler. gonk needs no restart for it: every root it serves is watched, and a
fast-forward is a change on disk like any other, so the next read sees it. A refusal leaves
that checkout where it was, and the log says why.

macOS, a `launchd` agent beside gonk's own (`~/Library/LaunchAgents/dev.ikigai-rs.gonk-checkout.plist`),
every 15 minutes:

```xml
<plist version="1.0"><dict>
  <key>Label</key><string>dev.ikigai-rs.gonk-checkout</string>
  <key>ProgramArguments</key><array>
    <string>/bin/sh</string>
    <string>-c</string>
    <string>date; exec /Users/you/.cargo/bin/ikigai-gonk checkout --all</string>
  </array>
  <key>StartInterval</key><integer>900</integer>
  <key>StandardOutPath</key><string>/tmp/ikigai-gonk-checkout.log</string>
  <key>StandardErrorPath</key><string>/tmp/ikigai-gonk-checkout.log</string>
</dict></plist>
```

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.ikigai-rs.gonk-checkout.plist
```

Linux, a crontab line (`crontab -e`), the same interval and log:

```text
*/15 * * * * (date; $HOME/.cargo/bin/ikigai-gonk checkout --all) >> /tmp/ikigai-gonk-checkout.log 2>&1
```

The log is `/tmp/ikigai-gonk-checkout.log` in both, one `date` line before each run's report:
the command prints no time of its own, and neither scheduler adds one. (Plain `date` in the
crontab on purpose: cron reads `%` as a newline.) Under either scheduler git runs without a terminal:
an HTTPS remote needs a credential helper and an SSH remote a key the agent already holds,
or that checkout is a `FAILED` line (git's own message) rather than a hang.

⚠ **A managed clone shows its default branch.** Work in progress in other branches or
worktrees (a kata-flight loop's worktrees, for one) is not what gonk browses there; point a
root at that worktree by hand if that is what you want to read.

| file | what it holds |
| --- | --- |
| `store.toml`, `gonk.store.toml` | `path = "…"` — where the dataset lives, read by `ikigai-store` |
| `gonk/clients.json` | identity → grant name: certificate fingerprints under `clients` (the same shape `ikigai serve quic://…` reads), passkeys under `passkeys` |
| `gonk/grants.json` | grant name → capability scopes (the same shape as the cli's `grants.json`) |
| `gonk/invites.json` | outstanding passkey invites, by the SHA-256 of their codes |
| `gonk/passkeys.log` | one `gonk:PasskeyRevoked` line per `passkey revoke` and one `gonk:PasskeySessionsEnded` line per `passkey logout --all`, append-only, owner-only; the running server reads what is appended |
| `gonk/render-rules.ttl` | this deployment's render rules, replacing the shipped table wholesale |
| `gonk/quic/` | `server.crt`, `server.key`, and one `clients/<name>/` bundle per client |
| `~/.ikigai/backups/` | the rotation: `gonk-store-<stamp>.nq.gz` and a `.meta.json` sidecar each, owner-only; any other file there is never listed or pruned |

## What it composes

The manifest is the module manifest: gonk links
[`ikigai-store`](https://github.com/ikigai-rs/ikigai-store) with its RocksDB backend,
[`ikigai-ledger`](https://github.com/ikigai-rs/ikigai-ledger),
[`ikigai-browse`](https://github.com/ikigai-rs/ikigai-browse) and
[`ikigai-repo`](https://github.com/ikigai-rs/ikigai-repo), the `ikigai-web`, `ikigai-ipc` and
`ikigai-quic` transports, and `ikigai-resolve` for the mount. For the backup family it adds
[`ikigai-compress`](https://github.com/ikigai-rs/ikigai-compress) — whose four resources ARE
bound, because the scripts archive is gzipped and every archive is expanded on restore by
resolving them through this kernel (the store archive is streamed into the same encoder,
ledger #1699) —
and `ikigai-time`, whose `JobRegistry` fires the schedule and whose `urn:time:*` control plane
is deliberately **not** bound. For the browser face it adds two libraries it calls as
functions and binds no resources from: `ikigai-xslt` (the stylesheet engine) and
`ikigai-passkey` (the assertion verifier). **No LLM client** — the model is mounted, never
linked. For [scripts](#scripts-saved-queries-and-plans-ledger-939) it adds
[`ikigai-script`](https://github.com/ikigai-rs/ikigai-script), `ikigai-engine`'s plan doors
(`urn:plan:*`, nothing else of the engine) and
[`ikigai-shacl`](https://github.com/ikigai-rs/ikigai-shacl), whose validator the plan doors
compose and no door serves. For [natural language](#natural-language-a-query-drafted-from-a-sentence-ledger-1138)
it adds [`ikigai-nl`](https://github.com/ikigai-rs/ikigai-nl) (`urn:nl:*`, drafting through the
mount, so no model client) and binds `ikigai-vocab`'s `urn:ikigai:vocab`, the vocabulary its
grounding states.

⚠ **That is a correction, and it has now been made twice.** Through 0.1.0 this section said
"no filesystem module, no process execution and no outbound network client is compiled in, so
none is reachable whatever a grant says". The browse family made two of those three false:
`urn:repo:{root}:file` and `:tree` read the filesystem under a configured root, and
`urn:repo:{status,log,branch}`, `urn:repo:pr:*` and `urn:system:exec` spawn `git` and `gh`.

**The third has now changed too, and it is worth stating exactly rather than dropping.** With
a `gonk.mount` line, gonk opens an outbound connection — QUIC, or a Unix socket — **to one
peer named in its config, presenting one client certificate, speaking ikigai's own wire
protocol**. That is narrower than what the old sentence ruled out in three ways that matter:

- **No HTTP client is reachable, and since ledger #1176 none is linked again.** From ledger
  #939 until then `ikigai-shacl` brought rudof, and rudof brought `reqwest` and turned on
  oxigraph's `http-client` (its SPARQL `SERVICE` handler). gonk now links ikigai-shacl with
  its `rudof` feature off, and the installed binary's dependency graph holds neither
  (`cargo tree -e no-dev -i reqwest` prints nothing). The refusals stay as well: `urn:httpGet`
  and its family are not bound, `ikigai-llm` is not a dependency, the validator is served to
  no caller and refuses a `SERVICE` in a shape itself, and the store refuses `SERVICE` and
  `LOAD` at every door (0.2.10). ⚠ The TEST build still links rudof, through the
  `ikigai-layers` dev-dependency, so this is a statement about the binary, not about `cargo test`.
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

`tests/conformance.rs` pins the socket and QUIC catalog to the store's thirteen resources, the
ledger's twenty-two and gonk's own five (the chunk renderer and the four `urn:sparql:*` forms,
which are a mapping onto the store and link nothing new), pins the HTTP door's to those plus its
pages, and walks
`ikigai-conformance` over the hub, a door, and the HTTP door, scoped by `Suite::own` to gonk's
own resources, with the store's and the ledger's pinned exactly as inherited (each crate walks
its own suite). `tests/browse.rs` pins the
browse composition's twenty more, in the hub and through a door, pins the five the mount adds
(and their absence without one), and pins the spend gate per capability. `tests/backup.rs`
pins the backup composition's eight more — gonk's four and compress's four — asserts that
`urn:time:schedule`, `urn:time:cancel` and `urn:time:jobs` are bound by nothing, and restores
a real archive, comparing the graph set and the per-graph counts rather than a total.
`tests/scripts_939.rs` pins the script family's twelve — the library's eight, the plan doors'
three and the validator — in the hub, and the validator's absence at every door.
`tests/nl_1138.rs` pins the natural-language family's five (four without a mount, which binds no
drafter).
`tests/topology.rs` sources `urn:kernel:topology` through the hub and both door shapes, pins the
named nodes and their order, asserts nothing of this crate's is opaque, and runs the §12.5
reachability walk over the result.

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
cannot: **who the other holder is and where it writes.** Since `ikigai-browse` 0.4.0 it knows
it the strongest way available — by **choosing**. `src/browse.rs`'s `Graph` is that choice,
made once in `main`, and the two statements that must agree about it are two readings of the
one value: `graph.sharer_writes()` opens the store, `browse::wire(…, &graph)` puts the same
value on the mount. There is no second place to edit.

Before 0.4.0 the promise was TRANSCRIBED: `main` wrote `SharerWrites::only_the_default_graph()`
because someone had read browse's three writers and found `GraphName::DefaultGraph` hard-coded
in each. That is a fact about a dependency's internals, re-typed in a consumer, with a comment
asking the next person to keep it true — and a transcribed invariant drifts. Today `main` does
not name `SharerWrites` at all.

The choice since 2026-09-16 is **`urn:iki:browse:graph:default`** (see *Giving browse its own
graph* below, and read it before upgrading a store that has one), so the store answers
freshness per read from that promise: a scoped read
(`urn:iki:store:graph-{select,ask,construct,describe}`), whose universe is one NAMED graph by
construction, is cacheable under the store's own three write threads **for every graph the
promise does not name** — and that is what every ledger read is made of. Declared, the same
read is **10.9µs**. The broad faces stay uncacheable, by name:
`urn:iki:store:{select,ask,construct,describe}`, `urn:iki:store:info`, and therefore
`urn:iki:ledger:ledgers`, which asks *which graphs exist* through the broad door.

★ **What naming a graph costs is that graph's cacheability, and nothing else's.** A scoped
read of `urn:iki:browse:graph:default` is `Expiry::Always` — the sharer may write it, and the
store says so per read rather than this crate declaring it anywhere. The ledger's reads are
untouched, and that is measured rather than argued: the same 247-item corpus, the same
composition, once with browse in the default graph and once with it in its own.

```
--- 247 items, mean of 20 reads ---
  owned     (open — no browse face)                  items    12.16µs  next    13.42µs
  shared    (open_shared, undeclared)                items    12.63ms  next    22.60ms
  declared  (browse in the DEFAULT graph)            items    10.82µs  next    12.70µs
  declared  (browse in its NAMED graph — shipped)    items    10.85µs  next    12.43µs
```

⚠ **A false promise there would be silent, unbounded staleness** — reads of a graph the sharer
does write, cached against threads its writes never cut. So the promise is not left to a
comment: `tests/browse.rs` prints all three numbers and takes the store's own
`reserved_graphs_fingerprint()` either side of a real browse write, which fingerprints the
**quads** of every reserved graph rather than the set of graph names, and errors rather than
passing vacuously on a store that promised nothing.

⚠⚠ **And either side of a real browse READ**, which is why the manifest takes 0.4.0 rather
than staying where it was. Through 0.3.2, browse's annotation reads passed no graph at all —
in `quads_for_pattern` that means *every graph* — and `annotate::refresh` rewrites a drifted
annotation **during a Source**. Together those made a plain read destructive across a graph
boundary: it re-anchored an annotation-shaped quad sitting in another writer's named graph and
persisted the move. gonk, with one dataset shared between browse and a graph-scoped ledger, is
exactly the host that had something to lose, and it would have lost it twice — the other
writer's quads relocated, and this server's coverage promise false from that moment on.
`a_browse_read_touches_no_reserved_graph` states it; built against 0.3.2 it fails on the
visibility half and, with that assertion removed, again on the write-back half.

### Giving browse its own graph — and upgrading a store that predates it

**Since 2026-09-16 browse's quads live in `urn:iki:browse:graph:default`.** Before that they
were in the store's default graph, which has no IRI: no `urn:cap:store:*:graph:` token could
name them, so every query over an annotation or an archived explanation was a root one, and
the ledger↔browse join was reachable from the socket door and from nowhere else.

⚠⚠ **This is a data migration, not a setting, and the failure mode is silence.** A binary
carrying this choice reads and writes ONE graph. Quads an older gonk wrote into the default
graph are still on disk and are no longer visible to any browse read — the annotation panel is
empty, the archive is empty, a review's findings are gone, and **nothing errors, at any
layer**. New explanations land in the named graph and never join the old ones.

So gonk counts them before the doors open, using `ikigai-browse`'s own counting function — the
number in the banner is the number `migrate-annotation-ns` prints:

```
ikigai-gonk: ⚠⚠ THE BROWSE ARCHIVE IS INVISIBLE — 10 quad(s) are outside <urn:iki:browse:graph:default>
```

**The upgrade, in order. gonk must be STOPPED for step 4** — it holds the RocksDB write lock,
so the migration cannot run beside it, and it should not: the migration is a rewrite of the
dataset this server is serving.

```sh
# 1. the migration tool (a feature-gated binary of ikigai-browse, not shipped with gonk)
cargo install ikigai-browse --version 0.4.0 --locked --features migrate --bin migrate-annotation-ns

# 2. a backup, taken through the running server — it is the only thing that can export the
#    dataset, and `--commit` in step 5 cannot be undone by restarting
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:gonk:backup'
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:gonk:backup:status'   # confirm it succeeded

# 3. what is there now, read-only, through the socket — no lock taken
ikigai --connect ~/.ikigai/gonk.sock -c \
  'source urn:iki:store:select query="SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }"'

# 4. STOP gonk — the store has one writer
launchctl bootout gui/$(id -u)/dev.ikigai-rs.gonk        # or however it is supervised

# 5. migrate: dry run first, and it prints PASS/FAIL with the four counts
migrate-annotation-ns ~/.ikigai/store --graph urn:iki:browse:graph:default
migrate-annotation-ns ~/.ikigai/store --graph urn:iki:browse:graph:default --commit

# 6. install the new binary, THEN start it — a gonk with the new choice must not run against
#    an unmigrated store, and a gonk with the old one must not run against a migrated one
cargo install --path . --locked                          # or the released version
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.ikigai-rs.gonk.plist

# 7. the banner says where the quads land, and says nothing about a migration
#      browse  urn:repo:{…}:* — annotations and archive in <urn:iki:browse:graph:default>
```

⚠ **Steps 5 and 6 are one window, in that order.** Migrating first and starting the OLD binary
strands the quads the other way (it reads the default graph and the data is now in the named
one) — gonk says so too, but the window is a window either way. Keep it short.

**After the migration**, two things are an operator's and neither is automatic:

- **Grants.** Nobody holds the browse graph's tokens until someone mints them —
  `ikigai-gonk grants --browse-graph read`, or `--browse-graph read` on `passkey invite` /
  `client add`. See *The three doors*.
- **Saved queries.** Any query of yours that read browse's quads as a bare pattern now
  matches nothing, silently. Wrap that half in `GRAPH <urn:iki:browse:graph:default> { … }`.
  Backups need no change: an archive is N-Quads and every comparison this server makes about
  one is per graph, so the browse graph simply appears as a graph.

### Importing an archive from another host's store

A gonk that starts with browse roots configured has an **empty** browse graph, and pays in
inference for the first explanation of everything. If another host has already derived that
archive over the SAME directories — a standalone `ikigai-dev-server`, or another gonk — the
archive can be carried across rather than re-derived. The tool is `migrate-archive-roots`, a
second feature-gated binary of `ikigai-browse`, and it is a different operation from the
in-place move above on every axis: two stores, only the target written; a source that may stay
LIVE, because it is opened read-only; and roots RENAMED on the way, because the two hosts
need not have named the same directories the same thing.

★ **The root name is inside the data, in five places, and a partial rewrite is silent.** The
subject IRI (`urn:ikigai:browse:explain:{root}:…`, `…:review:{root}:…`), the `ik:repo`
literal, and three object positions (`ik:about`, `ik:annotates`, `prov:used`) — plus
`prov:wasGeneratedBy` on an annotation, pointing at the review pass whose IRI also carries the
root, which is the one an obvious list misses. Rewrite some and not the others and the archive
is present, countable, SPARQL-visible and **answered by nothing**, because a read builds its
IRI from THIS host's root name. The tool keys on the IRI position rather than on a list of
predicates, and it refuses rather than guessing: every root the source mentions must be
mapped or dropped, every browse-minted subject must be assignable to a root, and no two roots
may be renamed onto one name.

⚠ **Identical root names on both hosts is the cheap way out of all of that**, because then
every rename is the identity and no IRI is rewritten at all. It is worth renaming this
server's roots to match the source's BEFORE the import rather than mapping afterwards; the
per-root `rewritten` column is `0` for every row when you have.

**The import, in order. gonk must be STOPPED for step 4 — it holds the target's writer
lock. The SOURCE host does not have to stop**: two different store paths, two different
locks, and the source is never written.

```sh
# 1. the tool — feature-gated, and NOT the same binary as the in-place move above. It arrived
#    in ikigai-browse 0.4.1; until that is on crates.io, build it from a checkout.
cargo install ikigai-browse --version 0.4.1 --locked --features migrate --bin migrate-archive-roots
#    or, from a checkout of ikigai-browse:
cargo build --release --features migrate --bin migrate-archive-roots

# 2. a backup, taken through the running server — `--commit` cannot be undone by restarting
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:gonk:backup'
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:gonk:backup:status'

# 3. the SURVEY: run it with no --root and no --drop at all. It prints one row per root the
#    source holds, with quad and explanation/review/annotation counts, and then refuses.
#    Both stores are open read-only here, so this runs against a live pair.
migrate-archive-roots <source-store> ~/.ikigai/store --graph urn:iki:browse:graph:default

# 4. STOP gonk — the store has one writer
launchctl bootout gui/$(id -u)/dev.ikigai-rs.gonk        # or however it is supervised

# 5. decide every root, dry run, then commit. The dry run's "would be" column is the same
#    counting function over a projection of the plan, not a second prediction.
migrate-archive-roots <source-store> ~/.ikigai/store \
  --root ikigai-core=ikigai-core --root ikigai-cli=ikigai-cli \
  --drop some-root-this-host-does-not-serve \
  --graph urn:iki:browse:graph:default
#   … then the same line again with --commit

# 6. start gonk. The banner's unmigrated-quads check is about the GRAPH, not about this
#    import, so it says nothing either way — step 7 is the acceptance.
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.ikigai-rs.gonk.plist
```

⚠ **A root this host does not serve is a DROP, and a drop is a decision to state, not to
discover.** Carried under a name no root of this host produces, an archive is unreachable by
every read — which is the failure the tool exists to prevent — so it refuses the carry and
makes you write `--drop`. The survey's quad and explanation counts for that root are what the
drop costs, and they are worth reading before you type it.

⚠ **The import is a one-shot, not a sync.** Anything the source derives after it is not here.
Re-running is safe and cheap — the transfer is idempotent, and a second run reports every
quad as `already in target` and `new to target 0` — so re-run it immediately before retiring
the source rather than trusting the first run's numbers.

#### Verifying it — and the three ways a `0` lies

★ **The counts prove a copy happened; only a resolution proves the rewrite was right**, and
on this server the obvious count is the one thing that cannot tell you. Read an entry back:

```sh
# derives NOTHING by declaration, and joins on ik:about rather than the working tree —
# so an empty answer here for a path the archive holds is a migration defect
ikigai --connect ~/.ikigai/gonk.sock \
  -c 'source urn:repo:ikigai-core:explain-versions:src/lib.rs as=application/json'
```

⚠ **Never verify with a bare `explain`**: with a `gonk.mount` LLM peer configured it derives,
spends, and answers — which looks exactly like success. `explain-versions` derives nothing;
`explain … version={tag}` answers `NotFound` on a miss rather than falling back to a model.
Both are safe; `explain` alone is not.

And when you do count, count IN THE GRAPH, through a door that can see it. A `0` from this
server means three different things and only one of them is "the import did not run":

| what you ran | why it says `0` |
| --- | --- |
| `SELECT (COUNT(*)) WHERE { GRAPH <urn:iki:browse:graph:default> { ?s ?p ?o } }` at `/sparql`, anonymously | the browse graph is **outside the caller's scope** — the anonymous HTTP door holds its ledgers' tokens and nothing else, and a scoped read answers a graph outside the set *emptily rather than erroring* (see *Not built*) |
| `SELECT (COUNT(*)) WHERE { ?s ?p ?o }` through the socket | the pattern matches the store's **default graph**, which holds nothing: every quad here is in a named graph, and `urn:iki:store:select` does not union them into the default one |
| either of the above, correctly scoped | the import really did not run |

So the one query that answers the question is graph-named AND run where the graph is readable
— the socket door, or an identity holding `urn:cap:store:read:graph:urn:iki:browse:graph:default`:

```sh
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:store:select as=text/csv \
  query="SELECT ?g (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g"'
```

A per-root breakdown of what landed is the `ik:repo` literal, which the import rewrote to this
host's names — so it is also the check that the rename took:

```sh
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:iki:store:select as=text/csv \
  query="SELECT ?repo (COUNT(*) AS ?n) WHERE { GRAPH <urn:iki:browse:graph:default> { \
         ?s <https://ikigai-rs.dev/ns#repo> ?repo } } GROUP BY ?repo ORDER BY ?repo"'
```

A dropped root must not appear in it, and no root this server does not serve may either.

★ **A backup's own metadata is the cheapest before/after you will get**, and it needs no
query at all: `urn:iki:gonk:backup:status` prints per-graph counts, so a backup taken at step
2 and another after step 6 bracket the import in numbers this server produced itself.

### Watched roots, and why the reads are cached at all

`ikigai-browse` declares its `tree`, `file`, `hash` and `state` reads live and uncacheable,
which is the only honest declaration a *library* can make: caching is a promise that something
will notice when the file changes, and a library cannot know whether its host is watching. A
server can. gonk watches every configured root (FSEvents/inotify, recursively) and cuts two
golden threads per root: `urn:iki:gonk:browse:root:{name}`, which the `tree` and `file` reads
hang from, and `urn:iki:gonk:browse:root:{name}:wide`, which `hash` and `state` hang from
because each can see a file a `.gitignore` hides.

★ **The watch ignores what git ignores** (ledger #667): every `.gitignore` under the root,
`.git/info/exclude` and the global excludes file, plus — always — anything inside a `target/`
directory and anything inside `.git/` except the files `git status` reads (`HEAD`, `index`,
`packed-refs`, `config`, `info/exclude`, `refs/…`). A change inside `target/` or `.git/`
cuts nothing; a change inside a directory a `.gitignore` hides cuts only the wide thread; an
ignored entry directly in a listed directory (`Cargo.lock` in a library, `target` itself)
still cuts both, because a cached listing shows it. And because an ignored change cuts
nothing, no read of an ignored path is cached: `tree:target`, `file:book/index.html` and
`hash:target` are served live. Before this, with every ikigai repository a root, one cargo
build cut its root 28,467 times in 90 seconds.

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

★ **The browse page links TWO of browse's stylesheets, and both are that family's own
resources rather than anything of gonk's.** `urn:repo:style` is the syntax theme for the
`hl-` classes inside a file view; `urn:repo:style:layout` (`ikigai-browse` 0.4.2) is the page
furniture — crumbs, entry lists, the action strip, the disclosure menus, annotation cards,
the pull-request listings. Until that release the only copy of those rules anywhere was a
const string inside `ikigai-web`'s binary, so every other host served browse's markup
unstyled and this door rendered a tree as a cascade of gonk chips (ledger #441). Both links
are withheld from a caller who cannot read them, together, for the same reason.

`gonk.css` defines **no `browse-*` rule** and must not grow one: a rule here would silently
win against the family's own sheet and drift the moment browse changed its markup. When this
door knows the caller cannot annotate, it states that as a FACT on the shell —
`data-browse-posture="read-only"` — and the layout sheet decides what to hide. Presentation
only: the annotation Sink requires `urn:cap:annotate` whatever the page shows.

★ **Looking at these pages is a tool, not a ceremony:**
`cargo run --example page-preview -- <out-dir> [root=<path>]` writes the root list, a tree
page and a file page as files — the same bytes the door serves, with htmx's first swap
already done and both stylesheets beside them — under a capability the example states in one
place. It exists because the browse pages cannot be opened by hand: the only way to hold
`urn:cap:browse:read:*` over HTTP is a passkey ceremony in a browser. ⚠ It shows how a page
LOOKS and never what a caller may do; `tests/browse.rs` holds the second half. Serve the
directory with something that answers `/k…` a 403 and the page also shows what a caller who
may not use one affordance sees.

⚠ **A refusal from one affordance is not a page failure.** Every browse page fetches its
parts through affordances of its own, including a recent-pull-requests block that loads
itself; a caller without `urn:cap:exec:gh` gets a typed 403 there while the tree underneath
is fine. `web/gonk.js` writes such a message where the request came FROM — in place of that
block's "loading…", or beside the control that issued it — and only a 403 disables the
control, because an authority refusal is permanent for that grant while a 404 or a 503 is
not. ⚠ What this door still cannot do is decide UP FRONT whether a caller may use a control:
the kernel's rule for a wildcard requirement is `pub(crate)`, so "may this caller?" has no
public answer (ledger #439). Catching the refusal is the achievable half.

For the resources themselves — named ledgers, the grant table, delete versus purge, ordering
policies — see `ikigai-ledger`'s README.

## Port 1062

Port 1062 is the default for both HTTP (TCP) and QUIC (UDP), which are separate namespaces, so
one number names the server. `--port` and `--bind` move both doors (the QUIC door takes the HTTP
door's port unless a QUIC bind is named); `--quic-bind` moves QUIC alone.

**Until 2026-10-12 the default was 1060.** What the move changes for a server already running:

- **Clients name the new port.** A browser opens `http://localhost:1062/`; a QUIC client
  connects to `quic://<host>:1062` and a `mount` line names it; `roborev file` and
  `kata import` file over the socket and name no port. Passkeys are registered to the
  relying party `localhost`, which carries no port, and the origin a sign-in is checked against
  follows the port the server is serving on.
- **A server that must stay on 1060** says so: `gonk.port = 1060` (or `--port 1060`).
- **Old links still work.** Item and comment bodies carry thousands of links like
  `http://localhost:1060/l/default/item/739`. The store keeps every body exactly as written;
  when a page RENDERS one, a link at `http://localhost:1060` or `http://127.0.0.1:1060` whose
  path is one of this server's own routes (`/l/…`, `/iki/…`, `/browse…`, `/queue…`, the
  rest of `doors::GONK_PATHS`) gets a root-relative href, so it opens on the server showing the
  page, whatever port that is. Its visible text is unchanged, and a 1060 link to any other
  path is left alone (`render::rehome`).

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

### The access log

Every door writes one line per request to stderr, so under that agent a slow page can be read
back from `/tmp/ikigai-gonk.log` after the fact (ledger #739). The grammar is `ikigai-log`'s —
time, class, subject IRI, then `key=value` columns, every key on every line in this order:

```text
2026-10-06T04:57:53.855Z gonk:Access urn:iki:gonk:page:ledger:default door=http verb=source outcome=ok bytes=3943 dur=1468 principal=- q=as=text/html&status=all
2026-10-06T04:57:55.323Z gonk:Access urn:iki:ledger:append door=http verb=sink outcome=ok bytes=48 dur=2 principal=anon q=-
2026-10-06T04:57:53.886Z gonk:Access urn:iki:ledger:items door=socket verb=source outcome=ok bytes=46 dur=2 principal=owner q=-
```

`dur` is milliseconds and the time is when the request started; `grep ' dur=[0-9]\{4,\} '` is
every request that took a second or more. `outcome` is `ok` or the kernel's error kind — **not
the HTTP status**, which `ikigai-web` decides after the kernel returns and does not report
back. An HTTP request's `principal` is the signed-in passkey the door minted into its
capability, on reads as well as writes (ledger #1077); an anonymous read is `-`, and a
`?principal=` a reader types is never written as one (ledger #864, R6). A QUIC line names the client the connection authenticated,
`urn:iki:host:gonk:client:<fingerprint>` (ledger #816, #1183), on every verb and whatever
capability the client carries (ledger #879); a line written before 2026-10-10 says
`urn:iki:gonk:client:<fingerprint>`, and stays as written. A refusal the door kernel makes before dispatch (a capability
floor, a name nothing binds) writes no line. A foreign `Host`, a cross-site write and (ledger
#880) a GET another page loads are refused at the EDGE, before the kernel (which is what keeps
`OPTIONS` and `?description` from answering them), and the edge writes their line itself
(ledger #894): `outcome=denied bytes=- dur=0`, under the IRI the route table would have mapped
the path to, with the query in `q` and the principal of an anonymous request whatever cookie it
carried (a refused request's session is not believed). Between ledger #879 and #894 they wrote
none, so probing was invisible. No line ever carries a cookie, a token or a form
body. `gonk.log.access = false` turns it off; [`src/access.rs`](src/access.rs) has the rest.

★ **The Queue badge's poll is sampled** (ledger #767). Every open page fetches
`/queue/depth` every ten seconds, so one forgotten tab wrote 360 lines an hour into a file
launchd never rotates — 64% of the live log's lines on 2026-10-10. A poll now writes its line
only when it fails, when it takes a second or more (so the `dur=[0-9]\{4,\}` grep above still
finds every slow one), or when that poll has written no quiet line for ten minutes, to show it
is alive. The sample is per polled resource and by time (ledger #1718): the Queue badge, the
header's Layers count and an open Layers page's three-second poll each write one quiet line per
ten minutes, however many pages ask. Every other request is still one line. The file itself is still unrotated:
that is the launchd install's to do (`newsyslog`), not gonk's.

## Upgrading past audit round 4 (ledger #864)

What changed for an operator, in one place (ledger #864, #805, #816, #799):

**Now refused at startup**

- a `grants.json` grant naming a door's marker (`urn:iki:gonk:door:*`: a refusal, or the
  other-page label of ledger #880) or a
  QUIC client's name (`urn:iki:host:gonk:client:*`, or the old `urn:iki:gonk:client:*`) —
  both are computed by a door, never granted;
- an ARMED reviewer (`gonk.review.arm = true`) whose net grant names a host other than the
  one `gonk.mount` dials — `urn:cap:net:localhost` for a `quic://127.0.0.1:…` peer is the
  usual case. Write `urn:cap:net:<the mount's host>` (the refusal prints the stanza).

**Now refused at use, where it used to work**

- ⚠ **a net grant for another host.** A passkey or certificate whose grant holds
  `urn:cap:net:localhost` while the mount is `quic://127.0.0.1:4433` could explain and review
  through the kernel's prefix match on browse's wildcard; the mount now refuses it by name.
  Re-mint the role (`passkey invite <name> --browse derive --force`, which names this server's
  mount host and prints what it replaced) or edit the token in `grants.json` — it is re-read
  per request, no restart;
- the backup family on any grant, per connection and per request, not only at startup;
- a foreign `Host` (any method) and a cross-site write, before anything runs, the pages and
  the passkey ceremonies included: a `421 Misdirected Request` for the foreign `Host` (since
  ledger #2) and a `403` for the cross-site write;
- (ledger #880) a GET another page LOADS (an image, a `fetch`, a frame) from another site or
  another localhost port: a `403` at the edge. A GET another page NAVIGATES to gets only the
  read half of the caller's grant, so a link to an explanation from another page answers
  `403` and says why; open it from gonk's own page or type the address;
- a write whose `author` names another principal (`urn:iki:gonk:passkey:*`,
  `urn:iki:host:gonk:client:*`, or the old `urn:iki:gonk:client:*`), on every route.

**Moved**

- the QUIC door's default port follows the HTTP port. A server started with `--port N` (or
  `gonk.port = N`) and an enrolled certificate now listens on UDP `N`, not 1060 — clients of
  such a server connect to `N`. A server on the default port is unchanged;
- `client add --force` keeps the client's identity (it replaces the grant); `--rotate`
  replaces the identity.

**New surface**: `client list`, `client remove <name> | --fingerprint <fp>`, `client add
--rotate | --port | --quic-bind`, and `--config-home`, `--data-home`, `--store` (see [Install
and run](#install-and-run)). The access log names a QUIC request's client as its `principal`.

## Not built

- **Layer bindings (ledger #1190) have no status resource and no `urn:iki:gonk:feed`.** A
  binding says what it does on stderr (once per distinct problem); the change feed of ledger
  #1144 is not built from the mirror's listener here. Nor is an auto-take policy, nor a way
  for a gonk to ask the layer door who it is (a lost-reply take before this server's first
  take cannot be told apart from someone else's).
- **No manifold-driven forms yet.** The forms are hand-written in the stylesheet. They post
  only actions and inputs the ledger declares, and `/act` refuses anything else, but a new
  ledger input does not appear on the page until the stylesheet names it.
- **No graph visualization**, no live updates (a change made elsewhere shows on the next
  load), and no page without JavaScript for the in-place forms.
- **No certificate lifecycle beyond the commands.** `client list`, `client remove` and
  `client add --rotate` exist; there is no CA, no expiry and no distribution. See the QUIC
  section for what admission means.
- **No live reload of trusted certificates.** Grants and enrolments are re-read per QUIC
  connection; the certificate set is read at startup.
- **One trace per door.** A traced call through a door records the forward, not the hub's
  resolution beneath it.
- **No browse graph on the `/sparql` page — the join is reachable, the EDITOR is not where.**
  Since `ikigai-store` 0.2.5 a scoped read takes a set of graphs, so the join runs through
  every door: the socket door's owner holds root, and a QUIC certificate or a passkey identity
  whose grant carries both read tokens runs it with no root anywhere — over HTTP as the
  store's own resource under **this server's** mechanical path mapping
  (`GET /iki/store/graph-select?graph=<A>%20<B>&query=…` on port 1062, which
  `the_join_runs_through_the_http_door_under_a_signed_in_grant` drives end to end). ⚠ An
  earlier revision credited that path to `ikigai-web`. It is gonk's own door's mapping;
  `ikigai-web` writes the whole IRI in the path instead (`GET
  /urn:iki:store:graph-select?graph=…`, over a `mount = "prefer urn:iki:store:=…gonk.sock"`
  line), and mixing the two spellings earns a 404 that reads like a missing resource. What is
  not built is the join from the PAGE: the editor's box picks a LEDGER and the query runs
  against that ledger's graph alone, so `urn:iki:browse:graph:default` is invisible there
  whatever tokens the caller holds. A graph selector rather than a ledger selector is what
  that would take, and it is its own decision — the page would have to offer the graphs a
  capability can read (`urn:iki:store:graphs` answers exactly that) rather than the ledgers.
- **No refusal for a graph outside the set.** `GRAPH <other>` inside a scoped read matches
  nothing rather than erroring, so a query naming a graph the `graph=` argument left out is
  answered, emptily. `ikigai-store` reports it; nothing here can see it.
- **No archived explanation without a net grant.** `urn:repo:{root}:explain` is ONE action
  whether it derives or serves an entry the archive already holds, so the `urn:cap:net:*` it
  declares is required either way. `version=` provably derives nothing (`ikigai-browse`
  answers `NotFound` on a miss rather than falling back to a model), so through that ROW there
  is still no way to grant "may read what was already paid for" without also granting "may
  spend". ★ Since the graph decision there is a way around it that is not a hole:
  `--browse-graph read` reads the archive as QUADS through `urn:iki:store:graph-select`, with
  no net grant — the text without the spending. It is not the browse face (no rendering, no
  `version=` resolution, no file), so the grain the HTTP browse face has to answer is
  narrower than it was, not gone.
- **No explanation archive to start from, and no sync with one.** A gonk with browse roots
  configured begins with an empty browse graph and pays in inference for the first explanation
  of everything. An archive another host already derived over the same directories can be
  CARRIED in — see *Importing an archive from another host's store* — but that import is a
  one-shot: it is an operator running a tool with the server stopped, not a subscription.
  Anything the source derives afterwards stays there until the import is run again, and
  nothing here notices the difference. ⚠ Two hosts serving the same roots from two archives
  therefore DIVERGE silently, which is an argument for retiring the source rather than
  running both.
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
- **Backups sit on the disk they protect.** Off-machine is a separate decision and has not
  been made. `urn:iki:gonk:backup:archive:{name}` is what makes one copyable through the
  socket without linking a filesystem family into the process that holds the dataset, so the
  mechanism exists and the policy does not.
- **No encryption at rest.** An archive is mode `0600` and is the whole dataset in one file —
  every graph the per-graph capabilities exist to separate.
  [`ikigai-encrypt`](https://github.com/ikigai-rs/ikigai-encrypt) is the shape that would fix
  it and is not linked.
- **A backup is whole, never incremental.** Five rotations of the whole dataset, on a store
  measured in megabytes. Nothing here would scale to a store measured in gigabytes: the dump
  is materialized in memory and ordered, which is what buys byte-identical archives.
- **No RocksDB checkpoint.** The fast, engine-coupled snapshot (hard links, consistent,
  opaque) is the right tool before a risky migration and is not built; the portable RDF
  archive is the one that still works when the engine has moved or the store will not open.
- **A restore does not adopt.** It builds a store beside the live one and tells you what it
  verified; swapping it in is manual, with the server stopped.
- **No fetching in the server.** gonk browses what is on disk and watches it; it never pulls.
  Keeping managed checkouts current is `ikigai-gonk checkout --all` on a timer (see
  *Keeping them current*), a command outside the server.

The page's htmx is htmx 2.0.4 (Zero-Clause BSD), vendored as `web/htmx.min.js`.

## Accessibility

The browser face aims at WCAG 2.2 AA and makes no conformance claim.
[ACCESSIBILITY.md](ACCESSIBILITY.md) says what is in scope, what has been tested and with what,
the barriers we know about, and how to report one (an issue with the `accessibility` label).

## License

MIT OR Apache-2.0.
