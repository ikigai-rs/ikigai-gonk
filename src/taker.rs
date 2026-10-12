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

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Mutex};

use ikigai_core::{ArgRef, Capability, Error, Kernel, Request, Verb};
use ikigai_ledger::json::{Item, ItemsDocument};
use ikigai_resolve::Resolver;

use crate::layer::{self, Binding, Door, Role};
use crate::mirror::{self, LayerClient, Taken, Tuple};

/// How a taker reaches its own ledger: one request in, the answer's bytes out.
pub type LedgerIssue = Arc<dyn Fn(Request) -> Result<Vec<u8>, String> + Send + Sync>;

/// The ledger, through the hub, as the binding's own principal holding the write grant (it
/// files, and it never closes or claims: the person working the item does that).
pub fn hub_ledger(binding: &Binding, hub: Arc<Kernel>) -> Result<LedgerIssue, String> {
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
        ])
        .collect::<Vec<_>>();
    let capability = Capability::scoped(scopes)
        .with_principal(&binding.principal())
        .map_err(|e| format!("the taker's principal {}: {e:?}", binding.principal()))?;
    Ok(Arc::new(move |request: Request| {
        let target = request.target.to_string();
        Resolver::issue_as(&*hub, request, &capability)
            .map(|(r, _)| r.bytes)
            .map_err(|e| format!("{target}: {e}"))
    }))
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
    let items = keyed_items(binding, ledger)?;
    if let Some(item) = items
        .iter()
        .find(|i| i.key.as_deref() == Some(key.as_str()))
    {
        return Ok(Took::Already(item.number));
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
            let us = ourselves(&client, &items, None)?;
            if us.as_deref() != Some(taken.taker.as_str()) {
                return Err(format!(
                    "<{offer}> was taken by <{}> at {}{}",
                    taken.taker,
                    taken.time,
                    if us.is_none() {
                        " (and the layer host does not serve urn:layers:me and this server has \
                         taken nothing in this layer yet, so it cannot tell whether that was \
                         itself after a lost reply)"
                    } else {
                        ""
                    }
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

/// Who this server is at the layer door: what the door says (`urn:layers:me`, ledger
/// [#1316](http://localhost:1060/l/default/item/1316)), so a take whose reply was lost resolves
/// even before this server has taken anything. Only from a host that does not serve
/// `urn:layers:me` (ikigai-layers 0.2.1 or older) is it inferred, as before: the taker of an offer
/// the bound ledger already holds, which knows nothing until the first take (and is wrong when
/// the ledger holds an item filed for someone else's take: ledger #1336). `receipts` are the
/// layer's, when the caller has read them already.
fn ourselves(
    client: &LayerClient,
    items: &[Item],
    receipts: Option<&BTreeMap<String, Taken>>,
) -> Result<Option<String>, String> {
    if let Some(me) = client.me()? {
        return Ok(Some(me));
    }
    let read;
    let receipts = match receipts {
        Some(receipts) => receipts,
        None => {
            read = client.receipts()?;
            &read
        }
    };
    Ok(items
        .iter()
        .filter_map(|i| offer_of(i, client.layer()))
        .find_map(|o| receipts.get(&o).map(|t| t.taker.clone())))
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

/// The bound ledger's items filed from this layer (keyed `layer:{layer}:`), open and closed.
fn keyed_items(binding: &Binding, ledger: &LedgerIssue) -> Result<Vec<Item>, String> {
    let prefix = format!("layer:{}:", binding.layer);
    let mut items: Vec<Item> = Vec::new();
    let mut offset = 0usize;
    loop {
        let bytes = ledger(
            ledger_request(binding, Verb::Source, "items")
                .with_arg("status", inline("all"))
                .with_arg("limit", inline("0"))
                .with_arg("offset", inline(&offset.to_string()))
                .with_arg("as", inline("application/json")),
        )?;
        let page: ItemsDocument = serde_json::from_slice(&bytes)
            .map_err(|e| format!("the ledger's items are not its JSON document: {e}"))?;
        let got = page.items.len();
        offset += got;
        let total = page.total.unwrap_or(page.count);
        items.extend(
            page.items
                .into_iter()
                .filter(|i| i.key.as_deref().is_some_and(|k| k.starts_with(&prefix))),
        );
        if got == 0 || offset >= total {
            return Ok(items);
        }
    }
}

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
            let is_pr = url
                .strip_prefix("https://github.com/")
                .map(|rest| rest.split('/').collect::<Vec<_>>())
                .is_some_and(|parts| {
                    parts.len() == 4
                        && parts[2] == "pull"
                        && !parts[0].is_empty()
                        && !parts[1].is_empty()
                        && parts[3].chars().all(|c| c.is_ascii_digit())
                        && !parts[3].is_empty()
                });
            if is_pr && !found.iter().any(|f| f == url) {
                found.push(url.to_string());
            }
        }
    }
    found
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
        let items = keyed_items(&self.binding, &self.ledger)?;
        // Who we are is what the door says (`urn:layers:me`), as for a take; inferred from the
        // receipts only from a host that does not serve it (ledger #1336). Unknown, nothing is
        // reported this pass, and the offers are still announced.
        let us = match ourselves(&self.client, &items, Some(&taken)) {
            Ok(us) => us,
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
pub fn start(taker: Taker, hub: &Kernel) {
    let taker = Arc::new(taker);
    let (kick, kicks) = sync_channel::<()>(1);
    let layer = taker.binding.layer.clone();
    mirror::spawn_feed(
        taker.client.door(),
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
    let mut last = String::new();
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
        if !said.is_empty() && said != last {
            log(&said);
        }
        last = said;
    }
}
