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
```

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
[`ikigai-ledger`](https://github.com/ikigai-rs/ikigai-ledger), and the `ikigai-web`,
`ikigai-ipc` and `ikigai-quic` transports. For the browser face it adds two libraries it calls
as functions and binds no resources from: `ikigai-xslt` (the stylesheet engine) and
`ikigai-passkey` (the assertion verifier). No filesystem module, no process execution and no
outbound network client is compiled in, so none is reachable whatever a grant says.
`tests/conformance.rs` pins the socket and QUIC catalog to the store's twelve resources and the
ledger's fourteen, pins the HTTP door's to those plus its ten pages, and walks
`ikigai-conformance` over the hub, a door, and the HTTP door.

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

The page's htmx is htmx 2.0.4 (Zero-Clause BSD), vendored as `web/htmx.min.js`.

## License

MIT OR Apache-2.0.
