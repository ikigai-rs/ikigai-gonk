# gonk on a fresh machine

This runbook takes a Mac or a Linux box with nothing ikigai on it to a running gonk: a ledger
you can file work in, a repository you can browse, and, if you have a local model server,
explanations and reviews of that repository. It needs only crates.io, GitHub and the commands
below; every configuration file it names is one a step here tells you to write.

The [README](../README.md) is the reference for what each part does and why. This page is the
order to do it in. The outputs shown were captured by following it on macOS on 2026-10-10
(gonk at `9240be8`, `ikigai-cli` 0.1.43 from crates.io), with home directories shortened to
`/Users/you` and ports put back to their defaults; they are abbreviated with `…`.

## What you need

- **A Rust toolchain**, current stable, from <https://rustup.rs>.
- **A C and C++ compiler and libclang.** The store is RocksDB, built from source by
  `oxrocksdb-sys`, which runs `bindgen`. On macOS the Xcode command line tools are enough
  (`xcode-select --install`). On Debian or Ubuntu: `sudo apt install build-essential clang
  libclang-dev git`. (Not yet tried on a bare Linux box: CI builds on GitHub's
  `ubuntu-latest` image, which already carries these.)
- **`git`** on `PATH`. The browse family runs it to read a repository's state.
- **Optional:** an OpenAI-compatible model server on this machine (Ollama, `mlx_lm.server`,
  LM Studio, vLLM, the `llama.cpp` server) for [explain and review](#6-optional-explain-and-review).

## 1. Install the two binaries

gonk is not on crates.io yet, so it installs from GitHub. `--locked` builds the dependency
versions gonk's CI built; without it, cargo resolves a new set that nothing has tested.

```sh
cargo install --locked --git https://github.com/ikigai-rs/ikigai-gonk ikigai-gonk
cargo install --locked ikigai-cli --features quic
```

The first is the server. The second is `ikigai`, the command line that talks to it over its
socket, makes certificates, and (in step 6) serves your model to gonk. The `quic` feature
is what gives it `cert generate`, `serve quic://…` and `--connect quic://…`.

```sh
ikigai-gonk --help
ikigai --version
```

## 2. The two homes

gonk reads no environment variables. It reads two directories:

| home | default | holds |
| --- | --- | --- |
| config home | `~/.config/ikigai` (or `$XDG_CONFIG_HOME/ikigai`) | `config.toml` (every `gonk.*` key), `gonk/grants.json`, `gonk/clients.json`, `gonk/quic/` |
| data home | `~/.ikigai` | the store (`store/`), the socket (`gonk.sock`), `backups/` |

Neither has to exist first, and no configuration is needed to start: gonk creates what it
writes. Create the config file now anyway, empty, because steps 5 and 6 add lines to it:

```sh
mkdir -p ~/.config/ikigai
touch ~/.config/ikigai/config.toml
```

The file is `key = "value"` lines, one per line, repeatable where a key says so. A comment
goes on a line of its own: a `#` after a value becomes part of the value.

## 3. Start it

```sh
ikigai-gonk
```

It runs in the foreground and prints a banner naming every door and what it grants. With an
empty config home:

```text
ikigai-gonk 0.1.0 — holding the store at /Users/you/.ikigai/store
  http    http://localhost:1060/ — loopback (127.0.0.1:1060); anonymous read+write: default; 0 passkey(s); anonymous SPARQL budget 1000 ms
  browse  not composed — no gonk.browse.root; urn:repo:* and the git/gh facades are not bound
  backup  every 24h into /Users/you/.ikigai/backups — keep 5, one due now (catching up in 1m). …
  llm     not mounted — no gonk.mount; urn:llm:* resolves nowhere, so explain, review and the PR-derived layers are not bound
  review  not configured — no gonk.review.space; no queue is bound and urn:iki:gonk:review:pass is not offered
  …
  socket  /Users/you/.ikigai/gonk.sock — owner only
  quic    off — no client certificate is enrolled (…); to open it, run `ikigai-gonk client add <name> --ledger <ledger>=write` and restart
  mount   mount = "prefer urn:iki:ledger:=/Users/you/.ikigai/gonk.sock"  (and the same for urn:iki:store:)
```

Leave it running and use a second terminal for the rest. To stop it, Ctrl-C.

⚠ **One process holds the store.** RocksDB takes an exclusive lock on `~/.ikigai/store`, so
a second gonk (or an `ikigai` with `store = true` in its config) on the same directory is
refused. Every other ikigai process reaches the store through gonk's socket, as in step 4.

To keep it running across logins, put the bare binary under your supervisor: the README's
[Running it as a service](../README.md#running-it-as-a-service) has a `launchd` agent; a
systemd user unit is the same idea (`ExecStart=%h/.cargo/bin/ikigai-gonk`, `Restart=always`).
It takes no arguments there: everything comes from the config home.

## 4. Use the ledger

**In a browser:** open <http://localhost:1060/>. Use `localhost`, not `127.0.0.1` (passkeys
in step 5 are bound to the name). An anonymous caller on this machine may read and write the
`default` ledger: file an item from the form, open it, comment, close it.

**Over HTTP**, the mechanical routes for programs:

```sh
curl -X POST --data-binary 'Try gonk on a fresh machine' http://127.0.0.1:1060/iki/ledger/append
curl -X POST --data-binary 'Started it from the runbook' 'http://127.0.0.1:1060/iki/ledger/comment?item=1'
curl http://127.0.0.1:1060/iki/ledger/next
curl -X POST 'http://127.0.0.1:1060/iki/ledger/close?item=1&reason=done'
curl http://127.0.0.1:1060/iki/ledger/item/1
```

```text
#1 urn:iki:ledger:default:item:01m4k1sdhp9vv593rtqbdcm
commented on #1 (urn:iki:ledger:default:comment:01m4k1sqdp9vv593rtqbdcn)
 1.    #1  open    p-  Try gonk on a fresh machine
…
closed #1 (done)
   #1  closed  p-  Try gonk on a fresh machine  (done)
…
1 comment(s):
  2026-10-10T13:59:48.406Z (unattributed)
    Started it from the runbook
```

**From the `ikigai` command line**, through the owner-only socket. The socket door runs
with the owner's full authority, so it can do everything the HTTP door refuses an anonymous
caller:

```sh
printf 'Filed from the cli' | ikigai --connect ~/.ikigai/gonk.sock -c 'sink urn:iki:ledger:append' -c 'source urn:iki:ledger:items'
```

To put the ledger into every `ikigai` process on this machine instead of naming the socket
each time, add the two lines the banner prints to `~/.config/ikigai/config.toml` (spell out
your home directory):

```toml
mount = "prefer urn:iki:store:=/Users/you/.ikigai/gonk.sock"
mount = "prefer urn:iki:ledger:=/Users/you/.ikigai/gonk.sock"
```

gonk itself ignores `mount` lines; only `ikigai` reads them.

## 5. Browse a repository

A browse root is one line naming a repository directory. It must already exist; gonk refuses
to start with a root whose directory is missing. Clone one, or let gonk do it:

```sh
ikigai-gonk checkout https://github.com/ikigai-rs/ikigai-gonk.git --write-config
```

That clones into `~/.ikigai/checkouts/ikigai-gonk` and appends the line to `config.toml`.
For a repository you already have, write the line yourself (the name may not contain `:`,
`/`, `{` or `}`):

```toml
gonk.browse.root = "demo=/Users/you/src/demo"
```

Restart gonk (Ctrl-C, then `ikigai-gonk`): it reads its roots at startup. The banner now
says:

```text
  browse  urn:repo:{demo (watched)}:* — annotations and archive in <urn:iki:browse:graph:default>
```

**Over the socket**, which needs no grant:

```sh
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:repo:demo:tree' -c 'source urn:repo:demo:file:README.md' -c 'source urn:repo:demo:state'
```

```text
.git	dir	-
src	dir	-
README.md	file	51
…
# demo

A scratch repository for the gonk runbook.
…
eedab90be25077cf31ad1a8562e164f0ab4de23e clean
```

**In a browser** you need an identity. The anonymous caller holds ledger tokens only, so
<http://localhost:1060/browse> says *Sign in to browse the repositories*, and a direct read
answers `403`. Invite yourself, with the browse role:

```sh
ikigai-gonk passkey invite me --ledger default=delete --browse read
```

```text
passkey invite for `me` — grant `me` (8 scopes) in /Users/you/.config/ikigai/gonk/grants.json
  valid for 30 minutes, once
  open  http://localhost:1060/#invite=…
```

Open that link on this machine, click **Create passkey** and approve the browser's sheet
(Touch ID, a security key or a phone), then click **Sign in with passkey** in the header and
approve it again. No restart is needed. The header now carries a **Browse** link. The README's
[Passkeys](../README.md#passkeys-who-you-are-and-what-that-grants) section walks through what
each click shows.

## 6. Optional: explain and review

Explain (an orientation for a file or directory, archived per content version) and review
(findings minted as annotations) need a model. gonk does not link a model client: it
**mounts** `urn:llm:*` from a second process, an `ikigai serve quic://…` that holds the
model configuration and lends it. The two processes trust each other by certificate, in both
directions. These steps run that peer on the same machine; for a model on another machine,
change the address in 6.4 and 6.5 and nothing else.

### 6.1 Tell `ikigai` about the model server

Write `~/.config/ikigai/llm.json`, where `ikigai` reads its model providers. gonk asks two by
name:
`urn:llm:coder:ask` for files and reviews, and `urn:llm:ask` (the default provider) for
directories. One provider can be both:

```json
{
  "default": "coder",
  "providers": {
    "coder": { "base_url": "http://127.0.0.1:11434/v1", "model": "qwen3-coder:30b" }
  }
}
```

`base_url` is your server's OpenAI-compatible root (Ollama's is `http://127.0.0.1:11434/v1`)
and `model` a model it has. ⚠ **Spell the host `127.0.0.1`**, the same as in 6.4 and 6.5.
See [why the host's spelling matters](#why-the-hosts-spelling-matters). (This page was
checked against an MLX server speaking the same API on `127.0.0.1:8000`, not Ollama, which is
why the outputs below name a `qwen3.8-27b-4bit` model.)

### 6.2 The peer's identity

```sh
ikigai cert generate
```

```text
wrote server.{crt,key} and client.{crt,key} to /Users/you/.config/ikigai/quic
```

### 6.3 gonk's identity for the mount, and the exchange

Generate a second identity for gonk to present, give its certificate to the peer, and give
the peer's certificate to gonk:

```sh
ikigai cert generate --dir ~/.config/ikigai/gonk/quic/peers/llm
mkdir -p ~/.config/ikigai/quic/clients
cp ~/.config/ikigai/gonk/quic/peers/llm/client.crt ~/.config/ikigai/quic/clients/gonk.crt
cp ~/.config/ikigai/quic/server.crt ~/.config/ikigai/gonk/quic/peers/llm/server.crt
```

The first command also writes a `server.key` into `peers/llm/` that nothing uses; the last
command replaces its `server.crt` with the peer's, which is the certificate gonk pins.

### 6.4 Start the peer

In a third terminal:

```sh
ikigai serve quic://127.0.0.1:4433 --cap urn:cap:net:127.0.0.1 --no-config-mounts
```

```text
ikigai: mount   declined (--no-config-mounts) — the config home's `mount` lines are not read
ikigai: client  base  9545e69c5d7dc4ef  /Users/you/.config/ikigai/quic/client.crt
ikigai: client  gonk  fb94067b331d7a9f  /Users/you/.config/ikigai/quic/clients/gonk.crt
ikigai: serving on quic://127.0.0.1:4433  (fixed ceiling: urn:cap:net:127.0.0.1; surface: host + fs + llm)  (Ctrl-C to stop)
```

- `--cap urn:cap:net:127.0.0.1` is the ceiling every connection is clamped to. A net grant is
  what makes `urn:llm:*` servable at all (without one the surface line has no `llm`), and it
  is the only authority this peer lends.
- `--no-config-mounts` keeps the peer a leaf. Without it, the peer reads the `mount` lines
  from step 4 and mounts gonk's socket while gonk mounts the peer, and each waits out the
  other's self-description timeout.
- The `client gonk` line is the certificate from 6.3. The peer reads its trusted
  certificates at startup, so copy them before starting it.
- ⚠ **Restart gonk whenever you restart the peer.** gonk keeps its connection to the peer,
  and after the peer was restarted underneath it the next explain took **304 seconds**
  (measured 2026-10-10), waiting on the old connection, before a fresh one answered in 8.

### 6.5 Mount it in gonk

Add to `~/.config/ikigai/config.toml`, spelling out your home directory:

```toml
gonk.mount = "prefer urn:llm:=quic://127.0.0.1:4433 /Users/you/.config/ikigai/gonk/quic/peers/llm"
```

The certificate directory must exist (6.3 made it); gonk refuses to start, naming the line,
when it does not. Restart gonk. The banner says the explanation family is bound:

```text
  llm     quic://127.0.0.1:4433 (prefer; dialed on first use) — explain/review bound; file urn:llm:coder:ask @400, dir urn:llm:ask @600, review urn:llm:coder:ask @800, pr urn:llm:coder:ask @600 tokens. Deriving needs urn:cap:net:127.0.0.1, which `--browse derive` mints and nothing else does
```

gonk does not dial until the first explain, so a peer that is down costs explain and nothing
else.

### 6.6 Explain and review

Over the socket:

```sh
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:repo:demo:explain:src/main.rs'
ikigai --connect ~/.ikigai/gonk.sock -c 'source urn:repo:demo:explain-versions:src/main.rs' -c 'source urn:repo:demo:review:src/main.rs'
```

```text
This is the entry point for a Rust application, containing the single `main` function …
[uncacheable]
code-v1@qwen3.8-27b-4bit	sha256:35e0…	qwen3.8-27b-4bit	2026-10-10T14:02:31.597Z
[uncacheable]
review by qwen3.8-27b-4bit · review-v5@qwen3.8-27b-4bit · nothing above threshold · reviewed the whole file (37 bytes)
--- annotations (0) ---
```

The second read of an unchanged file comes from the archive, with no model call.

In a browser, the **Explain** and **Review** buttons need a grant that may spend the peer's
inference, which is the `derive` role. `grants --browse derive` prints what it holds on this
server; the net token is read from your `gonk.mount` line:

```sh
ikigai-gonk grants --browse derive
```

```text
[
  "urn:cap:browse:read:*",
  "urn:cap:store:read:graph:urn:iki:browse:graph:default",
  "urn:cap:annotate",
  "urn:cap:net:127.0.0.1"
]
```

Widen the grant `me` from step 5 to it. Without `--force` this is refused, naming the two
scopes it would add:

```sh
ikigai-gonk passkey invite me --ledger default=delete --browse derive --force
```

```text
passkey invite for `me` — grant `me` (10 scopes) in /Users/you/.config/ikigai/gonk/grants.json
  REPLACED     grant `me` (--force) — every identity enrolled under it holds the new list from its next request or connection:
    adds     urn:cap:annotate
    adds     urn:cap:net:127.0.0.1
  …
```

The passkey you already enrolled holds the new grant from its next request, so you do not
need to open the new link; it expires unused.

### Why the host's spelling matters

Three places name a host, and they have to agree:

1. gonk's `gonk.mount` target. The `derive` role's net token is minted from it
   (`urn:cap:net:127.0.0.1` for `quic://127.0.0.1:4433`).
2. The peer's `--cap`. A request arrives carrying its caller's capability and is clamped to
   this ceiling, so a caller holding `urn:cap:net:127.0.0.1` against a ceiling of
   `urn:cap:net:localhost` arrives holding no net grant at all.
3. The host in `llm.json`'s `base_url`, which the model client checks what is left against.

Mixing `localhost` and `127.0.0.1` fails in two ways, both measured:

| `gonk.mount` | peer `--cap` | `base_url` | over the socket | as a `--browse derive` identity |
| --- | --- | --- | --- | --- |
| `127.0.0.1` | `localhost` | `localhost` | works | `denied: capability does not grant urn:cap:net:*` |
| `127.0.0.1` | `127.0.0.1` | `localhost` | `denied: … does not allow reaching localhost (needs urn:cap:net:localhost)` | refused the same way |
| `127.0.0.1` | `127.0.0.1` | `127.0.0.1` | works | works |

The first row is the trap: the socket holds the owner's whole authority, so the setup looks
finished from the command line, and only the browser's Explain button is refused.

## Trying it beside an existing gonk

Everything above assumes the default homes. To follow this page on a machine that already
runs gonk, give the second one its own homes and port, and keep them in its `config.toml`
so every subcommand reads the same thing:

```sh
ikigai-gonk --config-home ./cfg --data-home ./data
```

```toml
# ./cfg/config.toml
gonk.port = 1070
gonk.socket = "/tmp/gonk2/g.sock"
```

- `--port` (or `gonk.port`) moves the HTTP door and, once a client is enrolled, the QUIC
  door, which follows it.
- ⚠ **A Unix socket path must be shorter than 104 bytes** on macOS. A deep scratch directory
  overruns that, so name `gonk.socket` under a short `/tmp/…` path. Put it in `config.toml`
  rather than passing `--socket`: `passkey invite` takes no `--socket`, works out the default
  socket under `--data-home`, and refuses to run when that default is too long, even though
  it never opens the socket.
- `client`, `passkey invite`, `checkout`, `review request` and `grants --browse` take the
  same `--config-home` and `--data-home`.
- `ikigai` (0.1.43 and later) takes `--config-home` too, so the model peer can use its own:
  `ikigai --config-home ./peer cert generate`, `ikigai --config-home ./peer serve …`.
