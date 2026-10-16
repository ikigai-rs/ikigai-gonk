//! The Layers page: a person reads and posts layer traffic where they already look (ledger
//! [#1675](http://localhost:1062/l/default/item/1675)).
//!
//! ```toml
//! gonk.layers.view = "quic://ikigai-rs.dev:4434 ~/.ikigai/layers-client/brian"
//! ```
//!
//! One line names a layer host's door and the certificate directory of the identity the page
//! reads and posts AS (`client.crt`, `client.key` and the `server.crt` it pins, laid out as
//! `ikigai --cert-dir` expects). Read once, at start. Without it the page says it is not
//! configured and how to configure it.
//!
//! ```text
//! /layers                    urn:iki:gonk:page:layers                    Source  every layer the identity reads
//! /layers/{layer}            urn:iki:gonk:page:layer:{layer}             Source  one layer: entries, or tuples and takes
//! /layers/~/wait             urn:iki:gonk:fragment:layers-wait           Source  an open page's short poll (layer=, after=, door=)
//! /layers/~/count            urn:iki:gonk:fragment:layers-count          Source  the header's unread count (seen=, open=, quiet=)
//! /layers/{layer}/say        urn:iki:gonk:layers:say:{layer}             Sink    a form → one entry, said as the identity
//! /layers/{layer}/file/{id}  urn:iki:gonk:layers:file:{layer}:{id}       Source  a file entry's bytes, checked first
//! ```
//!
//! # ★ The identity is the CONFIGURED one, and nothing else in gonk can use it
//!
//! The door this view talks through ([`LayersView`]'s [`Door`]) is NOT mounted into gonk's
//! kernel: no `urn:layer:` name resolves through it from the socket, QUIC, a script or the
//! model. Only this module's endpoints call it, and only with names under [`PREFIXES`]
//! (`urn:layer:`, `urn:layers:`, `urn:collab:`, the three the hub's MCP entry overrides);
//! `Confined` refuses anything else before it leaves. It never falls back to gonk's own
//! machine identity: a `gonk.layer` binding's certificate directory is refused here
//! ([`check_not_a_binding`]), and the page posts only when the door says the identity is a
//! PERSON (`urn:layers:me`'s `kind`), so what Brian posts is stamped as Brian.
//!
//! # ★ Who may use the page: a signed-in passkey holding the page's own tokens
//!
//! gonk's HTTP face answers any process on this machine, and an anonymous loopback caller holds
//! the configured ledgers' tokens. The layers are someone's conversations, and a post here is
//! stamped as that someone, so neither half is anonymous: reading needs [`CAP_READ`] and
//! posting [`CAP_SAY`], which this server mints for NO anonymous caller. A passkey's grant
//! carries them (`grants.json`), the way `urn:cap:annotate` carries the Queue. A write is
//! a `POST` and passes the door's same-origin rule (`crate::doors::http_refusal`) like every
//! other write; another page's navigation keeps neither token (`crate::admit::FOREIGN_PAGE_KEEPS`).
//!
//! # ★ One long poll at the door, whatever the browser does
//!
//! A follower thread ([`LayersView::start`]) holds the one `urn:layers:feed after=<cursor>
//! wait=50` for every layer the identity may follow, and reads ONLY what an event says is new:
//! `entries after=<last seq read>`, never from 0 on every load (an entries page is O(entries)
//! at the door, and nothing is ever removed). A page and its long poll read this process's
//! copy; an unreachable door is one banner ([`Snapshot::down`]), not an error per read.
//!
//! # ★ An open page asks every three seconds, and holds nothing in between (ledger [#1705](http://localhost:1062/l/default/item/1705))
//!
//! The page's watcher asks [`WAIT_PATH`] [`LIVE_EVERY_SECS`] after it lands, and the answer
//! comes AT ONCE from this process's copy, carrying the next watcher. It used to be a long poll
//! that gonk held for up to 45 seconds: one of the browser's six HTTP/1.1 connections to this
//! origin per open Layers tab, so six open tabs left none for a click. A short poll holds none
//! between asks, and the cost is up to three seconds before a new entry shows, which a
//! conversation can afford. And it used to be a chain that a single failed ask (a network
//! error, gonk restarting) ended for good, silently; now `web/gonk.js` re-arms a failed ask
//! with a backoff (5 seconds, doubling, at most 60) and says so once, in words, in the page's
//! `#layers-reconnect` status, and stops with a sentence saying why when the answer is a
//! refusal no retry can fix (a 403 after gonk restarted and forgot the sign-in, a 404).
//!
//! # Unread
//!
//! Per viewer (the signed-in passkey's IRI) and per layer, the last feed position the viewer
//! was shown, kept in `<data home>/layers-view/cursors.json`: opening a layer, or a live update
//! arriving on its open page, moves it. The overview counts the events after it.
//!
//! # ★ The header's count: a short poll of this process's copy (ledger [#1700](http://localhost:1062/l/default/item/1700))
//!
//! Every gonk page a reader of the layers opens carries the unread total in its header's Layers
//! link ("Layers 3", named "Layers, 3 new"). It is [`COUNT_IRI`], polled at the Queue badge's
//! cadence (`crate::queue::BADGE_EVERY`, one request per open page whatever the number of
//! layers), and it reads ONLY this process's copy and the cursors: never the door, which the
//! follower alone reads. ⚠ **Not a long poll, on purpose.** A long poll on every page holds one
//! of the browser's six HTTP/1.1 connections to this origin per open tab for as long as it
//! waits, so six open gonk tabs would leave none for a click; a ten-second answer from memory
//! holds none. The cost is up to ten seconds of latency, which a header indicator can afford;
//! the open layer's own page polls every three seconds (above).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Request, Result, UriTemplate, Verb,
};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{Literal, NamedNode, Term, Triple};
use sha2::{Digest, Sha256};

use crate::layer::Door;
use crate::mount::{LazyMount, Target};
use crate::render::{self, element, envelope, wrap};
use crate::web::{self, Web};

/// The config key.
pub const KEY: &str = "gonk.layers.view";

/// Reading the page, its short poll and a file: held by no anonymous caller.
pub const CAP_READ: &str = "urn:cap:gonk:layers:read";

/// Posting through the page, as the configured identity.
pub const CAP_SAY: &str = "urn:cap:gonk:layers:say";

/// The overview.
pub const PAGE_IRI: &str = "urn:iki:gonk:page:layers";
/// Its path.
pub const PAGE_PATH: &str = "/layers";
/// One layer's page.
pub const LAYER_PAGE_TEMPLATE: &str = "urn:iki:gonk:page:layer:{layer}";
/// What an open Layers page asks every few seconds.
pub const WAIT_IRI: &str = "urn:iki:gonk:fragment:layers-wait";
/// Its path: `~` is not a layer-name character, so no layer's page can shadow it.
pub const WAIT_PATH: &str = "/layers/~/wait";
/// The header's unread count.
pub const COUNT_IRI: &str = "urn:iki:gonk:fragment:layers-count";
/// Its path, beside the page's own poll.
pub const COUNT_PATH: &str = "/layers/~/count";
/// The say adapter.
pub const SAY_TEMPLATE: &str = "urn:iki:gonk:layers:say:{layer}";
/// A checked file download.
pub const FILE_TEMPLATE: &str = "urn:iki:gonk:layers:file:{layer}:{id}";

/// The `Content-Security-Policy` a file download is served under: no script, no plugin, no
/// same-origin, nothing loaded, whatever the bytes are.
pub const FILE_CSP: &str = "sandbox; default-src 'none'";

/// The only names this view sends to its door.
pub const PREFIXES: [&str; 3] = ["urn:layer:", "urn:layers:", "urn:collab:"];

/// How long the follower's feed read asks the door to wait, in seconds: the brief's `wait=50`,
/// under the door's bound of 60.
pub const FEED_WAIT_SECS: u64 = 50;

/// How long an open Layers page waits between asks of gonk what changed: a SHORT poll of this
/// process's copy, answered at once (ledger [#1705](http://localhost:1062/l/default/item/1705)).
pub const LIVE_EVERY_SECS: u64 = 3;

/// The same while the door cannot be reached: nothing will change until it answers again.
pub const DOWN_EVERY_SECS: u64 = 15;

/// How many entries of a channel the page shows (and this process keeps), newest first.
pub const WINDOW: usize = 50;

/// How often the follower re-reads who the identity is and what it holds.
pub const REDISCOVER: Duration = Duration::from_secs(10 * 60);

/// How long a page waits for the FIRST read of the door before it renders what it has.
pub const FIRST_READ: Duration = Duration::from_secs(10);

/// The largest entries page asked for at once (the door's default).
const PAGE: u64 = 100;

/// The longest a file entry's bytes may be, decoded: the door's `MAX_ENTRY_MAX` (1 MiB).
const MAX_FILE_BYTES: usize = 1024 * 1024;

const IK: &str = "https://ikigai-rs.dev/ns#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const DCT_TITLE: &str = "http://purl.org/dc/terms/title";
const DCT_CREATED: &str = "http://purl.org/dc/terms/created";
const PROV_ATTRIBUTED: &str = "http://www.w3.org/ns/prov#wasAttributedTo";
const PROV_USED: &str = "http://www.w3.org/ns/prov#used";
const PROV_ASSOCIATED: &str = "http://www.w3.org/ns/prov#wasAssociatedWith";
const PROV_ENDED: &str = "http://www.w3.org/ns/prov#endedAtTime";
const PROV_ACTIVITY: &str = "http://www.w3.org/ns/prov#Activity";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_NON_NEGATIVE: &str = "http://www.w3.org/2001/XMLSchema#nonNegativeInteger";
const PARTICIPANT_PREFIX: &str = "urn:collab:participant:";

fn ik(local: &str) -> String {
    format!("{IK}{local}")
}

// ------------------------------------------------------------------------------ config

/// One `gonk.layers.view` line, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewConfig {
    /// The layer host's door and the identity presented there.
    pub target: Target,
}

/// Parse `gonk.layers.view`: `<quic://host:port> <cert dir>`. `home` is `$HOME`, for `~/`.
///
/// ```
/// use ikigai_gonk::layers_view::parse;
/// let certs = tempfile::tempdir().unwrap();
/// for file in ["client.crt", "client.key", "server.crt"] {
///     std::fs::write(certs.path().join(file), "pem").unwrap();
/// }
/// let line = format!("quic://127.0.0.1:4434 {}", certs.path().display());
/// let view = parse(&line, std::path::Path::new("/home/u")).unwrap();
/// assert_eq!(view.target.to_string(), "quic://127.0.0.1:4434");
/// assert!(parse("quic://h:1", std::path::Path::new("/home/u")).is_err());
/// assert!(parse(&format!("/tmp/x.sock {}", certs.path().display()), std::path::Path::new("/")).is_err());
/// ```
///
/// # Errors
///
/// A field missing or extra, a door that is not `quic://host:port`, a certificate directory
/// that is not one or lacks one of its three files.
pub fn parse(line: &str, home: &Path) -> std::result::Result<ViewConfig, String> {
    let shape = "expected `<quic://host:port> <certificate directory>`";
    let words: Vec<&str> = line.split_whitespace().collect();
    let [url, dir] = words[..] else {
        return Err(format!("{KEY} `{line}`: {shape}"));
    };
    let Some(authority) = url.strip_prefix("quic://").filter(|a| a.contains(':')) else {
        return Err(format!(
            "{KEY} `{line}`: the door is `quic://host:port`, not `{url}`: a layer host is \
             reached over QUIC with the certificate of the identity this page reads and posts as"
        ));
    };
    let certs = crate::mount::expand_home(dir, home);
    if !certs.is_dir() {
        return Err(format!(
            "{KEY} `{line}`: {} is not a directory (the client.crt, client.key and server.crt \
             of the identity this page reads and posts as)",
            certs.display()
        ));
    }
    for file in ["client.crt", "client.key", "server.crt"] {
        if !certs.join(file).is_file() {
            return Err(format!(
                "{KEY} `{line}`: {} has no {file}; the directory is a certificate set as \
                 `ikigai --cert-dir` expects it (client.crt, client.key, server.crt)",
                certs.display()
            ));
        }
    }
    Ok(ViewConfig {
        target: Target::Quic {
            authority: authority.to_string(),
            certs,
        },
    })
}

/// Refuse a view whose certificates are a `gonk.layer` binding's: that is THIS SERVER's own
/// identity at the layer door, and a person's page never speaks as the server.
///
/// # Errors
///
/// When `view` presents the same certificate directory as one of `bindings`.
pub fn check_not_a_binding(
    view: &ViewConfig,
    bindings: &[crate::layer::Binding],
) -> std::result::Result<(), String> {
    let Target::Quic { certs, .. } = &view.target else {
        return Ok(());
    };
    let same = |other: &Path| {
        let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        canonical(certs) == canonical(other)
    };
    for binding in bindings {
        if let Target::Quic { certs: theirs, .. } = &binding.target {
            if same(theirs) {
                return Err(format!(
                    "{KEY}: {} is the certificate directory gonk.layer `{}` presents, which is \
                     THIS SERVER's identity at the layer door. The Layers page reads and posts as \
                     a person: name that person's certificate set",
                    certs.display(),
                    binding.layer
                ));
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------ the door

/// A door that sends nothing outside [`PREFIXES`].
struct Confined(Arc<dyn Door>);

impl Door for Confined {
    fn call(&self, request: Request) -> std::result::Result<Representation, Error> {
        let target = request.target.as_str();
        if !PREFIXES.iter().any(|p| target.starts_with(p)) {
            return Err(Error::Denied(format!(
                "the Layers page sends only names under {} to its door, not {target}",
                PREFIXES.join(", ")
            )));
        }
        self.0.call(request)
    }
}

fn get(iri: &str, args: &[(&str, String)]) -> Request {
    let mut request = Request::new(
        Verb::Source,
        Iri::parse(iri).unwrap_or_else(|_| panic!("a layer IRI: {iri}")),
    );
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

fn text(representation: &Representation) -> String {
    String::from_utf8_lossy(&representation.bytes).into_owned()
}

fn json(representation: &Representation) -> std::result::Result<serde_json::Value, String> {
    serde_json::from_slice(&representation.bytes).map_err(|e| format!("not JSON: {e}"))
}

// ------------------------------------------------------------------------------ the data

/// A layer's access model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Model {
    /// Append-only entries.
    Channel,
    /// Tuples, taken atomically, with receipts.
    Tuplespace,
    /// A model this page does not know.
    Other(String),
}

/// What the identity holds on one layer, and what the layer is.
#[derive(Debug, Clone)]
pub struct LayerInfo {
    /// `urn:layer:{name}`.
    pub name: String,
    /// The definition's `rdfs:label`, or the name.
    pub label: String,
    /// The definition's `ik:layerModel`.
    pub model: Model,
    /// The verbs the identity's collaborations grant it here.
    pub verbs: BTreeSet<String>,
}

impl LayerInfo {
    fn holds(&self, verb: &str) -> bool {
        self.verbs.contains(verb)
    }
}

/// A file entry's description: never its bytes, which are read at download.
#[derive(Debug, Clone)]
pub struct FileMeta {
    /// `ik:fileName`.
    pub name: String,
    /// `ik:mediaType`.
    pub media_type: String,
    /// `ik:contentHash`, `sha256:<hex>`.
    pub hash: String,
    /// `ik:byteSize`.
    pub size: Option<u64>,
}

/// One entry of a channel, or one tuple of a tuplespace.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Its IRI.
    pub iri: String,
    /// The last segment of the IRI.
    pub id: String,
    /// `ik:seq`.
    pub seq: u64,
    /// The local name of its `rdf:type`.
    pub kind: String,
    /// `prov:wasAttributedTo`.
    pub who: String,
    /// `dcterms:created`.
    pub created: String,
    /// `dcterms:title`.
    pub title: Option<String>,
    /// `ik:body`, or a tuple's `ik:brief` or `ik:note`.
    pub body: Option<String>,
    /// `ik:repository`.
    pub repository: Option<String>,
    /// `ik:severity`'s local name.
    pub severity: Option<String>,
    /// `ik:inReplyTo`.
    pub in_reply_to: Option<String>,
    /// `ik:supersedes`.
    pub supersedes: Option<String>,
    /// `ik:addressedTo` (a tuple).
    pub addressed_to: Option<String>,
    /// `ik:offer` (a report on a tuple).
    pub offer: Option<String>,
    /// A file entry's description.
    pub file: Option<FileMeta>,
}

/// One take in a tuplespace.
#[derive(Debug, Clone)]
pub struct Receipt {
    /// The receipt's IRI.
    pub iri: String,
    /// `ik:seq`: the take's place in the feed.
    pub seq: u64,
    /// `prov:wasAssociatedWith`.
    pub taker: String,
    /// `prov:endedAtTime`.
    pub ended: String,
    /// `prov:used`: the tuple taken.
    pub tuple: String,
    /// The tuple's title, when the receipt carries it.
    pub title: Option<String>,
}

/// One layer as this process has read it.
#[derive(Debug, Clone)]
pub struct LayerView {
    /// What it is and what the identity holds.
    pub info: LayerInfo,
    /// The last feed position read.
    pub last: u64,
    /// Whether it has been read at all since it was discovered.
    pub synced: bool,
    /// A channel's newest entries, ascending by `seq`, at most [`WINDOW`].
    pub entries: Vec<Entry>,
    /// A tuplespace's pending tuples, ascending.
    pub tuples: Vec<Entry>,
    /// A tuplespace's recent takes, ascending.
    pub receipts: Vec<Receipt>,
}

/// Who the door says the identity is (`urn:layers:me`).
#[derive(Debug, Clone)]
pub struct Me {
    /// The principal the door minted.
    pub principal: String,
    /// `person`, `agent`, `machine` or `other`.
    pub kind: Option<String>,
}

/// What a page renders: a copy, taken under the lock and released before rendering.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Who the identity is, once read.
    pub me: Option<Me>,
    /// Every layer the identity may read, by name.
    pub layers: BTreeMap<String, LayerView>,
    /// Whether the door has been read once.
    pub discovered: bool,
    /// Why the door cannot be reached, when it cannot.
    pub down: Option<String>,
    /// Bumped on every change.
    pub epoch: u64,
}

// ------------------------------------------------------------------------------ cursors

/// The per-viewer, per-layer feed position each viewer was last shown.
struct Cursors {
    path: Option<PathBuf>,
    map: Mutex<BTreeMap<String, BTreeMap<String, u64>>>,
}

impl Cursors {
    fn open(path: Option<PathBuf>) -> Cursors {
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Cursors {
            path,
            map: Mutex::new(map),
        }
    }

    fn get(&self, viewer: &str, layer: &str) -> u64 {
        lock(&self.map)
            .get(viewer)
            .and_then(|m| m.get(layer))
            .copied()
            .unwrap_or(0)
    }

    /// Move `viewer`'s cursor on `layer` to `seq`. A cursor past the layer's end (a restored
    /// host) is pulled back to it.
    fn set(&self, viewer: &str, layer: &str, seq: u64) {
        let mut map = lock(&self.map);
        let slot = map
            .entry(viewer.to_string())
            .or_default()
            .entry(layer.to_string())
            .or_insert(0);
        if *slot == seq {
            return;
        }
        *slot = seq;
        if let Some(path) = &self.path {
            // Best effort, and said when it fails: an unread marker is a convenience, and a page
            // must not fail because its directory is read-only.
            if let Err(e) =
                write_atomically(path, &serde_json::to_vec_pretty(&*map).unwrap_or_default())
            {
                eprintln!(
                    "gonk: layers view: cannot save the unread cursors to {}: {e}",
                    path.display()
                );
            }
        }
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ------------------------------------------------------------------------------ the view

/// The Layers page's state: one door, one follower, one copy of what was read.
pub struct LayersView {
    door: Confined,
    /// The follower's long poll, on a door of its own: a QUIC door answers the calls on one
    /// connection ONE AT A TIME (ikigai-quic's `serve_bounded`), so a 50-second feed read on
    /// the same connection would hold every post and download behind it past their deadline.
    feed_door: Confined,
    origin: String,
    feed_wait: u64,
    cursors: Cursors,
    state: Mutex<Snapshot>,
    discovered_at: Mutex<Option<Instant>>,
    changed: Condvar,
    started: AtomicBool,
    /// Serializes reads of the door that change the copy (the follower's, a post's catch-up).
    sync: Mutex<()>,
}

/// How a test sizes the waits.
#[derive(Debug, Clone, Copy)]
pub struct Waits {
    /// The follower's `wait=` at the door, in seconds.
    pub feed_secs: u64,
}

impl Default for Waits {
    fn default() -> Waits {
        Waits {
            feed_secs: FEED_WAIT_SECS,
        }
    }
}

impl LayersView {
    /// A view over `door`, named `origin` in what the page says. `cursors` is where the unread
    /// cursors are kept (`None`: in memory only).
    pub fn new(
        door: Arc<dyn Door>,
        origin: String,
        cursors: Option<PathBuf>,
        waits: Waits,
    ) -> Arc<LayersView> {
        LayersView::with_feed_door(Arc::clone(&door), door, origin, cursors, waits)
    }

    /// [`LayersView::new`], with the follower's long poll on `feed`, a door of its own.
    pub fn with_feed_door(
        door: Arc<dyn Door>,
        feed: Arc<dyn Door>,
        origin: String,
        cursors: Option<PathBuf>,
        waits: Waits,
    ) -> Arc<LayersView> {
        Arc::new(LayersView {
            door: Confined(door),
            feed_door: Confined(feed),
            origin,
            feed_wait: waits.feed_secs.min(60),
            cursors: Cursors::open(cursors),
            state: Mutex::new(Snapshot::default()),
            discovered_at: Mutex::new(None),
            changed: Condvar::new(),
            started: AtomicBool::new(false),
            sync: Mutex::new(()),
        })
    }

    /// The view `gonk.layers.view` configures: two [`LazyMount`]s on its door (one for the
    /// follower's long poll, one for everything else), uncacheable, under the layer bindings'
    /// short call deadline, that nothing dials until the page is first used.
    pub fn connect(config: &ViewConfig, cursors: Option<PathBuf>) -> Arc<LayersView> {
        let door = || {
            Arc::new(
                LazyMount::fresh(config.target.clone())
                    .with_call_timeout(crate::layer::CALL_TIMEOUT),
            ) as Arc<dyn Door>
        };
        LayersView::with_feed_door(
            door(),
            door(),
            config.target.to_string(),
            cursors,
            Waits::default(),
        )
    }

    /// The door, as the page names it.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Start the follower, once. Called by the first page that needs the copy.
    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let view = Arc::clone(self);
        std::thread::Builder::new()
            .name("gonk-layers-view".to_string())
            .spawn(move || view.follow())
            .expect("a thread for the layers follower");
    }

    /// A copy of what was read.
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.state).clone()
    }

    fn changed_with(&self, update: impl FnOnce(&mut Snapshot) -> bool) {
        let mut state = lock(&self.state);
        if update(&mut state) {
            state.epoch += 1;
            self.changed.notify_all();
        }
    }

    fn set_down(&self, down: Option<String>) {
        self.changed_with(|s| {
            if s.down == down {
                return false;
            }
            s.down = down;
            true
        });
    }

    /// Block until the door has been read once (or found unreachable), at most `within`.
    pub fn wait_first_read(&self, within: Duration) {
        let deadline = Instant::now() + within;
        let mut state = lock(&self.state);
        while !state.discovered && state.down.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// What `viewer` has not been shown yet, across every layer but `except` (the layer whose
    /// page is open, which reads its entries as they arrive): `None` until the door has been
    /// read once, so "nothing known" is never drawn as "nothing new". Read from this process's
    /// copy and the cursors, never the door.
    pub fn unread(&self, viewer: &str, except: Option<&str>) -> Option<u64> {
        let state = lock(&self.state);
        if !state.discovered {
            return None;
        }
        Some(
            state
                .layers
                .values()
                .filter(|l| Some(l.info.name.as_str()) != except)
                .map(|l| l.last - self.cursors.get(viewer, &l.info.name).min(l.last))
                .sum(),
        )
    }

    /// Read the door now: discover when due, sync what is unsynced, catch up the layers no
    /// feed covers. The follower's step without its long poll; a test calls it to settle.
    ///
    /// # Errors
    ///
    /// What the door answered, as a sentence.
    pub fn refresh(&self) -> std::result::Result<(), String> {
        let _sync = lock(&self.sync);
        let due = lock(&self.discovered_at).is_none_or(|at| at.elapsed() >= REDISCOVER);
        if due {
            let (me, infos) = self.discover()?;
            *lock(&self.discovered_at) = Some(Instant::now());
            self.changed_with(|s| {
                s.me = Some(me);
                let mut layers = BTreeMap::new();
                for info in infos {
                    let name = info.name.clone();
                    let layer = match s.layers.remove(&name) {
                        Some(mut kept) if kept.info.model == info.model => {
                            kept.info = info;
                            kept
                        }
                        _ => LayerView {
                            info,
                            last: 0,
                            synced: false,
                            entries: Vec::new(),
                            tuples: Vec::new(),
                            receipts: Vec::new(),
                        },
                    };
                    layers.insert(name, layer);
                }
                s.layers = layers;
                true
            });
        }
        let unsynced: Vec<LayerInfo> = lock(&self.state)
            .layers
            .values()
            .filter(|l| !l.synced)
            .map(|l| l.info.clone())
            .collect();
        for info in unsynced {
            let last = self.feed_last(&info)?;
            self.catch_up(&info, 0, last)?;
        }
        let unfed: Vec<(LayerInfo, u64)> = lock(&self.state)
            .layers
            .values()
            .filter(|l| !l.info.holds("feed"))
            .map(|l| (l.info.clone(), l.last))
            .collect();
        for (info, last) in unfed {
            let now = self.feed_last(&info).unwrap_or(last);
            if now != last {
                self.catch_up(&info, last, now)?;
            }
        }
        // Read once means discovered AND every layer read: a page that waited for the first
        // read must not draw an unread layer as an empty one.
        self.changed_with(|s| !std::mem::replace(&mut s.discovered, true));
        self.set_down(None);
        Ok(())
    }

    /// `urn:layers:me`, then each collaboration's manifest, then each readable layer's
    /// definition.
    fn discover(&self) -> std::result::Result<(Me, Vec<LayerInfo>), String> {
        let me = self
            .door
            .call(get("urn:layers:me", &[("as", "application/json".into())]))
            .map_err(|e| format!("reading urn:layers:me: {e}"))?;
        let me = json(&me).map_err(|e| format!("urn:layers:me: {e}"))?;
        let principal = me["principal"]
            .as_str()
            .ok_or("urn:layers:me named no principal")?
            .to_string();
        let kind = me["kind"].as_str().map(str::to_string);
        let mut verbs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for collab in me["collaborations"].as_array().into_iter().flatten() {
            let Some(collab) = collab.as_str().filter(|c| c.starts_with("urn:collab:")) else {
                continue;
            };
            let manifest = self
                .door
                .call(get(collab, &[("as", "application/json".into())]))
                .map_err(|e| format!("reading {collab}: {e}"))?;
            let manifest = json(&manifest).map_err(|e| format!("{collab}: {e}"))?;
            for mount in manifest["mounts"].as_array().into_iter().flatten() {
                let (Some(layer), Some(held)) = (
                    mount["layer"].as_str(),
                    mount["grants"][principal.as_str()].as_array(),
                ) else {
                    continue;
                };
                if !crate::layer::is_layer_name(layer) {
                    continue;
                }
                verbs
                    .entry(layer.to_string())
                    .or_default()
                    .extend(held.iter().filter_map(|v| v.as_str()).map(str::to_string));
            }
        }
        let mut infos = Vec::new();
        for (name, held) in verbs {
            if !held.contains("read") {
                continue;
            }
            let iri = format!("urn:layer:{name}");
            let definition = self
                .door
                .call(get(&iri, &[]))
                .map_err(|e| format!("reading {iri}: {e}"))?;
            let graph = Graph::parse(&definition.bytes).map_err(|e| format!("{iri}: {e}"))?;
            let model = match graph.value(&iri, &ik("layerModel")).as_deref() {
                Some("channel") => Model::Channel,
                Some("tuplespace") => Model::Tuplespace,
                Some(other) => Model::Other(other.to_string()),
                None => Model::Other("unstated".to_string()),
            };
            infos.push(LayerInfo {
                label: graph
                    .value(&iri, RDFS_LABEL)
                    .unwrap_or_else(|| name.clone()),
                name,
                model,
                verbs: held,
            });
        }
        Ok((Me { principal, kind }, infos))
    }

    /// The layer's last feed position, from its own feed's JSON face (`after=0` answers it
    /// with at most the door's page of events, each a line; it is read once per sync).
    fn feed_last(&self, info: &LayerInfo) -> std::result::Result<u64, String> {
        if !info.holds("feed") {
            // Without the feed grant the last position is the newest entry's or tuple's seq;
            // the catch-up reads it from the entries themselves.
            return Ok(u64::MAX);
        }
        let iri = format!("urn:layer:{}:feed", info.name);
        let answer = self
            .door
            .call(get(
                &iri,
                &[("after", "0".into()), ("as", "application/json".into())],
            ))
            .map_err(|e| format!("reading {iri}: {e}"))?;
        json(&answer).map_err(|e| format!("{iri}: {e}"))?["last"]
            .as_u64()
            .ok_or_else(|| format!("{iri} named no `last`"))
    }

    /// Read what is new on one layer since `from`, up to `to` (`u64::MAX`: whatever there is),
    /// and keep it.
    fn catch_up(&self, info: &LayerInfo, from: u64, to: u64) -> std::result::Result<(), String> {
        match info.model {
            Model::Channel => {
                // From the cursor, never from 0: only the newest WINDOW are kept, so a first
                // read starts WINDOW before the end.
                let mut after = if from == 0 && to != u64::MAX {
                    to.saturating_sub(WINDOW as u64)
                } else {
                    from
                };
                let mut read = Vec::new();
                loop {
                    let iri = format!("urn:layer:{}:entries", info.name);
                    let answer = self
                        .door
                        .call(get(
                            &iri,
                            &[("after", after.to_string()), ("limit", PAGE.to_string())],
                        ))
                        .map_err(|e| format!("reading {iri}: {e}"))?;
                    let graph = Graph::parse(&answer.bytes).map_err(|e| format!("{iri}: {e}"))?;
                    let page = entries_of(&graph, &format!("urn:layer:{}:entry:", info.name));
                    let full = page.len() as u64 >= PAGE;
                    after = page.iter().map(|e| e.seq).max().unwrap_or(after);
                    read.extend(page);
                    if !full {
                        break;
                    }
                }
                let last = if to == u64::MAX { after } else { to.max(after) };
                self.changed_with(|s| {
                    let Some(layer) = s.layers.get_mut(&info.name) else {
                        return false;
                    };
                    let before = (layer.last, layer.entries.len(), layer.synced);
                    for entry in read {
                        if !layer.entries.iter().any(|e| e.iri == entry.iri) {
                            layer.entries.push(entry);
                        }
                    }
                    layer.entries.sort_by_key(|e| e.seq);
                    let excess = layer.entries.len().saturating_sub(WINDOW);
                    layer.entries.drain(..excess);
                    layer.last = layer.last.max(last);
                    layer.synced = true;
                    before != (layer.last, layer.entries.len(), layer.synced)
                });
            }
            Model::Tuplespace => {
                let iri = format!("urn:layer:{}:tuples", info.name);
                let pending = self
                    .door
                    .call(get(
                        &iri,
                        &[("limit", PAGE.to_string()), ("as", "text/turtle".into())],
                    ))
                    .map_err(|e| format!("reading {iri}: {e}"))?;
                let pending = Graph::parse(&pending.bytes).map_err(|e| format!("{iri}: {e}"))?;
                let tuples = entries_of(&pending, &format!("urn:layer:{}:", info.name));
                let window = if to == u64::MAX {
                    0
                } else {
                    to.saturating_sub(PAGE)
                };
                let taken = self
                    .door
                    .call(get(
                        &iri,
                        &[
                            ("state", "taken".into()),
                            ("after", window.to_string()),
                            ("limit", PAGE.to_string()),
                            ("as", "text/turtle".into()),
                        ],
                    ))
                    .map_err(|e| format!("reading {iri} state=taken: {e}"))?;
                let taken = Graph::parse(&taken.bytes).map_err(|e| format!("{iri}: {e}"))?;
                let receipts = receipts_of(&taken);
                let newest = tuples
                    .iter()
                    .map(|t| t.seq)
                    .chain(receipts.iter().map(|r| r.seq))
                    .max()
                    .unwrap_or(0);
                let last = if to == u64::MAX {
                    newest
                } else {
                    to.max(newest)
                };
                self.changed_with(|s| {
                    let Some(layer) = s.layers.get_mut(&info.name) else {
                        return false;
                    };
                    let changed = !layer.synced
                        || layer.last != last.max(layer.last)
                        || layer.tuples.len() != tuples.len()
                        || layer.receipts.len() != receipts.len()
                        || layer
                            .tuples
                            .iter()
                            .zip(&tuples)
                            .any(|(a, b)| a.iri != b.iri);
                    layer.tuples = tuples;
                    layer.receipts = receipts;
                    layer.last = layer.last.max(last);
                    layer.synced = true;
                    changed
                });
            }
            Model::Other(_) => {
                // Nothing this page can show; the overview says what the model is.
                self.changed_with(|s| match s.layers.get_mut(&info.name) {
                    Some(layer) if !layer.synced => {
                        layer.synced = true;
                        true
                    }
                    _ => false,
                });
            }
        }
        Ok(())
    }

    /// The follower: the one long poll at the door, for as long as the process lives.
    fn follow(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.step() {
                Ok(()) => backoff = Duration::from_secs(1),
                Err(why) => {
                    self.set_down(Some(why));
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    }

    fn step(&self) -> std::result::Result<(), String> {
        self.refresh()?;
        let cursor: Vec<String> = lock(&self.state)
            .layers
            .values()
            .filter(|l| l.synced && l.info.holds("feed"))
            .map(|l| format!("{}:{}", l.info.name, l.last))
            .collect();
        if cursor.is_empty() {
            std::thread::sleep(Duration::from_secs(self.feed_wait.max(1)));
            return Ok(());
        }
        let answer = self.feed_door.call(get(
            "urn:layers:feed",
            &[
                ("after", cursor.join(",")),
                ("wait", self.feed_wait.to_string()),
                ("as", "application/json".into()),
            ],
        ));
        let answer = match answer {
            Ok(answer) => answer,
            // A cursor past a layer's end: the host was restored or replaced. Read every
            // layer again from its feed.
            Err(Error::Conflict(_)) => {
                self.changed_with(|s| {
                    for layer in s.layers.values_mut() {
                        layer.synced = false;
                    }
                    true
                });
                return Ok(());
            }
            Err(e) => return Err(format!("following urn:layers:feed: {e}")),
        };
        let body = json(&answer).map_err(|e| format!("urn:layers:feed: {e}"))?;
        let mut touched: BTreeMap<String, u64> = BTreeMap::new();
        for event in body["events"].as_array().into_iter().flatten() {
            if let (Some(layer), Some(seq)) = (event["layer"].as_str(), event["seq"].as_u64()) {
                let top = touched.entry(layer.to_string()).or_insert(0);
                *top = (*top).max(seq);
            }
        }
        let _sync = lock(&self.sync);
        for (name, seq) in touched {
            let found = lock(&self.state)
                .layers
                .get(&name)
                .map(|l| (l.info.clone(), l.last));
            if let Some((info, last)) = found {
                self.catch_up(&info, last, seq)?;
            }
        }
        Ok(())
    }

    /// Post one message or question to `layer`, as the configured identity. Answers the new
    /// entry's IRI.
    fn say(&self, layer: &str, turtle: String) -> Result<String> {
        let iri = format!("urn:layer:{layer}:say");
        let request = Request::new(
            Verb::Sink,
            Iri::parse(&iri).map_err(|e| Error::Endpoint(e.to_string()))?,
        )
        .with_arg("content", ArgRef::Inline(turtle.into_bytes()))
        .with_arg("as", ArgRef::Inline(b"text/plain".to_vec()));
        let answer = self.door.call(request).map_err(|e| {
            door_refusal(
                e,
                &format!("The layer door at {} refused the post: ", self.origin),
            )
        })?;
        // A layer the follower does not follow is caught up here, so the post shows at once.
        let unfed = lock(&self.state)
            .layers
            .get(layer)
            .filter(|l| !l.info.holds("feed"))
            .map(|l| (l.info.clone(), l.last));
        if let Some((info, last)) = unfed {
            let _sync = lock(&self.sync);
            let _ = self.catch_up(&info, last, u64::MAX);
        }
        Ok(text(&answer).trim().to_string())
    }

    /// A file entry's bytes, read from the door and served only when they match the size and
    /// the `sha256:` digest the entry states (the door does not check them).
    fn file(&self, layer: &str, id: &str) -> Result<(FileMeta, Vec<u8>)> {
        let iri = format!("urn:layer:{layer}:entry:{id}");
        let answer = self.door.call(get(&iri, &[])).map_err(|e| {
            door_refusal(
                e,
                &format!(
                    "The layer door at {} did not answer for the file: ",
                    self.origin
                ),
            )
        })?;
        let graph = Graph::parse(&answer.bytes).map_err(Error::Endpoint)?;
        let entry = entries_of(&graph, &format!("urn:layer:{layer}:entry:"))
            .into_iter()
            .find(|e| e.iri == iri)
            .ok_or_else(|| Error::NotFound(format!("{iri} is not an entry")))?;
        let meta = entry
            .file
            .ok_or_else(|| Error::NotFound(format!("{iri} is not a file entry")))?;
        let content = graph
            .value(&iri, &ik("content"))
            .ok_or_else(|| Error::Conflict(format!("{iri} carries no content")))?;
        let bytes = match graph.value(&iri, &ik("encoding")).as_deref() {
            Some("text") => content.into_bytes(),
            Some("base64") => {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .decode(content.trim())
                    .map_err(|e| {
                        Error::Conflict(format!(
                            "refused: {iri}'s content is not base64 ({e}); nothing was sent"
                        ))
                    })?
            }
            other => {
                return Err(Error::Conflict(format!(
                    "refused: {iri}'s encoding is {}, not `text` or `base64`; nothing was sent",
                    other.unwrap_or("not stated")
                )))
            }
        };
        check_file(&meta, &bytes)
            .map_err(|why| Error::Conflict(format!("refused: {iri} {why}; nothing was sent")))?;
        Ok((meta, bytes))
    }
}

/// Whether `bytes` are the file `meta` describes: the stated size, and the stated
/// `sha256:<hex>` digest.
///
/// ```
/// use ikigai_gonk::layers_view::{check_file, FileMeta};
/// let meta = |hash: &str, size| FileMeta {
///     name: "a.txt".into(),
///     media_type: "text/plain".into(),
///     hash: hash.into(),
///     size: Some(size),
/// };
/// let good = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
/// assert!(check_file(&meta(good, 5), b"hello").is_ok());
/// assert!(check_file(&meta(good, 6), b"hello").unwrap_err().contains("size"));
/// assert!(check_file(&meta(good, 5), b"hellO").unwrap_err().contains("digest"));
/// ```
///
/// # Errors
///
/// A sentence naming what does not match.
pub fn check_file(meta: &FileMeta, bytes: &[u8]) -> std::result::Result<(), String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!(
            "is {} bytes, over the {MAX_FILE_BYTES} a file entry may be",
            bytes.len()
        ));
    }
    match meta.size {
        Some(size) if size == bytes.len() as u64 => {}
        Some(size) => {
            return Err(format!(
                "states a size of {size} bytes and its content is {}",
                bytes.len()
            ))
        }
        None => return Err("states no size".to_string()),
    }
    let digest = format!("sha256:{}", hex(&Sha256::digest(bytes)));
    if !digest.eq_ignore_ascii_case(meta.hash.trim()) {
        return Err(format!(
            "states the digest {} and its content's digest is {digest}",
            meta.hash
        ));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The door's error, kept as the KIND it was (so the HTTP status says the same thing), with
/// `prefix` before its sentence.
fn door_refusal(e: Error, prefix: &str) -> Error {
    match e {
        Error::Denied(m) => Error::Denied(format!("{prefix}{m}")),
        Error::NotFound(m) => Error::NotFound(format!("{prefix}{m}")),
        Error::Conflict(m) => Error::Conflict(format!("{prefix}{m}")),
        Error::Timeout(m) => Error::Timeout(format!("{prefix}{m}")),
        Error::Unavailable(m) => Error::Unavailable(format!("{prefix}{m}")),
        Error::InvalidArgument { name, detail } => Error::InvalidArgument {
            name,
            detail: format!("{prefix}{detail}"),
        },
        Error::MissingArgument(name) => Error::InvalidArgument {
            name: name.clone(),
            detail: format!("{prefix}the door says `{name}` is missing"),
        },
        other => Error::Endpoint(format!("{prefix}{other}")),
    }
}

// ------------------------------------------------------------------------------ parsing

/// A Turtle answer, by subject.
struct Graph {
    subjects: Vec<String>,
    by: BTreeMap<String, Vec<(String, Term)>>,
}

impl Graph {
    fn parse(bytes: &[u8]) -> std::result::Result<Graph, String> {
        let mut graph = Graph {
            subjects: Vec::new(),
            by: BTreeMap::new(),
        };
        for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(bytes) {
            let quad = quad.map_err(|e| format!("not Turtle: {e}"))?;
            let subject = match &quad.subject {
                oxigraph::model::NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
                oxigraph::model::NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
            };
            if !graph.by.contains_key(&subject) {
                graph.subjects.push(subject.clone());
            }
            graph
                .by
                .entry(subject)
                .or_default()
                .push((quad.predicate.as_str().to_string(), quad.object));
        }
        Ok(graph)
    }

    fn values(&self, subject: &str, predicate: &str) -> Vec<String> {
        self.by
            .get(subject)
            .into_iter()
            .flatten()
            .filter(|(p, _)| p == predicate)
            .map(|(_, o)| match o {
                Term::NamedNode(n) => n.as_str().to_string(),
                Term::BlankNode(b) => format!("_:{}", b.as_str()),
                Term::Literal(l) => l.value().to_string(),
                #[allow(unreachable_patterns)]
                _ => String::new(),
            })
            .collect()
    }

    fn value(&self, subject: &str, predicate: &str) -> Option<String> {
        self.values(subject, predicate).into_iter().next()
    }
}

fn local(iri: &str) -> String {
    iri.rsplit(['#', '/', ':'])
        .next()
        .unwrap_or(iri)
        .to_string()
}

/// Every subject under `prefix` that carries an `ik:seq`, as entries, ascending by `seq`.
fn entries_of(graph: &Graph, prefix: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = graph
        .subjects
        .iter()
        .filter(|s| s.starts_with(prefix))
        .filter_map(|s| {
            let seq = graph.value(s, &ik("seq"))?.parse().ok()?;
            let types = graph.values(s, RDF_TYPE);
            let kind = types
                .iter()
                .find(|t| t.starts_with(IK))
                .or(types.first())
                .map(|t| local(t))
                .unwrap_or_else(|| "Entry".to_string());
            let file = (kind == "File").then(|| FileMeta {
                name: graph.value(s, &ik("fileName")).unwrap_or_default(),
                media_type: graph.value(s, &ik("mediaType")).unwrap_or_default(),
                hash: graph.value(s, &ik("contentHash")).unwrap_or_default(),
                size: graph.value(s, &ik("byteSize")).and_then(|v| v.parse().ok()),
            });
            Some(Entry {
                iri: s.clone(),
                // An entry's id is its last segment; a tuple's, the rest of its subject under
                // the layer (`offer-01m…`), so an offer and a report never share an anchor.
                id: s[prefix.len()..].replace(':', "-"),
                seq,
                kind,
                who: graph.value(s, PROV_ATTRIBUTED).unwrap_or_default(),
                created: graph.value(s, DCT_CREATED).unwrap_or_default(),
                title: graph.value(s, DCT_TITLE),
                body: graph
                    .value(s, &ik("body"))
                    .or_else(|| graph.value(s, &ik("brief")))
                    .or_else(|| graph.value(s, &ik("note"))),
                repository: graph.value(s, &ik("repository")),
                severity: graph.value(s, &ik("severity")).map(|v| local(&v)),
                in_reply_to: graph.value(s, &ik("inReplyTo")),
                supersedes: graph.value(s, &ik("supersedes")),
                addressed_to: graph.value(s, &ik("addressedTo")),
                offer: graph.value(s, &ik("offer")),
                file,
            })
        })
        .collect();
    out.sort_by_key(|e| e.seq);
    out
}

/// Every `prov:Activity` in a receipts answer, ascending by `seq`.
fn receipts_of(graph: &Graph) -> Vec<Receipt> {
    let mut out: Vec<Receipt> = graph
        .subjects
        .iter()
        .filter(|s| graph.values(s, RDF_TYPE).iter().any(|t| t == PROV_ACTIVITY))
        .filter_map(|s| {
            let tuple = graph.value(s, PROV_USED).unwrap_or_default();
            Some(Receipt {
                iri: s.clone(),
                seq: graph.value(s, &ik("seq"))?.parse().ok()?,
                taker: graph.value(s, PROV_ASSOCIATED).unwrap_or_default(),
                ended: graph.value(s, PROV_ENDED).unwrap_or_default(),
                title: graph.value(&tuple, DCT_TITLE),
                tuple,
            })
        })
        .collect();
    out.sort_by_key(|r| r.seq);
    out
}

// ------------------------------------------------------------------------------ posting

/// The entry a say form posts: one `ik:Message` or `ik:Question` about `<urn:layer:new>`, with
/// its body and, when it answers one, the entry it replies to — serialized by oxigraph, so
/// the body cannot reshape the Turtle.
///
/// ```
/// use ikigai_gonk::layers_view::say_turtle;
/// let t = say_turtle("question", "Is \"this\" > that?\n", Some("urn:layer:talk:entry:01a")).unwrap();
/// assert!(t.contains("Question"));
/// assert!(t.contains("urn:layer:talk:entry:01a"));
/// assert!(say_turtle("brief", "x", None).is_err());
/// assert!(say_turtle("message", "  ", None).is_err());
/// ```
///
/// # Errors
///
/// A kind other than `message` or `question`, an empty body, a reply target that is not an
/// IRI.
pub fn say_turtle(
    kind: &str,
    body: &str,
    reply_to: Option<&str>,
) -> std::result::Result<String, Error> {
    let class = match kind {
        "message" => "Message",
        "question" => "Question",
        other => {
            return Err(Error::InvalidArgument {
                name: "kind".to_string(),
                detail: format!(
                    "`{other}` is not a kind this page posts: a message or a question (briefs, \
                     issues and files are posted by a client, not this page)"
                ),
            })
        }
    };
    if body.trim().is_empty() {
        return Err(Error::InvalidArgument {
            name: "body".to_string(),
            detail: "the post is empty: write something first".to_string(),
        });
    }
    let named = |iri: &str| {
        NamedNode::new(iri).map_err(|e| Error::InvalidArgument {
            name: "reply_to".to_string(),
            detail: format!("`{iri}` is not an IRI: {e}"),
        })
    };
    let subject = named("urn:layer:new")?;
    let mut triples = vec![
        Triple::new(subject.clone(), named(RDF_TYPE)?, named(&ik(class))?),
        Triple::new(
            subject.clone(),
            named(&ik("body"))?,
            Literal::new_simple_literal(body),
        ),
    ];
    if let Some(reply) = reply_to {
        triples.push(Triple::new(
            subject,
            named(&ik("inReplyTo"))?,
            named(reply)?,
        ));
    }
    let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle)
        .with_prefix("ik", IK)
        .map_err(|e| Error::Endpoint(e.to_string()))?
        .for_writer(Vec::new());
    for triple in &triples {
        serializer
            .serialize_triple(triple)
            .map_err(|e| Error::Endpoint(e.to_string()))?;
    }
    let bytes = serializer
        .finish()
        .map_err(|e| Error::Endpoint(e.to_string()))?;
    String::from_utf8(bytes).map_err(|e| Error::Endpoint(e.to_string()))
}

/// Whether `id` is shaped like an entry id: the last segment of an entry IRI.
fn is_entry_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// ------------------------------------------------------------------------------ rendering

/// Who a principal is, in a few words: a participant's short name, `(you)` for the identity.
fn who(principal: &str, me: Option<&Me>) -> String {
    let short = principal
        .strip_prefix(PARTICIPANT_PREFIX)
        .map(str::to_string)
        .or_else(|| {
            principal
                .strip_prefix("urn:iki:host:")
                .and_then(|rest| rest.split(':').next())
                .map(|host| format!("{host} (a machine)"))
        })
        .unwrap_or_else(|| principal.to_string());
    if me.is_some_and(|m| m.principal == principal) {
        format!("{short} (you)")
    } else {
        short
    }
}

/// A file's size in words.
fn size_words(size: Option<u64>) -> String {
    match size {
        None => "size not stated".to_string(),
        Some(n) if n < 1024 => format!("{n} bytes"),
        Some(n) if n < 1024 * 1024 => format!("{:.1} KB ({n} bytes)", n as f64 / 1024.0),
        Some(n) => format!("{:.1} MB ({n} bytes)", n as f64 / (1024.0 * 1024.0)),
    }
}

/// The name a downloaded file is saved under: the bare name, never a path.
fn bare_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    match clean.trim() {
        "" | "." | ".." => "download".to_string(),
        kept => kept.to_string(),
    }
}

fn kind_word(kind: &str) -> String {
    match kind {
        "Message" => "Message",
        "Question" => "Question",
        "Brief" => "Brief",
        "Issue" => "Issue",
        "File" => "File",
        "Offer" => "Offer",
        "Progress" => "Progress report",
        "Release" => "Release",
        "Handback" => "Hand-back",
        other => return other.to_string(),
    }
    .to_string()
}

fn verbs_sentence(info: &LayerInfo) -> String {
    let mut can = Vec::new();
    if info.holds("read") {
        can.push("read");
    }
    if info.holds("feed") {
        can.push("follow");
    }
    let posts = info.holds("say") && info.model == Model::Channel;
    if posts {
        can.push("post");
    }
    let mut sentence = format!("You may {}", list_words(&can));
    if !posts {
        sentence.push_str("; this page offers no way to write here");
    }
    sentence.push('.');
    sentence
}

fn list_words(words: &[&str]) -> String {
    match words {
        [] => "nothing".to_string(),
        [one] => (*one).to_string(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn model_word(model: &Model) -> String {
    match model {
        Model::Channel => "a channel: messages, questions, briefs, issues and files".to_string(),
        Model::Tuplespace => "a task layer: offers taken atomically, with receipts".to_string(),
        Model::Other(m) => format!("a `{m}` layer, which this page cannot show"),
    }
}

fn entry_row(
    name: &str,
    entry: &Entry,
    layer: &str,
    me: Option<&Me>,
    new: bool,
    replies: bool,
) -> String {
    let anchor = format!("entry-{}", entry.id);
    let mut attributes: Vec<(&str, String)> = vec![
        ("id", entry.id.clone()),
        ("anchor", anchor.clone()),
        ("href", format!("#{anchor}")),
        ("seq", entry.seq.to_string()),
        ("kind", entry.kind.to_lowercase()),
        ("kind-label", kind_word(&entry.kind)),
        ("who", who(&entry.who, me)),
        ("datetime", entry.created.clone()),
        ("when", web::when(&entry.created)),
        ("new", if new { "true" } else { "false" }.to_string()),
    ];
    if let Some(title) = &entry.title {
        attributes.push(("title", title.clone()));
    }
    if let Some(repo) = &entry.repository {
        attributes.push(("repository", repo.clone()));
        if repo.starts_with("https://") {
            attributes.push(("repository-href", repo.clone()));
        }
    }
    if let Some(severity) = &entry.severity {
        attributes.push(("severity", severity.clone()));
    }
    for (name, target) in [
        ("reply", &entry.in_reply_to),
        ("supersedes", &entry.supersedes),
    ] {
        if let Some(target) = target {
            let id = local(target);
            attributes.push((
                if name == "reply" {
                    "reply-label"
                } else {
                    "supersedes-label"
                },
                id.clone(),
            ));
            attributes.push((
                if name == "reply" {
                    "reply-href"
                } else {
                    "supersedes-href"
                },
                format!("#entry-{id}"),
            ));
        }
    }
    if let Some(to) = &entry.addressed_to {
        attributes.push(("addressed-to", who(to, me)));
    }
    if let Some(offer) = &entry.offer {
        attributes.push(("offer", local(offer)));
    }
    if let Some(file) = &entry.file {
        let name = bare_name(&file.name);
        attributes.push(("file-name", name));
        attributes.push(("media-type", file.media_type.clone()));
        attributes.push(("size", size_words(file.size)));
        attributes.push(("digest", file.hash.clone()));
        attributes.push((
            "download-href",
            format!("{PAGE_PATH}/{layer}/file/{}", entry.id),
        ));
    }
    if replies {
        attributes.push((
            "answer-href",
            format!("{PAGE_PATH}/{layer}?reply={}#say", entry.id),
        ));
    }
    let attributes: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = entry
        .body
        .as_deref()
        .map(|b| element("body", &[], b))
        .unwrap_or_default();
    wrap(name, &attributes, &body)
}

fn receipt_row(receipt: &Receipt, me: Option<&Me>, new: bool) -> String {
    element(
        "receipt",
        &[
            ("taker", &who(&receipt.taker, me)),
            ("datetime", &receipt.ended),
            ("when", &web::when(&receipt.ended)),
            ("seq", &receipt.seq.to_string()),
            (
                "what",
                receipt.title.as_deref().unwrap_or(&local(&receipt.tuple)),
            ),
            ("new", if new { "true" } else { "false" }),
        ],
        "",
    )
}

/// Rows as chunk documents of at most [`render::CHUNK_ROWS`] rows and about half of
/// [`render::MAX_CHUNK_BYTES`]: xrust's cost is superlinear in one document (ledger #519).
fn chunks(rows: &[String]) -> Vec<String> {
    let budget = render::MAX_CHUNK_BYTES / 2;
    let mut out = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for row in rows {
        if count > 0 && (count >= render::CHUNK_ROWS || current.len() + row.len() > budget) {
            out.push(render::chunk(&std::mem::take(&mut current)));
            count = 0;
        }
        current.push_str(row);
        count += 1;
    }
    if count > 0 {
        out.push(render::chunk(&current));
    }
    out
}

async fn rendered(inv: &Invocation<'_>, rows: &[String]) -> Result<String> {
    render::rendered_chunks(inv, &chunks(rows)).await
}

/// The notices a page shows above its content: not configured, or the door down.
fn notices(view: Option<&LayersView>, snapshot: &Snapshot) -> String {
    let mut out = String::new();
    match view {
        None => out.push_str(&element(
            "notice",
            &[
                ("kind", "info"),
                (
                    "config",
                    &format!("{KEY} = \"quic://<host>:<port> <certificate directory>\""),
                ),
            ],
            "The Layers page is not configured on this server. Add one line to config.toml and \
             restart gonk: the layer host's door, and the certificate directory of the identity \
             to read and post as (its client.crt, client.key and the server.crt it pins). It \
             never uses this server's own certificates.",
        )),
        Some(view) => {
            if let Some(down) = &snapshot.down {
                out.push_str(&element(
                    "notice",
                    &[("kind", "error")],
                    &format!(
                        "The layer door at {} cannot be reached: {down}. This page shows what \
                         was read before; it updates when the door answers again.",
                        view.origin()
                    ),
                ));
            } else if !snapshot.discovered {
                out.push_str(&element(
                    "notice",
                    &[("kind", "info")],
                    &format!("Reading the layers from {}…", view.origin()),
                ));
            }
        }
    }
    out
}

fn viewer(inv: &Invocation<'_>) -> String {
    inv.capability
        .principal()
        .map(str::to_string)
        .unwrap_or_else(|| "anonymous".to_string())
}

/// What a page says to a caller without [`CAP_READ`].
fn lacking() -> web::Lacking {
    web::Lacking {
        signed_out: "Sign in to read the layers.".to_string(),
        signed_in: "Your passkey's grant doesn't include the Layers page.".to_string(),
        needs: vec![
            (
                CAP_READ.to_string(),
                "to read the layers and follow them".to_string(),
            ),
            (
                CAP_SAY.to_string(),
                "to post to them, as the identity the page is configured with".to_string(),
            ),
        ],
    }
}

/// Whether the identity may post on `layer` through this page: the `say` verb, a channel,
/// and a PERSON, so a post is never stamped as a machine.
fn may_post(snapshot: &Snapshot, info: &LayerInfo) -> bool {
    info.holds("say")
        && info.model == Model::Channel
        && snapshot
            .me
            .as_ref()
            .is_some_and(|m| m.kind.as_deref() == Some("person"))
}

/// What the page's banner says about the door, as its watcher carries it (`door=`): an answer
/// sends the banner only when this differs, so a poll that changes nothing replaces nothing a
/// person may be reading.
fn door_state(snapshot: &Snapshot) -> &'static str {
    if snapshot.down.is_some() {
        "down"
    } else if !snapshot.discovered {
        "reading"
    } else {
        "ok"
    }
}

/// The page's watcher: asks `url` once, [`LIVE_EVERY_SECS`] after it is put on the page
/// ([`DOWN_EVERY_SECS`] while the door is down), and is replaced by the answer, which carries
/// the next one. A failed ask is re-armed by `web/gonk.js` with a backoff (ledger #1705).
fn watch(url: String, down: bool) -> String {
    let trigger = format!(
        "load delay:{}s",
        if down {
            DOWN_EVERY_SECS
        } else {
            LIVE_EVERY_SECS
        }
    );
    element("watch", &[("url", &url), ("trigger", &trigger)], "")
}

async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("gonk-layers-call".to_string())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .map_err(|e| Error::Endpoint(format!("no thread for the layer door: {e}")))?;
    rx.await
        .map_err(|_| Error::Endpoint("the layer door's call panicked".to_string()))
}

/// Start the follower and wait (off the async thread) for the first read.
async fn ready(view: &Arc<LayersView>) -> Result<Snapshot> {
    view.start();
    let waiting = Arc::clone(view);
    off_thread(move || {
        waiting.wait_first_read(FIRST_READ);
        waiting.snapshot()
    })
    .await
}

fn layer_binding(inv: &Invocation<'_>) -> Result<String> {
    let layer = web::binding(inv, "layer")?;
    if !crate::layer::is_layer_name(&layer) {
        return Err(Error::NotFound(format!("`{layer}` is not a layer name")));
    }
    Ok(layer)
}

fn optional_arg(inv: &Invocation<'_>, name: &str) -> Option<String> {
    inv.inline_str(name)
        .ok()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn arg(name: &str, summary: &str) -> ArgSpec {
    ArgSpec::new(name).summary(summary).class(XSD_STRING)
}

fn layer_arg() -> ArgSpec {
    arg("layer", "the layer's name: `urn:layer:{layer}`").binding()
}

// ------------------------------------------------------------------------------ the pages

/// Bind the page's resources into the HTTP face's page space.
pub fn bind(space: EndpointSpace, web: &Arc<Web>, view: Option<Arc<LayersView>>) -> EndpointSpace {
    let template = |t: &str| UriTemplate::parse(t).expect("a constant template");
    space
        .bind(
            Exact::new(PAGE_IRI),
            Overview {
                web: Arc::clone(web),
                view: view.clone(),
            },
        )
        .bind(Exact::new(WAIT_IRI), Wait { view: view.clone() })
        .bind(Exact::new(COUNT_IRI), Count { view: view.clone() })
        .bind(template(SAY_TEMPLATE), Say { view: view.clone() })
        .bind(template(FILE_TEMPLATE), File { view: view.clone() })
        .bind(
            template(LAYER_PAGE_TEMPLATE),
            LayerPage {
                web: Arc::clone(web),
                view,
            },
        )
}

/// The rows of the overview, and the total unread across them.
fn overview_rows(snapshot: &Snapshot, view: &LayersView, viewer: &str) -> (String, u64) {
    let mut total = 0;
    let rows = snapshot
        .layers
        .values()
        .map(|layer| {
            let seen = view.cursors.get(viewer, &layer.info.name).min(layer.last);
            let unread = layer.last - seen;
            total += unread;
            let newest = layer
                .entries
                .last()
                .map(|e| e.created.clone())
                .or_else(|| layer.tuples.last().map(|t| t.created.clone()))
                .unwrap_or_default();
            element(
                "layer-row",
                &[
                    ("name", &layer.info.name),
                    ("label", &layer.info.label),
                    ("href", &format!("{PAGE_PATH}/{}", layer.info.name)),
                    ("model", &model_word(&layer.info.model)),
                    ("verbs", &verbs_sentence(&layer.info)),
                    (
                        "unread",
                        &match (layer.synced, unread) {
                            (false, _) => "not read yet".to_string(),
                            (true, 0) => "nothing new".to_string(),
                            (true, 1) => "1 new".to_string(),
                            (true, n) => format!("{n} new"),
                        },
                    ),
                    ("has-unread", if unread > 0 { "true" } else { "false" }),
                    ("datetime", &newest),
                    ("when", &web::when(&newest)),
                ],
                "",
            )
        })
        .collect();
    (rows, total)
}

fn me_attributes(snapshot: &Snapshot) -> Vec<(&'static str, String)> {
    match &snapshot.me {
        None => Vec::new(),
        Some(me) => vec![
            ("me", me.principal.clone()),
            (
                "me-kind",
                match me.kind.as_deref() {
                    Some("person") => "a person".to_string(),
                    Some("agent") => "an agent".to_string(),
                    Some("machine") => "a machine".to_string(),
                    Some(other) => other.to_string(),
                    None => "not a participant".to_string(),
                },
            ),
        ],
    }
}

/// `urn:iki:gonk:page:layers`: every layer the identity reads.
struct Overview {
    web: Arc<Web>,
    view: Option<Arc<LayersView>>,
}

#[async_trait]
impl Endpoint for Overview {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(
                "the Layers page answers Source only".to_string(),
            ));
        }
        web::html_only(inv)?;
        let ledgers = web::readable_ledgers(&self.web, inv);
        // The overview announces what is new itself, so the header's count stays quiet here.
        let mut children = web::nav_on_layers(&self.web, inv, &ledgers, "quiet=1");
        let mut attributes = vec![
            ("view", "layers".to_string()),
            ("full", "true".to_string()),
            ("title", "Layers".to_string()),
        ];
        if !inv.capability.allows(CAP_READ) {
            children.push_str(&lacking().element());
        } else {
            let snapshot = match &self.view {
                Some(view) => ready(view).await?,
                None => Snapshot::default(),
            };
            children.push_str(&notices(self.view.as_deref(), &snapshot));
            if let Some(view) = &self.view {
                let (rows, total) = overview_rows(&snapshot, view, &viewer(inv));
                children.push_str(&rows);
                attributes.extend(me_attributes(&snapshot));
                if snapshot.discovered && snapshot.layers.is_empty() {
                    attributes.push((
                        "empty",
                        "The door names no layer this identity may read.".to_string(),
                    ));
                }
                children.push_str(&watch(
                    format!(
                        "{WAIT_PATH}?after={}&seen={total}&door={}",
                        snapshot.epoch,
                        door_state(&snapshot)
                    ),
                    snapshot.down.is_some(),
                ));
            }
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let doc = envelope("page", &attributes, &children);
        Ok(web::html(
            render::render(&doc, true).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-layers"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-layers")
            .title("Layers")
            .summary(
                "Every layer the configured identity may read at its layer door, with what is \
                 new since this viewer last looked. Shows the grant it needs to a caller \
                 without urn:cap:gonk:layers:read.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(web::as_html_arg())
            .output("text/html")
    }
}

/// `urn:iki:gonk:page:layer:{layer}`: one layer's entries, or its tuples and takes.
struct LayerPage {
    web: Arc<Web>,
    view: Option<Arc<LayersView>>,
}

#[async_trait]
impl Endpoint for LayerPage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(
                "a layer page answers Source only".to_string(),
            ));
        }
        web::html_only(inv)?;
        let layer = layer_binding(inv)?;
        let ledgers = web::readable_ledgers(&self.web, inv);
        // The open layer reads its entries as they arrive: the header counts the others.
        let mut children = web::nav_on_layers(&self.web, inv, &ledgers, &format!("open={layer}"));
        let mut attributes = vec![
            ("view", "layer".to_string()),
            ("full", "true".to_string()),
            ("title", layer.clone()),
            ("name", layer.clone()),
        ];
        let mut slots: Vec<(String, String)> = Vec::new();
        if !inv.capability.allows(CAP_READ) {
            children.push_str(&lacking().element());
        } else if let Some(view) = &self.view {
            let snapshot = ready(view).await?;
            children.push_str(&notices(Some(view), &snapshot));
            let Some(found) = snapshot.layers.get(&layer) else {
                if snapshot.discovered {
                    return Err(Error::NotFound(format!(
                        "the identity this page reads as holds no read grant on a layer \
                         `{layer}` at {}",
                        view.origin()
                    )));
                }
                let attributes: Vec<(&str, &str)> =
                    attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
                let doc = envelope("page", &attributes, &children);
                return Ok(web::html(
                    render::render(&doc, true).map_err(web::render_err)?,
                ));
            };
            let who_views = viewer(inv);
            let seen = view.cursors.get(&who_views, &layer).min(found.last);
            let me = snapshot.me.as_ref();
            attributes[2].1 = found.info.label.clone();
            attributes.push(("model", model_word(&found.info.model)));
            attributes.push(("verbs", verbs_sentence(&found.info)));
            attributes.extend(me_attributes(&snapshot));
            let posts = may_post(&snapshot, &found.info) && inv.capability.allows(CAP_SAY);
            if posts {
                let reply = optional_arg(inv, "reply")
                    .filter(|id| is_entry_id(id))
                    .filter(|id| found.entries.iter().any(|e| &e.id == id));
                children.push_str(&say_form(&layer, &found.info.label, reply.as_deref()));
            } else if found.info.holds("say") && found.info.model == Model::Channel {
                attributes.push((
                    "no-post",
                    if !inv.capability.allows(CAP_SAY) {
                        format!("Your passkey's grant doesn't include posting here ({CAP_SAY}).")
                    } else {
                        "The identity this page is configured with is not a person, so the page \
                         does not post as it."
                            .to_string()
                    },
                ));
            }
            if !found.synced {
                attributes.push((
                    "reading",
                    format!(
                        "Reading {} from the door; it appears here when it is read.",
                        found.info.label
                    ),
                ));
            }
            match found.info.model {
                Model::Channel if found.synced => {
                    let rows: Vec<String> = found
                        .entries
                        .iter()
                        .rev()
                        .map(|e| entry_row("entry", e, &layer, me, e.seq > seen, posts))
                        .collect();
                    let unread_shown = found.entries.iter().filter(|e| e.seq > seen).count() as u64;
                    let unread = found.last - seen;
                    if unread > unread_shown {
                        attributes.push((
                            "older-unread",
                            format!(
                                "{} more new entries are older than the newest {WINDOW} shown \
                                 here.",
                                unread - unread_shown
                            ),
                        ));
                    }
                    attributes.push(("entries", (!rows.is_empty()).to_string()));
                    children.push_str(&render::slot("entries"));
                    slots.push(("entries".to_string(), rendered(inv, &rows).await?));
                }
                Model::Tuplespace if found.synced => {
                    attributes.push(("tuplespace", "true".to_string()));
                    let (tuples, receipts) = tuple_rows(found, me, seen);
                    children.push_str(&render::slot("tuples"));
                    children.push_str(&render::slot("receipts"));
                    slots.push(("tuples".to_string(), rendered(inv, &tuples).await?));
                    slots.push(("receipts".to_string(), rendered(inv, &receipts).await?));
                }
                _ => {}
            }
            // Opening the layer is reading it.
            if found.synced {
                view.cursors.set(&who_views, &layer, found.last);
            }
            children.push_str(&watch(
                format!(
                    "{WAIT_PATH}?layer={layer}&after={}&door={}",
                    found.last,
                    door_state(&snapshot)
                ),
                snapshot.down.is_some(),
            ));
        } else {
            children.push_str(&notices(None, &Snapshot::default()));
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let doc = envelope("page", &attributes, &children);
        let shell = render::render(&doc, true).map_err(web::render_err)?;
        Ok(web::html(
            render::splice(shell, &slots).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-layer"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-layer")
            .title("A layer")
            .summary(
                "One layer as the configured identity reads it: a channel's newest entries, \
                 newest first, with a form to post a message or a question where the identity \
                 holds `say`; a task layer's pending tuples and recent takes, read-only. \
                 Opening it marks it read for this viewer.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(layer_arg())
            .input(arg("reply", "the id of the entry the form answers").optional())
            .input(web::as_html_arg())
            .output("text/html")
    }
}

fn tuple_rows(layer: &LayerView, me: Option<&Me>, seen: u64) -> (Vec<String>, Vec<String>) {
    let tuples = layer
        .tuples
        .iter()
        .rev()
        .map(|t| entry_row("tuple", t, &layer.info.name, me, t.seq > seen, false))
        .collect();
    let receipts = layer
        .receipts
        .iter()
        .rev()
        .map(|r| receipt_row(r, me, r.seq > seen))
        .collect();
    (tuples, receipts)
}

fn say_form(layer: &str, label: &str, reply: Option<&str>) -> String {
    let mut attributes = vec![
        ("layer", layer.to_string()),
        ("label", label.to_string()),
        ("action", format!("{PAGE_PATH}/{layer}/say")),
        ("cancel-href", format!("{PAGE_PATH}/{layer}#say")),
    ];
    if let Some(id) = reply {
        attributes.push(("reply-to", format!("urn:layer:{layer}:entry:{id}")));
        attributes.push(("reply-label", id.to_string()));
        attributes.push(("reply-href", format!("#entry-{id}")));
    }
    let attributes: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    element("say", &attributes, "")
}

/// `urn:iki:gonk:fragment:layers-wait`: what an open Layers page asks every few seconds. It
/// answers AT ONCE, from this process's copy and the cursors (never the door), with a new watcher
/// and whatever changed since the page's: the new entries, a task layer's lists, the overview's
/// rows, the banner, a sentence for the live region. A poll that brings nothing new sends only
/// the next watcher. The name is from when it was a long poll (ledger #1705 says why it is not).
struct Wait {
    view: Option<Arc<LayersView>>,
}

#[async_trait]
impl Endpoint for Wait {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let view = self
            .view
            .as_ref()
            .ok_or_else(|| Error::NotFound("the Layers page is not configured".to_string()))?;
        let after: u64 =
            inv.inline_str("after")?
                .trim()
                .parse()
                .map_err(|_| Error::InvalidArgument {
                    name: "after".to_string(),
                    detail: "not a non-negative integer".to_string(),
                })?;
        let layer = optional_arg(inv, "layer");
        if let Some(name) = &layer {
            if !crate::layer::is_layer_name(name) {
                return Err(Error::InvalidArgument {
                    name: "layer".to_string(),
                    detail: format!("`{name}` is not a layer name"),
                });
            }
        }
        // What the page's banner shows; absent (a page from before ledger #1705), it differs.
        let door_was = optional_arg(inv, "door");
        // The follower is started by the page; after a restart of gonk it is this ask that
        // starts it again, and until it has read the door the copy says "reading".
        view.start();
        let snapshot = view.snapshot();
        let down = snapshot.down.is_some();
        let door_now = door_state(&snapshot);
        let mut attributes: Vec<(&str, String)> = vec![("view", "layers-wait".to_string())];
        let mut children = String::new();
        let mut slots = Vec::new();
        if door_was.as_deref() != Some(door_now) {
            attributes.push(("banner", "true".to_string()));
            children.push_str(&notices(Some(view), &snapshot));
        }
        let who_views = viewer(inv);
        match &layer {
            None => {
                let seen_before: u64 = optional_arg(inv, "seen")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let (rows, total) = overview_rows(&snapshot, view, &who_views);
                // ★ The rows are replaced only when they changed: the copy moved (`after` is
                // the epoch the page shows; a restarted gonk counts from 0 again, so any
                // difference counts), or the viewer's unread total did, which is how a layer
                // opened in ANOTHER tab reaches this one. Replacing them on every poll would
                // take the keyboard focus out of the list every few seconds.
                if snapshot.epoch != after || total != seen_before {
                    attributes.push(("rows", "true".to_string()));
                    children.push_str(&rows);
                }
                if total > seen_before {
                    attributes.push(("announce", format!("{total} new in the layers.")));
                }
                children.push_str(&watch(
                    format!(
                        "{WAIT_PATH}?after={}&seen={total}&door={door_now}",
                        snapshot.epoch
                    ),
                    down,
                ));
            }
            Some(name) => {
                let found = snapshot.layers.get(name);
                let last = found.map_or(after, |l| l.last);
                if let Some(found) = found.filter(|l| l.last > after) {
                    let me = snapshot.me.as_ref();
                    match found.info.model {
                        Model::Channel => {
                            let posts =
                                may_post(&snapshot, &found.info) && inv.capability.allows(CAP_SAY);
                            let rows: Vec<String> = found
                                .entries
                                .iter()
                                .rev()
                                .filter(|e| e.seq > after)
                                .map(|e| entry_row("entry", e, name, me, true, posts))
                                .collect();
                            if !rows.is_empty() {
                                attributes.push((
                                    "announce",
                                    match rows.len() {
                                        1 => format!("1 new entry in {}.", found.info.label),
                                        n => format!("{n} new entries in {}.", found.info.label),
                                    },
                                ));
                                attributes.push(("entries", "true".to_string()));
                                children.push_str(&render::slot("entries"));
                                slots.push(("entries".to_string(), rendered(inv, &rows).await?));
                            }
                        }
                        Model::Tuplespace => {
                            let seen = view.cursors.get(&who_views, name).min(found.last);
                            let (tuples, receipts) = tuple_rows(found, me, seen.min(after));
                            attributes.push(("tuplespace", "true".to_string()));
                            attributes.push(("announce", format!("{} changed.", found.info.label)));
                            children.push_str(&render::slot("tuples"));
                            children.push_str(&render::slot("receipts"));
                            slots.push(("tuples".to_string(), rendered(inv, &tuples).await?));
                            slots.push(("receipts".to_string(), rendered(inv, &receipts).await?));
                        }
                        Model::Other(_) => {}
                    }
                    // Delivered to an open page: the viewer has it.
                    view.cursors.set(&who_views, name, found.last);
                }
                children.push_str(&watch(
                    format!("{WAIT_PATH}?layer={name}&after={last}&door={door_now}"),
                    down,
                ));
            }
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let doc = envelope("page", &attributes, &children);
        let shell = render::render(&doc, false).map_err(web::render_err)?;
        Ok(web::html(
            render::splice(shell, &slots).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-layers-wait"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-layers-wait")
            .title("What changed on an open Layers page")
            .summary(
                "Answers at once, from gonk's copy, the fragment that brings an open Layers \
                 page up to date: what a layer holds past `after` (or, without `layer`, the \
                 overview's rows when they changed), the banner when the door's state differs \
                 from `door`, and the next watcher, which asks again in 3 seconds (15 while the \
                 door is down). A short poll: it never holds a connection. gonk holds the one \
                 long poll at the door.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_READ)
            .input(
                ArgSpec::new("after")
                    .summary(
                        "the feed position (with `layer`) or the overview's epoch the page has",
                    )
                    .class(XSD_NON_NEGATIVE),
            )
            .input(arg("layer", "the layer the page shows; absent for the overview").optional())
            .input(
                arg("door", "what the page's banner shows about the door")
                    .one_of(["ok", "down", "reading"])
                    .optional(),
            )
            .input(arg("seen", "the overview's unread total as the page shows it").optional())
            .output("text/html")
    }
}

/// `urn:iki:gonk:fragment:layers-count`: the header's unread count for the viewer, from this
/// process's copy (see the module docs for why it is a short poll). It answers at once, the
/// badge and the URL of the next poll; a sentence for the header's live region only when the
/// count ROSE past `seen`, so a poll that brings nothing new says nothing.
struct Count {
    view: Option<Arc<LayersView>>,
}

/// The URL the header's count polls: `seen` is the count the page shows (`None`: none known).
fn count_url(seen: Option<u64>, open: Option<&str>, quiet: bool) -> String {
    let mut url = format!(
        "{COUNT_PATH}?seen={}",
        seen.map_or_else(|| "-".to_string(), |n| n.to_string())
    );
    if let Some(layer) = open {
        url.push_str(&format!("&open={layer}"));
    }
    if quiet {
        url.push_str("&quiet=1");
    }
    url
}

#[async_trait]
impl Endpoint for Count {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(
                "the header's layers count answers Source only".to_string(),
            ));
        }
        // The floor refuses first (`describe` requires it); said again here so the count can
        // never answer a caller the Layers page would not.
        if !inv.capability.allows(CAP_READ) {
            return Err(Error::Denied(format!(
                "the layers' unread count needs {CAP_READ}"
            )));
        }
        let seen: Option<u64> = optional_arg(inv, "seen").and_then(|v| v.parse().ok());
        let open = optional_arg(inv, "open");
        if let Some(name) = &open {
            if !crate::layer::is_layer_name(name) {
                return Err(Error::InvalidArgument {
                    name: "open".to_string(),
                    detail: format!("`{name}` is not a layer name"),
                });
            }
        }
        let quiet = optional_arg(inv, "quiet").as_deref() == Some("1");
        let mut attributes: Vec<(&str, String)> = vec![("view", "layers-count".to_string())];
        // Not configured: an inert badge, and no next poll.
        if let Some(view) = &self.view {
            // The first count a reader of the layers asks for starts the follower; until it
            // has read the door once there is no count to show.
            view.start();
            let count = view.unread(&viewer(inv), open.as_deref());
            if let Some(n) = count.filter(|n| *n > 0) {
                attributes.push(("count", n.to_string()));
            }
            if let (Some(before), Some(now)) = (seen, count) {
                if now > before && !quiet {
                    attributes.push((
                        "announce",
                        match now {
                            1 => "1 new entry in the layers.".to_string(),
                            n => format!("{n} new entries in the layers."),
                        },
                    ));
                }
            }
            attributes.push(("url", count_url(count, open.as_deref(), quiet)));
            attributes.push(("every", crate::queue::BADGE_EVERY.to_string()));
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let doc = envelope("page", &attributes, "");
        Ok(web::html(
            render::render(&doc, false).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-layers-count"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-layers-count")
            .title("The layers' unread count, for the header")
            .summary(
                "How many layer entries this viewer has not been shown, across every layer the \
                 configured identity reads (but `open`, the layer whose page is open), polled by \
                 the header's Layers link at the Queue badge's cadence. Read from gonk's copy, \
                 never the door. Answers the badge, the next poll, and a sentence for a polite \
                 live region only when the count rose past `seen`.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_READ)
            .input(
                arg(
                    "seen",
                    "the count the page shows; `-` or absent when it shows none",
                )
                .optional(),
            )
            .input(
                arg(
                    "open",
                    "the layer whose page is open, which is not counted; absent elsewhere",
                )
                .optional(),
            )
            .input(
                arg(
                    "quiet",
                    "`1` on the overview, which announces what is new itself",
                )
                .optional(),
            )
            .output("text/html")
    }
}

/// `urn:iki:gonk:layers:say:{layer}`: a say form → one entry at the door, as the configured
/// identity.
struct Say {
    view: Option<Arc<LayersView>>,
}

#[async_trait]
impl Endpoint for Say {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Endpoint("a post answers Sink only".to_string()));
        }
        let view = self
            .view
            .as_ref()
            .ok_or_else(|| Error::NotFound("the Layers page is not configured".to_string()))?;
        let layer = layer_binding(inv)?;
        let fields = web::form(inv.inline_str("content")?);
        let snapshot = ready(view).await?;
        let info = snapshot
            .layers
            .get(&layer)
            .map(|l| l.info.clone())
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "the identity this page posts as reads no layer `{layer}` at {}",
                    view.origin()
                ))
            })?;
        if !info.holds("say") || info.model != Model::Channel {
            return Err(Error::Denied(format!(
                "the identity this page posts as holds no `say` on the channel `{layer}`, so the \
                 page does not offer it"
            )));
        }
        if !may_post(&snapshot, &info) {
            return Err(Error::Denied(format!(
                "the door says the identity this page is configured with is {}, not a person; \
                 the page posts only as a person",
                snapshot
                    .me
                    .as_ref()
                    .and_then(|m| m.kind.clone())
                    .unwrap_or_else(|| "unknown".to_string())
            )));
        }
        let kind = fields
            .get("kind")
            .map(|k| k.trim().to_string())
            .unwrap_or_else(|| "message".to_string());
        let body = fields.get("body").cloned().unwrap_or_default();
        let reply = match fields
            .get("reply_to")
            .map(|r| r.trim())
            .filter(|r| !r.is_empty())
        {
            None => None,
            Some(target) => {
                let id = target
                    .strip_prefix(&format!("urn:layer:{layer}:entry:"))
                    .unwrap_or(target);
                if !is_entry_id(id) {
                    return Err(Error::InvalidArgument {
                        name: "reply_to".to_string(),
                        detail: format!("`{target}` is not an entry of `{layer}`"),
                    });
                }
                Some(format!("urn:layer:{layer}:entry:{id}"))
            }
        };
        let turtle = say_turtle(&kind, &body, reply.as_deref())?;
        let posting = Arc::clone(view);
        let on = layer.clone();
        let said = off_thread(move || posting.say(&on, turtle)).await??;
        let flash = format!(
            "Posted to {} as a {kind}: {said}. It appears in the list when the door's feed \
             carries it.",
            info.label
        );
        let doc = envelope(
            "page",
            &[("view", "layers-say"), ("flash", &flash)],
            &say_form(&layer, &info.label, None),
        );
        Ok(web::html(
            render::render(&doc, false).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-layers-say"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-layers-say")
            .title("Post to a layer")
            .summary(
                "A form (`kind` message or question, `body`, optional `reply_to`) → one entry \
                 said at the layer door as the identity this page is configured with, which \
                 must hold `say` on the channel and be a person. The door's refusal is the \
                 answer when it refuses.",
            )
            .verb(Verb::Sink)
            .verb(Verb::Meta)
            .requires(CAP_SAY)
            .input(layer_arg())
            .input(arg(
                "content",
                "the form, url-encoded: kind, body, reply_to",
            ))
            .output("text/html")
    }
}

/// `urn:iki:gonk:layers:file:{layer}:{id}`: a file entry's bytes, after the check.
struct File {
    view: Option<Arc<LayersView>>,
}

#[async_trait]
impl Endpoint for File {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let view = self
            .view
            .as_ref()
            .ok_or_else(|| Error::NotFound("the Layers page is not configured".to_string()))?;
        let layer = layer_binding(inv)?;
        let id = web::binding(inv, "id")?;
        if !is_entry_id(&id) {
            return Err(Error::NotFound(format!("`{id}` is not an entry id")));
        }
        let snapshot = ready(view).await?;
        if !snapshot.layers.contains_key(&layer) {
            return Err(Error::NotFound(format!(
                "the identity this page reads as reads no layer `{layer}`"
            )));
        }
        let reading = Arc::clone(view);
        let (_meta, bytes) = off_thread(move || reading.file(&layer, &id)).await??;
        // ★ Never the stated media type: served as bytes, so a browser saves it and never
        // renders it (an HTML "file" would otherwise run as this origin). The route's CSP
        // sandboxes it besides (crate::doors).
        Ok(Representation::new(
            ReprType::new("application/octet-stream"),
            bytes,
        ))
    }

    fn name(&self) -> &str {
        "gonk-layers-file"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-layers-file")
            .title("A layer file")
            .summary(
                "A file entry's bytes, read from the layer door and answered only when they \
                 match the entry's stated size and sha256 digest; always \
                 application/octet-stream, never rendered.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_READ)
            .input(layer_arg())
            .input(arg("id", "the file entry's id").binding())
            .output("application/octet-stream")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_name_is_never_a_path() {
        assert_eq!(bare_name("../../etc/passwd"), "passwd");
        assert_eq!(bare_name("a\\b.txt"), "b.txt");
        assert_eq!(bare_name(".."), "download");
        assert_eq!(bare_name("ok name.pdf"), "ok name.pdf");
    }

    #[test]
    fn chunks_split_by_rows_and_by_bytes() {
        let small: Vec<String> = (0..25)
            .map(|i| format!("<view:entry id=\"{i}\"/>"))
            .collect();
        assert_eq!(chunks(&small).len(), 3);
        let big = "x".repeat(render::MAX_CHUNK_BYTES / 3);
        // Two rows of a third of the bound are over half of it together: one chunk each.
        let rows = vec![big.clone(), big.clone(), big];
        assert_eq!(chunks(&rows).len(), 3);
        let small = "x".repeat(render::MAX_CHUNK_BYTES / 5);
        assert_eq!(chunks(&[small.clone(), small]).len(), 1);
    }

    #[test]
    fn the_door_refuses_a_name_outside_the_three_prefixes() {
        struct Never;
        impl Door for Never {
            fn call(&self, _: Request) -> std::result::Result<Representation, Error> {
                panic!("nothing outside the prefixes reaches the door")
            }
        }
        let door = Confined(Arc::new(Never));
        let refused = door
            .call(get("urn:iki:ledger:default:items", &[]))
            .unwrap_err();
        assert!(matches!(refused, Error::Denied(_)), "{refused:?}");
    }
}
