//! The TAKE role of a layer binding ([`crate::layer`]): offers in a task layer, taken into this
//! server's own ledger and reported on (ledger [#1190](http://localhost:1060/l/default/item/1190);
//! design section 8, "his side").
//!
//! - **Taking is a person's decision**: `ikigai-gonk layer take <offer IRI>` ([`take`]); an
//!   auto-take policy is v2. The take is the layer's own atomic take, and on success the offer
//!   is filed into the bound ledger with `append key=layer:{layer}:<the offer's local name>
//!   about=<offer IRI>`: keyed, so a take run twice files once. A tuple that is not an offer is
//!   read first and refused, so nothing is taken by mistake. A take whose reply was lost is
//!   found again from its receipt, and when the receipt names THIS server (as the layer door
//!   says, `urn:layers:me`) the take is ours and the item is filed.
//! - **Reports follow the local item** ([`Taker::pass`]), each a tuple addressed to the offer's
//!   offerer under a name derived from what it reports, so a pass run twice drops once (the
//!   layer's replay rule, one subject one tuple): while the item is open, a `Progress` a day,
//!   which renews the offerer's lease on the claim it holds for us; once it is closed `done`, a
//!   `Handback` whose evidence is every pull request URL in its body and comments (new evidence
//!   is a new hand-back); once it is closed any other way, a `Release`, one per close reason
//!   (a later comment on the closed item is not another release).
//! - **This side never reads or writes the offerer's ledger.** It sees offers and nothing else.
//!
//! ★ "Closed `done` is a hand-back, closed otherwise is a release" is this module's reading of
//! the design's "on his close, a Handback; on his release, a Release": a ledger has no release
//! of an item that nobody claimed, and a close's reason is the one thing a person always says.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Mutex};

use ikigai_core::{ArgRef, Capability, Error, Kernel, Request, Verb};
use ikigai_ledger::json::Item;
use ikigai_resolve::Resolver;

use crate::layer::{self, Binding, Door, Role};
use crate::mirror::{self, LayerClient, Tuple};

/// How a taker reaches its own ledger: one request in, the answer's bytes out.
pub type LedgerIssue = Arc<dyn Fn(Request) -> Result<Vec<u8>, String> + Send + Sync>;

/// The ledger, through the hub, as the binding's own principal holding the write grant (it
/// files, and it never closes or claims: the person working the item does that).
pub fn hub_ledger(binding: &Binding, hub: Arc<Kernel>) -> Result<LedgerIssue, String> {
    let capability = capability(binding)?;
    Ok(Arc::new(move |request: Request| {
        let target = request.target.to_string();
        Resolver::issue_as(&*hub, request, &capability)
            .map(|(r, _)| r.bytes)
            .map_err(|e| format!("{target}: {e}"))
    }))
}

/// The capability a taker files into its ledger under ([`hub_ledger`]): the read grant, the
/// ledger's `write` and its graph's store write, as the binding's own principal — plus the
/// store's host reserve ([`crate::budget::STORE_RESERVE`], ledger
/// [#1598](http://localhost:1062/l/default/item/1598)), because the taker is this server's own
/// job. A priority, not an authority: it reaches no resource.
pub fn capability(binding: &Binding) -> Result<Capability, String> {
    let scopes = crate::grants::grants_for(&binding.ledger, crate::grants::Authority::Read)?
        .into_iter()
        .chain([
            ikigai_ledger::Ledger::parse(&binding.ledger)
                .map_err(|e| e.to_string())?
                .cap_write(),
            ikigai_store::cap_write_graph(
                &ikigai_ledger::Ledger::parse(&binding.ledger)
                    .map_err(|e| e.to_string())?
                    .graph(),
            ),
            crate::budget::STORE_RESERVE.to_string(),
        ])
        .collect::<Vec<_>>();
    Capability::scoped(scopes)
        .with_principal(&binding.principal())
        .map_err(|e| format!("the taker's principal {}: {e:?}", binding.principal()))
}

/// The key an offer is filed under in the taking ledger: `layer:{layer}:<local name>`, or a
/// digest when the offer's name is not one a ledger key can spell.
///
/// ```
/// use ikigai_gonk::taker::filed_key;
/// assert_eq!(
///     filed_key("tasks", "urn:layer:tasks:offer:01m4abc"),
///     "layer:tasks:offer:01m4abc"
/// );
/// ```
pub fn filed_key(layer: &str, offer: &str) -> String {
    let local = offer
        .strip_prefix(&format!("urn:layer:{layer}:"))
        .unwrap_or(offer);
    let key = format!("layer:{layer}:{local}");
    if ikigai_ledger::model::parse_key(&key).is_ok() {
        key
    } else {
        format!("layer:{layer}:sha256:{}", hex_digest(offer))
    }
}

fn hex_digest(text: &str) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A door that is a resolver: the running gonk's socket, whose mounts reach the layer host
/// as this gonk's own identity (`ikigai-gonk layer take`).
pub struct ResolverDoor(pub Arc<dyn Resolver>);

impl Door for ResolverDoor {
    fn call(&self, request: Request) -> Result<ikigai_core::Representation, Error> {
        self.0.issue(request).map(|(r, _)| r)
    }
}

/// What [`take`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Took {
    /// Taken now, and filed as this item number.
    Filed(i64),
    /// Taken earlier (by this server) and filed then, or now after a lost reply.
    Already(i64),
}

/// Take `offer` from `binding`'s layer and file it into the bound ledger.
///
/// # Errors
///
/// When the binding is not a take binding for the offer's layer, the layer is not a
/// tuplespace, the offer does not exist or was taken by someone else, or the ledger refuses.
pub fn take(
    binding: &Binding,
    door: Arc<dyn Door>,
    ledger: &LedgerIssue,
    offer: &str,
) -> Result<Took, String> {
    if binding.role != Role::Take {
        return Err(format!(
            "layer `{}` is bound in the {} role here, not take",
            binding.layer,
            binding.role.as_str()
        ));
    }
    if !offer.starts_with(&format!("{}:", binding.layer_iri())) {
        return Err(format!(
            "<{offer}> is not a tuple of layer `{}`",
            binding.layer
        ));
    }
    let client = LayerClient::new(door, &binding.layer);
    client.ready()?;
    let key = filed_key(&binding.layer, offer);
    let (items, complete) = keyed_items(binding, ledger)?;
    if let Some(item) = items
        .iter()
        .find(|i| i.key.as_deref() == Some(key.as_str()))
    {
        return Ok(Took::Already(item.number));
    }
    // A torn listing's items are real, so one found there is filed; one missing from it may
    // be filed too, and a take is not undone, so nothing is taken on it (ledger #1742).
    if !complete {
        return Err(format!(
            "{TORN}, so whether <{offer}> is filed here already is not known: nothing was \
             taken; run the take again"
        ));
    }
    // ★ Read before taking (ledger #1336): the host's take is by subject alone, with no
    // condition on what the tuple is, so a report's IRI given here by mistake was consumed and
    // only then refused, and its addressee never saw it. Reading first is as good as a
    // conditional take: a subject names ONE tuple for the life of a layer (the host refuses a
    // second drop under it), so what is read is what a take would take, and the only thing that
    // can happen in between is someone else's take, which our take answers as a Conflict.
    if let Some(pending) = client.read_pending(offer)? {
        if !pending.is("Offer") {
            return Err(not_an_offer(offer, &pending, "nothing was taken"));
        }
    }
    let tuple = match client.take_tuple(offer) {
        Ok(text) => mirror::tuples(&mirror::parse_turtle(text.as_bytes())?)
            .remove(offer)
            .ok_or_else(|| format!("the take of <{offer}> answered no such tuple"))?,
        Err(Error::Conflict(why)) => {
            // Taken already: by us, with the reply lost, or by someone else.
            let taken = client
                .receipt_of(offer)?
                .ok_or_else(|| format!("<{offer}>: {why}"))?;
            let us = client
                .me()
                .map_err(|e| format!("<{offer}> was taken by <{}>, and {e}", taken.taker))?;
            if us != taken.taker {
                return Err(format!(
                    "<{offer}> was taken by <{}> at {}",
                    taken.taker, taken.time
                ));
            }
            if !taken.tuple.is("Offer") {
                return Err(not_an_offer(
                    offer,
                    &taken.tuple,
                    "this server took it, and nothing is filed",
                ));
            }
            let number = file(binding, ledger, offer, &key, &taken.tuple)?;
            return Ok(Took::Already(number));
        }
        Err(e) => return Err(format!("taking <{offer}>: {e}")),
    };
    if !tuple.is("Offer") {
        // Unreachable from a host that keeps one tuple per subject (it was read as an offer).
        return Err(not_an_offer(
            offer,
            &tuple,
            "it was taken, and nothing is filed",
        ));
    }
    file(binding, ledger, offer, &key, &tuple).map(Took::Filed)
}

fn not_an_offer(subject: &str, tuple: &Tuple, outcome: &str) -> String {
    let classes: Vec<&str> = tuple
        .types
        .iter()
        .map(|t| t.strip_prefix("https://ikigai-rs.dev/ns#").unwrap_or(t))
        .collect();
    format!(
        "<{subject}> is not an offer ({}): {outcome}",
        if classes.is_empty() {
            "it states no class".to_string()
        } else {
            format!("it is {}", classes.join(", "))
        }
    )
}

/// File `tuple` (an offer) into the ledger, keyed.
fn file(
    binding: &Binding,
    ledger: &LedgerIssue,
    offer: &str,
    key: &str,
    tuple: &Tuple,
) -> Result<i64, String> {
    let title = tuple
        .title
        .clone()
        .unwrap_or_else(|| format!("Offer {offer}"));
    let mut body = tuple.brief.clone().unwrap_or_default();
    body.push_str(&format!(
        "\n\nTaken in layer {} as <{offer}>{}.",
        binding.layer,
        tuple
            .dropper
            .as_deref()
            .map(|d| format!(", offered by <{d}>"))
            .unwrap_or_default()
    ));
    let mut about = offer.to_string();
    if let Some(repository) = &tuple.repository {
        about.push(' ');
        about.push_str(repository);
    }
    let request = ledger_request(binding, Verb::Sink, "append")
        .with_arg("content", inline(&format!("{title}\n\n{body}")))
        .with_arg("key", inline(key))
        .with_arg("about", inline(&about))
        .with_arg("author", inline(&binding.principal()))
        .with_arg("as", inline("application/json"));
    let answer = ledger(request)?;
    let json: serde_json::Value = serde_json::from_slice(&answer)
        .map_err(|e| format!("the ledger's append answer is not JSON: {e}"))?;
    json.pointer("/item/number")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| format!("the ledger's append answer names no item: {json}"))
}

fn ledger_request(binding: &Binding, verb: Verb, resource: &str) -> Request {
    layer::request(
        verb,
        &format!("urn:iki:ledger:{}:{resource}", binding.ledger),
    )
}

/// The bound ledger's items filed from this layer (keyed `layer:{layer}:`), open and closed,
/// each once, and whether the listing was the whole set.
///
/// ★ Read as the mirror reads its ledger ([`mirror::collect_items`], ledger #1742): the ledger
/// reads a page and counts the set in two reads, so an append landing between them tears the
/// listing, and, newest first, the next offset holds an item already read. This loop paged by
/// rows held, so it ended, but it kept every row: the torn listing held that item twice, and a
/// pass reported on it twice. Now each item is kept once, a listing still growing after
/// [`mirror::MAX_PAGES`] pages is refused, and a torn one ends marked partial, for the caller
/// to act on only when it is whole: the append that tore it kicks the pass that reads it whole.
fn keyed_items(binding: &Binding, ledger: &LedgerIssue) -> Result<(Vec<Item>, bool), String> {
    let prefix = format!("layer:{}:", binding.layer);
    let (items, complete) = mirror::collect_items(|offset| {
        let bytes = ledger(
            ledger_request(binding, Verb::Source, "items")
                .with_arg("status", inline("all"))
                .with_arg("limit", inline("0"))
                .with_arg("offset", inline(&offset.to_string()))
                .with_arg("as", inline("application/json")),
        )?;
        serde_json::from_slice(&bytes)
            .map_err(|e| format!("the ledger's items are not its JSON document: {e}"))
    })?;
    let filed = items
        .into_iter()
        .filter(|i| i.key.as_deref().is_some_and(|k| k.starts_with(&prefix)))
        .collect();
    Ok((filed, complete))
}

/// What a caller says when [`keyed_items`] was torn by a write landing while it was read.
const TORN: &str = "the ledger's listing was torn by a write landing while it was read";

/// The offer an item filed from `layer` is about.
fn offer_of(item: &Item, layer: &str) -> Option<String> {
    let prefix = format!("urn:layer:{layer}:");
    item.about.iter().find(|a| a.starts_with(&prefix)).cloned()
}

/// Every pull request URL in `item`'s body and comments, in order, once each.
///
/// ```
/// # // The ledger's Item is non_exhaustive, so the doc builds one from its JSON face.
/// let item: ikigai_ledger::json::Item = serde_json::from_value(serde_json::json!({
///     "number": 1, "display": "#1", "iri": "urn:x", "kind": null, "title": "t",
///     "body": "see https://github.com/o/r/pull/7.", "status": "closed", "closed_reason": "done",
///     "priority": null, "deferred": false, "labels": [], "about": [], "key": null,
///     "author": null, "revision": null, "claim": null, "created": "", "modified": "",
///     "links": [], "comments": [{"id": "urn:c", "author": null, "time": "",
///     "text": "merged: https://github.com/o/r/pull/9 and https://example.com/x"}],
///     "defects": [], "state": null, "awaiting": null
/// })).unwrap();
/// assert_eq!(
///     ikigai_gonk::taker::evidence(&item),
///     ["https://github.com/o/r/pull/7", "https://github.com/o/r/pull/9"]
/// );
/// ```
pub fn evidence(item: &Item) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let texts =
        std::iter::once(item.body.as_str()).chain(item.comments.iter().map(|c| c.text.as_str()));
    for text in texts {
        for word in text.split_whitespace() {
            let url = word.trim_matches(|c: char| {
                !(c.is_ascii_alphanumeric()
                    || c == '/'
                    || c == ':'
                    || c == '.'
                    || c == '-'
                    || c == '_')
            });
            let url = url.trim_end_matches('.');
            if is_pull_request(url) && !found.iter().any(|f| f == url) {
                found.push(url.to_string());
            }
        }
    }
    found
}

/// Whether `url` is, whole, a pull request URL: `https://github.com/{owner}/{repo}/pull/{n}`,
/// in GitHub's own name characters (the [`crate::mirror::REPOSITORY`] rule, so no `?`, `#`, `%`
/// or `@` can ride inside an owner or a repository).
/// What [`evidence`] finds, and the only evidence the offer side copies into its ledger as
/// evidence ([`crate::mirror`], ledger [#1746](http://localhost:1062/l/default/item/1746)).
///
/// ```
/// use ikigai_gonk::taker::is_pull_request;
/// assert!(is_pull_request("https://github.com/ikigai-rs/ikigai-core/pull/41"));
/// assert!(!is_pull_request("http://github.com/o/r/pull/1"));
/// assert!(!is_pull_request("https://github.com/o/r/pull/1/files"));
/// assert!(!is_pull_request("https://github.com/o/r/pull/x"));
/// assert!(!is_pull_request("https://example.com/o/r/pull/1"));
/// assert!(!is_pull_request("https://github.com/o/r?token=x/pull/1"));
/// assert!(!is_pull_request("javascript:alert(1)"));
/// ```
pub fn is_pull_request(url: &str) -> bool {
    url.strip_prefix("https://github.com/")
        .map(|rest| rest.split('/').collect::<Vec<_>>())
        .is_some_and(|parts| {
            parts.len() == 4
                && parts[2] == "pull"
                && !parts[0].is_empty()
                && !parts[1].is_empty()
                && parts[0]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && parts[1]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
                && parts[3].chars().all(|c| c.is_ascii_digit())
                && !parts[3].is_empty()
        })
}

/// One binding's take side, run by the server.
pub struct Taker {
    binding: Binding,
    client: LayerClient,
    ledger: LedgerIssue,
    /// The offers already announced on stderr.
    announced: Mutex<BTreeSet<String>>,
}

impl Taker {
    /// The take side of `binding`.
    ///
    /// # Errors
    ///
    /// When the binding is not a take binding.
    pub fn new(binding: Binding, door: Arc<dyn Door>, ledger: LedgerIssue) -> Result<Self, String> {
        if binding.role != Role::Take {
            return Err(format!(
                "layer `{}` is bound in the {} role, not take",
                binding.layer,
                binding.role.as_str()
            ));
        }
        Ok(Taker {
            client: LayerClient::new(door, &binding.layer),
            binding,
            ledger,
            announced: Mutex::new(BTreeSet::new()),
        })
    }

    /// Take `offer` (see [`take`]).
    ///
    /// # Errors
    ///
    /// As [`take`].
    pub fn take(&self, offer: &str) -> Result<Took, String> {
        take(&self.binding, self.client.door(), &self.ledger, offer)
    }

    /// One pass: announce the offers nobody has taken, and drop the reports each filed item
    /// is owed. Idempotent: a report's name is derived from what it reports.
    ///
    /// # Errors
    ///
    /// When the layer or the ledger cannot be read, or the binding is refused.
    pub fn pass(&self) -> Result<mirror::Report, String> {
        self.client.ready()?;
        let mut report = mirror::Report::default();
        let pending = self.client.pending()?;
        let taken = self.client.receipts()?;
        let (items, complete) = keyed_items(&self.binding, &self.ledger)?;
        // Reports follow a whole listing only (ledger #1742): the write that tore this one
        // kicks the pass that reads it whole. The offers are still announced.
        let items = if complete {
            items
        } else {
            report.failed(format!("{TORN} (reports wait a pass)"));
            Vec::new()
        };
        // Who we are is what the door says (`urn:layers:me`), as for a take, and only that
        // (ledger #1336, #1328): a host that does not serve it is named in the report. Unknown,
        // nothing is reported this pass, and the offers are still announced.
        let us = match self.client.me() {
            Ok(us) => Some(us),
            Err(e) => {
                report.failed(format!("{e} (reports wait a pass)"));
                None
            }
        };

        {
            let mut announced = self.announced.lock().unwrap_or_else(|p| p.into_inner());
            for tuple in pending.values().filter(|t| t.is("Offer")) {
                if announced.insert(tuple.subject.clone()) {
                    report.did(format!(
                        "offer <{}> is open: {} (`ikigai-gonk layer take {}` takes it)",
                        tuple.subject,
                        tuple.title.as_deref().unwrap_or("(untitled)"),
                        tuple.subject
                    ));
                }
            }
        }

        let sent: BTreeSet<&str> = pending
            .keys()
            .map(String::as_str)
            .chain(taken.keys().map(String::as_str))
            .collect();
        let today = &ikigai_ledger::sparql::iso8601(now_millis())[..10];
        for item in &items {
            let Some(offer) = offer_of(item, &self.binding.layer) else {
                continue;
            };
            let Some(receipt) = taken.get(&offer) else {
                continue;
            };
            if us.as_deref() != Some(receipt.taker.as_str()) {
                continue; // not our take (or not yet known to be)
            }
            let Some(offerer) = receipt.tuple.dropper.clone() else {
                report.failed(format!("<{offer}>'s receipt names no offerer"));
                continue;
            };
            let base = format!(
                "{}:report:{}",
                self.binding.layer_iri(),
                local_name(&self.binding.layer, &offer)
            );
            let (subject, class, extra) = if item.status == "open" {
                (
                    format!("{base}:progress:{}", today.replace('-', "")),
                    "Progress",
                    String::new(),
                )
            } else if item.closed_reason.as_deref() == Some("done") {
                let evidence = evidence(item);
                if evidence.is_empty() {
                    report.failed(format!(
                        "{} is closed done with no pull request URL in its body or comments, so \
                         there is nothing to hand back: comment one",
                        item.display
                    ));
                    continue;
                }
                let joined = evidence.join(" ");
                let extra = evidence
                    .iter()
                    .map(|e| format!(" ; ik:evidence <{e}>"))
                    .collect::<String>();
                (
                    format!("{base}:handback:{}", &hex_digest(&joined)[..12]),
                    "Handback",
                    extra,
                )
            } else {
                // Named by the close's REASON, never by `modified`: any later write to the
                // closed item (a comment) moves `modified`, and each move was another Release
                // (the audit of 2026-10-11). The offerer applies a release from the first one;
                // a second close for the same reason has nothing new to say.
                let reason = item.closed_reason.as_deref().unwrap_or("closed");
                (
                    format!("{base}:release:{}", &hex_digest(reason)[..12]),
                    "Release",
                    format!(
                        " ; ik:note {}",
                        turtle_string(&format!("closed here as {reason}"))
                    ),
                )
            };
            if sent.contains(subject.as_str()) {
                continue;
            }
            let content = format!(
                "@prefix ik: <https://ikigai-rs.dev/ns#> .\n<{subject}> a ik:{class} ; \
                 ik:offer <{offer}> ; ik:addressedTo <{offerer}>{extra} .\n"
            );
            match self.client.out(content.into_bytes()) {
                Ok(()) => report.did(format!("reported {class} on <{offer}> ({})", item.display)),
                Err(e) => report.failed(format!("reporting {class} on <{offer}>: {e}")),
            }
        }
        Ok(report)
    }
}

fn local_name(layer: &str, offer: &str) -> String {
    let local = offer
        .strip_prefix(&format!("urn:layer:{layer}:"))
        .unwrap_or(offer);
    if local
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':')
        && local.len() <= 80
    {
        local.to_string()
    } else {
        hex_digest(offer)[..24].to_string()
    }
}

/// A Turtle string literal.
fn turtle_string(text: &str) -> String {
    let escaped: String = text
        .chars()
        .flat_map(|c| match c {
            '\\' => vec!['\\', '\\'],
            '"' => vec!['\\', '"'],
            '\n' => vec!['\\', 'n'],
            '\r' => vec!['\\', 'r'],
            c => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

fn inline(value: &str) -> ArgRef {
    ArgRef::Inline(value.as_bytes().to_vec())
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Run `taker` for the life of the process: a pass now, on every event in the layer's feed, on
/// every write to the bound ledger, and every [`mirror::SWEEP`].
///
/// `feed` is the door the feed's long poll runs on: a door of its own ([`layer::feed_door`]),
/// for the reason [`mirror::start`] gives.
pub fn start(taker: Taker, hub: &Kernel, feed: Arc<dyn Door>) {
    let taker = Arc::new(taker);
    let (kick, kicks) = sync_channel::<()>(1);
    let layer = taker.binding.layer.clone();
    mirror::spawn_feed(
        feed,
        layer.clone(),
        format!("gonk-take-feed-{layer}"),
        kick.clone(),
    );
    if let Err(e) = mirror::spawn_listener(
        hub,
        &taker.binding.ledger,
        format!("gonk-take-cuts-{layer}"),
        kick.clone(),
    ) {
        let _ = writeln!(std::io::stderr(), "ikigai-gonk: layer {layer} (take): {e}");
    }
    let _ = kick.try_send(());
    let worker = Arc::clone(&taker);
    std::thread::Builder::new()
        .name(format!("gonk-take-{layer}"))
        .spawn(move || run_passes(&worker, &kicks))
        .expect("spawning the taker's thread");
}

fn run_passes(taker: &Taker, kicks: &Receiver<()>) {
    let mut problems = mirror::Problems::default();
    let log = |line: &str| {
        let _ = writeln!(
            std::io::stderr(),
            "ikigai-gonk: layer {} → ledger {} (take): {line}",
            taker.binding.layer,
            taker.binding.ledger
        );
    };
    loop {
        match kicks.recv_timeout(mirror::SWEEP) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let said = match taker.pass() {
            Ok(report) => {
                for line in &report.actions {
                    log(line);
                }
                report.problems.join("; ")
            }
            Err(e) => e,
        };
        if let Some(line) = problems.after(said, std::time::Instant::now(), now_millis()) {
            log(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const LAYER: &str = "tasks";

    fn binding() -> Binding {
        Binding {
            layer: LAYER.to_string(),
            role: Role::Take,
            target: crate::mount::Target::Quic {
                authority: "127.0.0.1:1".to_string(),
                certs: std::path::PathBuf::from("/nowhere"),
            },
            ledger: "default".to_string(),
            collab: None,
            deny: None,
        }
    }

    /// One page of the ledger's items, each filed from [`LAYER`], with `total` as the count.
    fn page(numbers: &[i64], total: usize, offset: usize) -> Vec<u8> {
        let items: Vec<serde_json::Value> = numbers
            .iter()
            .map(|n| {
                serde_json::json!({
                    "number": n, "display": format!("#{n}"), "iri": format!("urn:item:{n}"),
                    "kind": null, "title": "t", "body": "b", "status": "open",
                    "closed_reason": null, "priority": null, "deferred": false, "labels": [],
                    "about": [], "key": format!("layer:{LAYER}:offer:{n}"), "author": null,
                    "revision": null, "claim": null, "created": "", "modified": "", "links": [],
                    "comments": [], "defects": [], "state": null, "awaiting": null
                })
            })
            .collect();
        serde_json::to_vec(&serde_json::json!({
            "schema": ikigai_ledger::json::SCHEMA, "ledger": "default", "count": items.len(),
            "total": total, "offset": offset, "items": items, "unreadable": []
        }))
        .unwrap()
    }

    fn offset_of(request: &Request) -> usize {
        match request.args.get("offset") {
            Some(ArgRef::Inline(bytes)) => std::str::from_utf8(bytes).unwrap().parse().unwrap(),
            other => panic!("no inline offset: {other:?}"),
        }
    }

    /// A ledger whose pages are `pages(offset)`, counting how many it was asked for; past
    /// `MAX_PAGES` asks the listing never ends, and the test says so rather than hanging.
    fn ledger(
        asked: Arc<AtomicUsize>,
        pages: impl Fn(usize) -> Vec<u8> + Send + Sync + 'static,
    ) -> LedgerIssue {
        Arc::new(move |request: Request| {
            let n = asked.fetch_add(1, Ordering::SeqCst) + 1;
            let offset = offset_of(&request);
            assert!(
                n <= mirror::MAX_PAGES + 1,
                "asked for offset {offset} {n} times: the listing never ends"
            );
            Ok(pages(offset))
        })
    }

    fn numbers(items: &[Item]) -> Vec<i64> {
        items.iter().map(|i| i.number).collect()
    }

    /// ★ Ledger #1742: an append landing between the ledger's read of a page and its count
    /// tears the listing (ikigai-ledger reads the two in separate snapshots, ledger #1741): the
    /// page holds the one older item, the total says two, and, newest first, the next offset
    /// holds that same item again. The listing held it twice, so a pass dropped its report
    /// twice and said so twice (the host answers the second drop as a replay of the first).
    #[test]
    fn a_torn_listing_holds_each_item_once() {
        let asked = Arc::new(AtomicUsize::new(0));
        let ledger = ledger(Arc::clone(&asked), |offset| page(&[1], 2, offset));
        let (items, complete) = keyed_items(&binding(), &ledger).unwrap();
        assert_eq!(numbers(&items), [1]);
        assert!(!complete, "a torn listing is not the whole set");
    }

    /// Pages that overlap are collected once each, and a listing that reaches its total is
    /// the whole set; an item filed from another layer is not this layer's.
    #[test]
    fn a_whole_listing_is_this_layers_items_once_each() {
        let asked = Arc::new(AtomicUsize::new(0));
        let ledger = ledger(Arc::clone(&asked), |offset| match offset {
            0 => page(&[3, 2], 3, 0),
            _ => page(&[2, 1], 3, offset),
        });
        let (items, complete) = keyed_items(&binding(), &ledger).unwrap();
        assert_eq!(numbers(&items), [3, 2, 1]);
        assert!(complete);
        let other = Binding {
            layer: "other".to_string(),
            ..binding()
        };
        let (items, complete) = keyed_items(&other, &ledger).unwrap();
        assert!(items.is_empty() && complete, "{:?}", numbers(&items));
    }

    /// A ledger that grows faster than it is paged (every page full, the total always ahead)
    /// is refused after a bound, and the refusal says so, rather than followed for ever.
    #[test]
    fn a_listing_that_never_ends_is_refused_after_a_bound() {
        let asked = Arc::new(AtomicUsize::new(0));
        let ledger = ledger(Arc::clone(&asked), |offset| {
            let n = offset as i64 + 1;
            page(&[n], offset + 2, offset)
        });
        let e = keyed_items(&binding(), &ledger).unwrap_err();
        assert!(e.contains("pages"), "{e}");
        assert_eq!(asked.load(Ordering::SeqCst), mirror::MAX_PAGES);
    }
}
