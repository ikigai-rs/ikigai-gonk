# Push at the door: a live Dashboard without polling

Ledger item `default#2968`, part of epic `default#3189`. This is a design note with no code. It
ends in build items, each with its tests, for the hub to file.

Siblings: `default#3200` covers non-browser clients (the socket and QUIC doors), and
`default#3201` covers the cost of a store-wide cut. `default#2967` built the polling this
design replaces.

## What is true today (checked on `main` at `6992d7a`)

The item claims gonk's door has no push stream. That holds, for three separate reasons, and
each one alone would be enough:

1. **No stream is declared.** `doors::edge_config_with` builds an `EdgeConfig` with
   `push: None` (it falls through to `..EdgeConfig::default()`). `ikigai-web` 0.2.3 serves
   `/_ikigai/push` only when `EdgeConfig::push` is `Some`.
2. **No caller may listen.** `ikigai-web`'s stream calls `Kernel::listen` under the capability
   that `CapFn` grants the request. It answers `403` unless that capability holds
   `urn:cap:kernel:listen` (`ikigai_core::CAP_LISTEN`), and it never adds that capability
   itself. `doors::http_scopes_for` grants it to nobody. The only listener in the process is
   `mirror::spawn_listener`, which listens at root on the hub. It is not a door.
3. **The door kernel would hear nothing.** The stream listens on the kernel it serves, which is
   the HTTP door kernel (`doors::http_kernel_serving_with_space`). That kernel runs under
   `doors::NoCache`, so it stores no entry and no cut ever reaches it. Core's rule is that a
   non-root listener hears a cut only when an entry computed under its own capability depends
   on the cut thread. The door kernel has no entries, so it hears nothing. The hub does cache,
   and it caches per viewer: `HubSpace` forwards each read with `inv.capability`
   (`doors.rs:258`). But the push stream never looks at the hub.

A fourth fact shapes the design. **The Dashboard's fragments are `no-store`**
(`Expiry::Always`, `src/dashboard.rs`). Core learns that an answer depends on a thread only
when it stores the answer. So even a caching door kernel would hold nothing for a Dashboard
fragment, and `ikigai-web` says it directly: "a fragment answered `no-store` hangs from nothing
and must be polled."

The fifth fact sets the cost. **A ledger write cuts every ledger's counts.** A store read rests
on the store's write thread, not on one ledger's thread. `tests/dashboard_2542.rs` measures
this: one write to `beta` costs the next load four store queries, one per ledger.

## The decision in one paragraph

Give the HTTP door kernel a small cache. It admits only live fragments: answers that are
cacheable, hang from at least one thread and sit under a declared prefix. A single **cut relay**
listens at root on the hub and replays every hub cut into the door kernel with `Kernel::cut`.
Turn on `EdgeConfig::push` at the door. Add `urn:cap:kernel:listen` to the capability of a
signed-in session that may read at least one ledger. Make the Dashboard's fragments cacheable in
the kernel. They then hang from the hub threads they read and are heard when a write cuts them.
Pages declare `data-ui-push` and `hx-trigger="ikigai:cut"`, and `ikigai-ui`'s `shell.js`,
which every gonk page already loads, keeps one `EventSource` open per page. Anonymous callers
and failed streams fall back to the polling we have now. The socket and QUIC doors keep
`NoCache`.

## 1. How a door kernel listens on the ledgers' golden threads

### Options considered

| option | what it takes | why not, or why |
|---|---|---|
| A. The door's stream listens on the **hub** | a new `ikigai-web` hook, so `PushConfig` can name the kernel to listen on | The hub never caches gonk's fragments, because pages live in the door's space in front of the hub. A `path=` subscription would therefore never match: the edge matches by what a cut **invalidated**. The page would have to subscribe by `thread=`, which `ikigai-ui`'s `data-ui-push` cannot say, and the page would need to know the store's thread names. Rejected. |
| B. Move gonk's pages **into the hub**, cached there | moving `crate::web` into the hub's composition | The socket and QUIC doors would then serve pages. It also reverses `doors.rs`'s rule that pages are not a hub concern. Rejected. |
| C. **A cut relay into a caching door kernel** | a root listener on the hub, `Kernel::cut` on the door kernel, and a narrow `CachePolicy` | The edge, `ikigai-ui` and core are all used as published. `path=` matches, because the fragment is the cached entry. **Chosen.** |

### Why the relay is safe, and where it is not

`doors.rs` gives one cache per process for a reason. A second cache serves stale reads,
indefinitely, if no cut reaches it. The relay is what makes the second cache acceptable, so its
limits must be stated:

- **Every hub cut reaches the door.** The relay listens at **root** with prefix `""`, so it
  hears every cut in the hub's single cut order (core's rule: root hears every cut its spec
  matches). It calls `door.cut(event.thread)` for each event. The thread names are the same in
  both kernels because the door's cached fragment carries the threads of the hub reads it is
  made from. `HubSpace::new` (the HTTP flavor) hands representations back as the hub made them,
  in process, with their threads. Only the wire flavor (`for_the_wire`) drops them.
- **Lost history clears everything.** Each door-cached fragment also depends on one sentinel
  thread, `urn:iki:gonk:door:epoch`. When the relay's batch reports `dropped > 0`, it cuts the
  sentinel, which empties every live fragment in the door. The edge already sends viewers a
  `resync` when their own stream drops.
- **A cut during a compute is handled.** Core handles a cut that lands while an entry is being
  computed: the entry is stale on arrival (`ikigai-core` `cache.rs`, the raced-entry path).
  A relayed cut is an ordinary cut on the door kernel, so the same path covers it.
- ⚠ **There is a window.** Between the hub's cut and the relay's `door.cut`, a door read can be
  served the old fragment. The window is one listener wake-up on a dedicated thread, the same
  shape as `mirror::spawn_listener`. It is bounded by that thread's latency, and the cut event
  that ends it also tells the page to refetch, so the page lands on the new value. To keep the
  window off the read-your-writes path, **only fragments are admitted**. Whole pages, item
  pages, form answers and the redirect after a write keep going straight to the hub. A write
  submitted through the door therefore never reads back from the door cache.
- **The relay starts before the door does.** If the relay could not be registered (the hub
  refused, or the runtime failed), the door kernel keeps `NoCache` and `push` stays off. The
  System tab says so. A door cache with no relay is the stale cache `doors.rs` forbids, so that
  combination must not be able to exist (build item 2's test).

### What the door caches

`doors::LiveFragments` replaces `NoCache` on the HTTP door kernel only. It admits an entry when
all of these hold:

- its target is under a live prefix: gonk's `urn:iki:gonk:fragment:`, plus each prefix a face
  declares (see section 3);
- it is cacheable (not `Expiry::Always`);
- it hangs from at least one thread.

It holds a bounded number of entries, 4096 to start, with the least recently used as the
victim. Each fragment is keyed by the viewer's capability partition, which it already is
because the key is core's. So the bound protects memory and nothing else.

The Cache tab (`crate::operator`) reads `urn:kernel:inspect` on the hub. It gains a second
section for the door's cache, so an operator can see both caches.

### What the Dashboard's fragments change

The chart fragments, the agent tables and the agents note become `.cacheable()` in the kernel,
with the threads of the reads they are composed from. In HTTP terms, `ikigai-web` already
projects "cacheable with threads" as `no-cache` with a strong `ETag`. Today they are
`no-store`, a choice `src/dashboard.rs` made so that no browser cache holds a viewer's numbers.
`no-cache` would let the browser keep the bytes on disk. Keeping `no-store` on the wire while
the entry is cacheable in the kernel needs an `ikigai-web` knob: either a route that overrides
`Cache-Control` to `no-store`, or a `private, no-store` projection for a capability-scoped read.
That knob is build item 1. `gonk.js` already sends `If-None-Match` itself, so nothing on the
page depends on the browser's cache.

⚠ **The agents panel changes once a minute without a cut**, because a link's query holds the
current minute (`crate::agents`). A deadline is not a cut, and the edge says plainly that it
does not push deadlines. The agents fragments therefore carry `Expiry::At` (the end of the
minute) **and** their threads. They are pushed on a cut and refetched once a minute by a timer.
That timer is the one poll left on a listening page, and it is the edge's documented job split.
A follow-up could take the minute out of the links instead (open question 2).

## 2. Which grant carries `urn:cap:kernel:listen`, and for whom

**For a signed-in passkey session whose capability reads at least one ledger. It is added by
`http_scopes_for`, not written into `grants.json`.**

- **Why it is derived, not granted.** Core's rule means a listener learns only the names of
  threads its own capability already read. The scope therefore discloses nothing a session
  could not learn by polling. What it costs is a held connection, which is a resource question
  and not an authority question (section 4). If it were a separately granted scope, an operator
  would have to remember to add it to every grant, and the failure would be silent: a stream
  that `403`s falls back to polling and nobody notices.
- **Why the capability must carry it, not the edge.** `ikigai-web` is explicit here. The edge
  never adds `CAP_LISTEN`, because a capability with it added is a different capability, keyed
  to a different cache partition, and would hear nothing the page read. The page's reads and its
  stream must therefore be issued under the same capability, which is the one `http_cap`
  computes. Adding the scope there changes every signed-in session's partition once, when it
  ships, which costs one cold load each.
- **Anonymous loopback callers do not get it in the first build.** Any local process is an
  anonymous loopback caller, and each stream holds a connection slot for up to the idle timeout.
  Anonymous pages keep polling. If they should be pushed later, that is one line in
  `http_scopes_for` plus build item 6's stream bound. It is not a design change.
- **A non-loopback peer gets nothing**, as today. Admission (`http_admit_with`) runs ahead of
  the push stream in `ikigai-web` (`lib.rs`, "ADMISSION, ahead of every answer"), so a foreign
  `Host` or a cross-site read is refused before a listener is registered.
- **The operator can turn it off.** A `gonk.push = off` setting in the config leaves
  `EdgeConfig::push` at `None` and drops the scope, and every page falls back to polling.
- **The socket and QUIC doors are `default#3200`'s.** Neither has a stream to declare, and a
  grant there means a different transport. This design does not touch `clients.json` or
  `grants.json`.

## 3. How `data-ui-push` attaches to gonk's pages

`ikigai-ui` 0.4.1's `shell.js` already does the client side, and gonk links it on every page
(`ACCESSIBILITY.md`: "three files under `/static/` are `ikigai-ui`'s bytes"). It reads
`<meta name="ikigai-ui-push" content="…">`, opens one `EventSource` for every element that
declares `data-ui-push`, sends `ikigai:cut` to the elements whose paths a `cut` names, sends it
to all of them on a `resync`, keeps focus across the swap, and keeps those requests quiet.

### gonk's own pages

- `web/gonk.xsl` writes `<meta name="ikigai-ui-push" content="/_ikigai/push">` in `<head>`
  only when the viewer's capability holds `CAP_LISTEN` and push is on. Otherwise no stream is
  opened, which is `shell.js`'s documented behavior.
- `dashboard::polled` decides the trigger per viewer:
  - **A listening viewer:** `data-ui-push` (empty, so it uses its own `hx-get`) with
    `hx-trigger="ikigai:cut, gonk:refresh, every 5m"`. The 5-minute ask is a safety net for a
    stream that died without a `resync`. It costs one `304` per fragment.
  - **Anyone else:** exactly today's `every 30s, gonk:refresh`.
- **The pause control still pauses, as WCAG 2.2.2 requires.** `gonk.js` already holds every ask
  while paused, while the tab is hidden and while a panel holds focus, and it asks at once on
  resume (`tests/browser/dashboard_poll_2967.mjs`). An `ikigai:cut` is an ask like any other, so
  the same hold covers it. A cut that arrives while paused is remembered as "something changed"
  and asked once on resume. The hidden tab's fragments (`Tab`'s empty, `hidden` panel) declare
  nothing, so the stream does not subscribe them.
- **The one polite sentence does not change.** "Counts updated." is said once per real update,
  never for a `304`, whether a cut or a timer caused the ask.
- **With no script** the page is as of its load and says so, as now.

### A deployment's own pages, served by gonk

A deployment's pages come in as faces (`http_kernel_serving`'s `faces`, between gonk's pages
and the hub). They are built with `ikigai_ui::Pages` and opt in with
`Pages::push("/_ikigai/push")`. Their fragments declare `data-ui-push` and
`hx-trigger="ikigai:cut"`. For their fragments to be heard, two things must hold:

- the face declares its live prefix, and `LiveFragments` admits answers under it (the face API
  gains `live_prefix()`, with no prefix meaning nothing is admitted);
- the face's fragments are cacheable and read through this kernel, which a face's reads already
  are ("What a face reads through this kernel reaches the hub as every page read does, under the
  caller's capability").

gonk documents `/_ikigai/push` as a stable path. `PushConfig::path` stays at its default
(`ikigai_web::DEFAULT_PUSH_PATH`), so a deployment's `Pages::push` call and gonk agree without
configuration.

## 4. Fan-out and back-pressure for many viewers

### What one write costs

Take V listening viewers, each showing the Ledgers tab with L charts.

- **Events:** one store write produces one cut of the store's write thread in the hub. The relay
  replays it once into the door, and each of the V streams sends one `cut` event (the edge
  batches the events a listener drains together).
- **Refetches:** each viewer refetches its L charts, so V × L requests.
- **Store reads:** each viewer's partition holds its own counts, because `http_cap` mints the
  principal into the capability, so no two signed-in sessions share a partition. The cost is
  therefore up to **V × L store queries per write**, and it is the same for a write to any
  ledger. This is `default#3201`'s subject. Polling pays the same, spread over 30 seconds. Push
  pays it within a moment of every write.

### What this design does about it

1. **Coalesce on the page.** `gonk.js` holds `ikigai:cut` for a short window (250 ms, with up
   to 250 ms of random jitter) and allows at most one request in flight per fragment. A burst of
   writes (a batch, a worker's report and close) then becomes one refetch per fragment, and V
   viewers do not arrive in the same millisecond.
2. **Coalesce in the relay.** The relay drains the hub listener in batches and replays each
   distinct thread once per batch. A burst then becomes one door cut per thread.
3. **Narrow the threads.** This is the real fix, and it belongs to `default#3201`: per-ledger
   threads for counts, so a write to `beta` cuts only `beta`'s chart. Push makes that item more
   valuable, but neither blocks the other. The design works with today's store-wide thread and
   gets cheaper when `default#3201` lands.
4. **Partition sharing** (open question 1) would turn V × L into G × L, where G is the number of
   distinct grants. It is not proposed here.

### Bounds that already exist, and the ones to add

| bound | where | value |
|---|---|---|
| events a stream may queue before `resync` | `PushConfig::queue` (core's `LISTEN_CAPACITY`) | 256 |
| subscriptions per stream (more is a `400`) | `PushConfig::max_subscriptions` | 64; the Ledgers tab subscribes one per readable ledger plus the note, so gonk asserts its page stays under this |
| a reader that stops reading | `EdgeConfig::write_timeout` | the door's existing value |
| an idle stream | `PushConfig::idle_timeout` | 10 minutes, then the client reconnects and gets a `resync`, which costs a round of `304`s |
| relay queue | the relay's `ListenSpec::capacity` | 4096; on a drop it cuts the epoch sentinel (section 1) |
| door cache | `LiveFragments` | 4096 entries, least recently used evicted |
| **streams as a share of connections** | **new**: `PushConfig::max_streams` in `ikigai-web` | half of `max_connections`, so streams can never take every slot from ordinary requests. Today each stream holds a slot under `max_connections` (256) with nothing reserved |
| **streams per principal** | **new**: gonk's `admit_fn`, counting open streams per principal | 8 (eight tabs); the ninth is a `429`, and that page falls back to polling |

A browser allows six HTTP/1.1 connections per origin. One stream per tab is therefore a real
limit for someone with many gonk tabs open. The 5-minute safety poll covers a tab whose stream
could not open. Moving to HTTP/2 at the door is out of scope.

## 5. Relation to the cut stream from the back-requests spike (`default#2963`)

I have only the ledger's mention of `default#2963` to go on: "the cut stream raised by the
back-requests spike". I have not read that spike's text, so this section states constraints and
not conclusions.

- **There is one hub listener per purpose, and the relay is the door's.** If `default#2963`
  needs the hub's cut order in process (to recompute back-requests ahead of a reader, which is
  core's "recalculating golden thread" use of `listen`), it registers its own root listener as
  `mirror` does. It does not reuse the relay: the relay's job is replay, and a slow recompute
  must not delay the door's cuts.
- **There is one event shape.** If `default#2963` ever exposes cuts to clients, the shape is
  core's `CutEvent` as `ikigai-web` already serializes it (`thread`, `sequence`, `invalidated`,
  `more`, and `resync` on a drop). `sequence` is the hub's cut order in both places, so a
  client of both can compare them.
- **Non-browser streams are `default#3200`'s** whether they come from `default#2963` or not.

## Open questions (not blocking the build items)

1. **Partition sharing.** Should viewers with identical scopes share a partition, with the
   principal kept out of the cache key? That changes core's keying, not gonk's, and it would
   also change what a cut event may name. It needs its own design if `default#3201` does not
   make it moot.
2. **The agents' minute.** If the links in the agents tables stopped carrying the current
   minute, the last timer on a listening page would go away. That is `crate::agents`'s decision.
3. **Anonymous push.** This is one line once build item 6's bounds exist. Decide after
   measuring how many anonymous Dashboards are actually open.

## Build items

Each item lists what its tests prove. The items are ordered by dependency. Item 1 is an
upstream `ikigai-web` change and item 9 is measurement; every other item is in this repository.

1. **ikigai-web: `no-store` on the wire for a kernel-cacheable answer, and
   `PushConfig::max_streams`.** A route-level (or capability-scoped) `Cache-Control: no-store`
   override that keeps `ETag` and `If-None-Match` handling. Also a cap on concurrent streams,
   where the next stream gets a `503` with `Retry-After`.
   *Tests (in ikigai-web):* a cacheable answer with threads, on an overriding route, is sent as
   `no-store` with an `ETag` and revalidates to `304`; the N+1th stream is refused while
   ordinary `GET`s still succeed with N streams open.

2. **gonk: the cut relay and `LiveFragments`, with push still off.** `doors::LiveFragments`
   (the admission rule, LRU, 4096), the relay thread (a root listener on the hub with prefix
   `""`, replaying into the HTTP door kernel, with the epoch sentinel on a drop), and the
   wiring that makes "door cache without relay" impossible to construct.
   *Tests:* a write through the **socket** door to a ledger, then a read of a cached fragment
   through the HTTP door, returns the new value (the stale-cache failure `doors.rs` warns of,
   pinned as fixed); the admission rule admits a cacheable, threaded `urn:iki:gonk:fragment:`
   and refuses a page, a `no-store` answer, a thread-free answer and an unlisted prefix; a
   forced relay drop cuts the sentinel and the next read recomputes; a refused relay leaves the
   door on `NoCache` (asserted through `urn:kernel:inspect`); `tests/topology.rs` still passes
   (the policy has no identity); the socket and QUIC kernels still admit nothing.

3. **gonk: the Dashboard's fragments become cacheable.** The charts, agent tables and agents
   note carry their reads' threads (the agents fragments also carry `Expiry::At` at the end of
   the minute), and they use item 1's `no-store` override on the wire.
   *Tests:* `tests/dashboard_poll_2967.rs` keeps passing unchanged (a `304` while unchanged, a
   `200` after a write, a `403` for an unreadable ledger whatever the validator); a fragment's
   wire headers are still `no-store` with an `ETag`; the door cache's inspect shows one entry per
   fragment per viewer; `tests/dashboard_2542.rs`'s cost table is restated for a load served
   from the door cache.

4. **gonk: `urn:cap:kernel:listen` for signed-in readers, and `EdgeConfig::push` on.**
   `http_scopes_for` adds it for a passkey session that may read at least one ledger.
   `edge_config_with` sets `push: Some(PushConfig { .. })` unless `gonk.push = off`.
   *Tests:* an anonymous loopback stream is a `403`; a signed-in reader's stream answers
   `text/event-stream` and `ready`; a write cuts and the stream sends a `cut` naming
   `/dashboard/chart/{ledger}` in `paths`; a reader of `alpha` only hears no cut that names
   `beta`'s fragment, and no event names an entry outside its partition; a cross-site or
   foreign-`Host` stream is refused before a listener is registered (counted on the hub's
   listener count); `gonk.push = off` gives a `404` at the path and no scope; the catalog tests
   (`tests/conformance.rs`, `tests/browse.rs`) are unchanged.

5. **gonk: the Dashboard listens.** `web/gonk.xsl` writes the `ikigai-ui-push` meta for a
   listening viewer. `dashboard::polled` writes `data-ui-push` with
   `hx-trigger="ikigai:cut, gonk:refresh, every 5m"` for that viewer and today's trigger for
   anyone else. `gonk.js` holds `ikigai:cut` under the pause, the hidden tab and focus, asks
   once on resume if a cut came while paused, and coalesces (250 ms plus jitter, one request in
   flight per fragment).
   *Tests:* rendered markup per viewer (listening, anonymous, push off); the hidden panel
   declares no `data-ui-push`; the Ledgers tab's subscriptions stay under
   `max_subscriptions` for the configured ledgers; `gonk.js` wiring pinned as WIRING, as #2967
   does. `tests/browser/dashboard_push_2968.mjs` (local, as #2967's is) checks that a close
   updates the chart with no timer due, focus and scroll stay, `#status` says "Counts updated."
   once, a pause holds a cut and resume asks once, a hidden tab asks nothing, a `resync`
   refetches everything shown, and axe reports no violation in four themes at 1280 and 320 px.
   `ACCESSIBILITY.md` gains its rows.

6. **gonk: stream bounds.** Per-principal stream count in `admit_fn` (8, `429` after), with
   `max_streams` from item 1 set to half of `max_connections`.
   *Tests:* the ninth stream for one principal is a `429` and another principal's first stream
   still opens; closing a stream frees its count (including a client that vanishes at
   `write_timeout`); with streams at the cap, an ordinary page `GET` still answers.

7. **gonk: the Cache and System tabs show push.** The Cache tab adds the door cache (entries,
   admissions, evictions). The System tab shows whether push is on, the relay's state (running,
   or refused and why), open streams, and relay drops.
   *Tests:* each figure renders for an operator and is absent for a non-operator, the tab's
   existing `403` rule holds, and a refused relay says so in words.

8. **gonk: faces opt in.** A face declares `live_prefix()`, `LiveFragments` admits under it,
   and the face's `Pages::push("/_ikigai/push")` works end to end.
   *Tests:* a test face built with `ikigai_ui::Pages::push` whose fragment is heard after a
   ledger write; a face that declares no prefix is never cached at the door; a face cannot
   declare a prefix under `urn:iki:gonk:` (refused at construction).

9. **gonk: measure the fan-out.** A bench in `benches/` with V streams (10, 50, 200) and one
   write per second. It reports events sent, refetches, store queries and p95 time from write to
   the last `cut` event, before and after `default#3201`.
   *Tests:* the bench runs in CI at V = 10 as a smoke test. The numbers go in the pull request,
   not in assertions.

Items 2 and 3 can land with push still off. They change no behavior a viewer can see except
fewer recomputes, so the relay can be observed in production before any stream depends on it.
