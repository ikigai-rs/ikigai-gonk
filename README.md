# ikigai-gonk

**A standalone work-ledger server for [ikigai](https://github.com/ikigai-rs).** One binary
holds the machine's durable RDF store and serves named ledgers — items, comments, labels,
links, claims, and a ranked `next` — through three doors: HTTP on loopback port 1060, an
owner-only Unix socket, and QUIC for other machines, where every connection is admitted by a
client certificate and runs under the grant that certificate is enrolled for.

```text
$ ikigai-gonk
ikigai-gonk 0.1.0 — holding the store at /Users/you/.ikigai/store
  http    http://127.0.0.1:1060 — loopback; ledgers read+write: default
  socket  /Users/you/.ikigai/gonk.sock — owner only
  quic    off — no /Users/you/.config/ikigai/gonk/clients.json (enrol a client with `ikigai-gonk client add`)
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

## Filing and reading work over HTTP

Every resource of [`ikigai-ledger`](https://github.com/ikigai-rs/ikigai-ledger) is reachable
by the mechanical mapping `ikigai-web` uses: the path's segments become the IRI, the method
the verb, query parameters the arguments, the body the content, and `Accept` the
representation (`as` and `content` are the transport's own names, so a query parameter
spelled either is ignored).

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
| `DELETE` | `/iki/ledger/item/01m2h…` | refused on this door — see below |

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

The QUIC door opens once a client is enrolled. On the gonk machine:

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
| **HTTP**, `127.0.0.1:1060` | any process on this machine | the narrow read and write tokens of the ledgers in `gonk.http.ledger` (default: `default`) — and an empty capability for any peer that is not loopback |
| **socket**, `~/.ikigai/gonk.sock` | this user only (`0600`, peer UID checked) | root — the socket's owner can already read the dataset's files |
| **QUIC**, `udp 0.0.0.0:1060` | a certificate under `gonk/quic/clients/` | the grant its fingerprint maps to in `gonk/clients.json`; refused when it maps to none |

**The HTTP door is unauthenticated, so it is loopback only and grants little.** It can list,
file, comment, close, claim, label and link in its ledgers. It cannot delete or purge, cannot
reach another ledger, and cannot touch the store's whole-dataset doors
(`urn:iki:store:select`, `urn:iki:store:update`), because none of those tokens are in its
grant. Its capability is computed per request from the connection, so an authenticated
identity — a passkey session — replaces the loopback check without moving the seam. That is
also what can make a non-loopback bind meaningful; until then there is none.

**The socket and QUIC doors share one kernel with HTTP.** A kernel owns its cache and its
golden threads, and both transports take one by value; three kernels over one store would
let a write through one door leave another serving the read it cached before. So there is
one kernel, and the socket and QUIC doors forward every request to it under a cache that
stores nothing.

## What it refuses

- **A non-loopback HTTP bind.** `--bind 0.0.0.0:1060` stops the server before it opens
  anything, and says to use QUIC.
- **A second holder of the store.** Another gonk, or an `ikigai` with `store = true`, holding
  the dataset stops this one with the path and the mount lines that fix it.
- **The store's broad tokens in any grant.** `urn:cap:store:read` is every graph and
  `urn:cap:store:write` is `DROP ALL`; a `grants.json` naming either stops the server at
  startup, and a connection whose grant names one is refused. The store's narrow write door
  refuses the broad key anyway, so such a grant would also be one that cannot write.
- **A certificate it trusts but has no grant for**, or whose grant is unknown or empty. The
  refusal is logged with the full fingerprint.
- **An unreadable authority file.** A `clients.json` or `grants.json` that exists and does
  not parse stops the server rather than serving under a guess.
- **A `gonk.*` config key it does not read**, and a socket path too long to bind.

## Configuration

Flags override config wholesale; there is no environment-variable channel. The config home
is `~/.config/ikigai` (or `$XDG_CONFIG_HOME/ikigai`).

```toml
# ~/.config/ikigai/config.toml
gonk.bind = "127.0.0.1:1060"          # or gonk.port = 1060, which always means loopback
gonk.socket = "~/.ikigai/gonk.sock"
gonk.quic.bind = "0.0.0.0:1060"
gonk.http.ledger = "default"          # repeatable
```

| file | what it holds |
| --- | --- |
| `store.toml`, `gonk.store.toml` | `path = "…"` — where the dataset lives, read by `ikigai-store` |
| `gonk/clients.json` | certificate fingerprint → grant name (the same shape `ikigai serve quic://…` reads) |
| `gonk/grants.json` | grant name → capability scopes (the same shape as the cli's `grants.json`) |
| `gonk/quic/` | `server.crt`, `server.key`, and one `clients/<name>/` bundle per client |

## What it composes

The manifest is the module manifest: gonk links
[`ikigai-store`](https://github.com/ikigai-rs/ikigai-store) with its RocksDB backend,
[`ikigai-ledger`](https://github.com/ikigai-rs/ikigai-ledger), and the `ikigai-web`,
`ikigai-ipc` and `ikigai-quic` transports — nothing else. No filesystem module, no process
execution, no outbound network client is compiled in, so none is reachable whatever a grant
says. `tests/conformance.rs` pins the served catalog to the store's twelve resources and the
ledger's fourteen and walks `ikigai-conformance` over the kernel `main` serves, both directly
and through a door.

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

- **No HTML face, no SPARQL route, no identity on HTTP.** The HTTP door serves the ledger's
  text and Turtle faces through the mechanical mapping.
- **No certificate lifecycle.** No CA, expiry, rotation or distribution; see the QUIC
  section for what admission means.
- **No live reload of trusted certificates.** Grants and enrolments are re-read per QUIC
  connection; the certificate set is read at startup.
- **One trace per door.** A traced call through the socket or QUIC door records the forward,
  not the hub's resolution beneath it.

## License

MIT OR Apache-2.0.
