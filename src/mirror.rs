//! The OFFER role of a layer binding ([`crate::layer`]): this server's ledger, mirrored into a
//! task layer (ledger [#1190](http://localhost:1060/l/default/item/1190); design sections 8 and
//! 10).
//!
//! # The pass
//!
//! Level-triggered and idempotent: every pass reads what IS (the layer's pending tuples and its
//! receipts, the ledger's items and the offers recorded on them) and acts on the difference from
//! what SHOULD be, so running it twice in a row changes nothing the second time.
//!
//! 1. **Reports first, in feed order.** A pending `Progress`, `Release` or `Handback` on one of
//!    our offers is addressed to us; it is taken, by its subject, in `ik:seq` order (never
//!    take-first, whose order is the intray's). Every report we have ever taken is then applied,
//!    and applying one twice is a no-op: a `Handback` is a comment keyed by the report's IRI
//!    with `handback=true` (so a replay comments nothing and cannot re-set a hand-back the
//!    dispatcher has cleared), a `Release` is a keyed comment plus the release of the taker's
//!    claim while that offer is still the item's newest, and a `Progress` renews the taker's
//!    lease when it is taken (and comments, keyed, when it carries a note).
//! 2. **Then each item.** An open, unclaimed item that is not awaiting its dispatcher has a
//!    pending offer; an offer whose item is closed, deleted, claimed outside the layer or
//!    handed back is withdrawn (taken back by us); a taken offer's item is claimed for its taker
//!    for [`LEASE`]; an offer released by its taker is replaced by a new one.
//!
//! ★ **Nothing closes across.** The mirror holds `urn:cap:ledger:{read,claim,comment}:{ledger}`
//! (the worker grant, [`crate::grants::Authority::Worker`]) and never `close`: the taker's
//! hand-back makes the item wait for its dispatcher, who checks the evidence and closes it.
//!
//! # Where an offer is recorded
//!
//! On its item, as a COMMENT keyed `layer:{layer}:offered:<offer IRI>`, written before the offer
//! is dropped. The design said `about <offer IRI>`, and `item:{id}:about` needs
//! `urn:cap:ledger:write:{ledger}`, which is the close grant too; a keyed comment is the record
//! the worker grant CAN write, it is idempotent, and it shows the person reading the item where
//! its work went. A crash between the record and the drop leaves a recorded offer the layer has
//! never seen, which the next pass drops (the same subject, so the layer's replay rule makes a
//! retry one record).
//!
//! # What crosses
//!
//! An offer is a fixed CONSTRUCT over the ledger's graph that names only the item's title, its
//! body as the BRIEF, one `https://github.com/{owner}/{repo}` it is `about` as the repository,
//! and the minted offer IRI; then `urn:jsonld:prune` against the layer's own context, and the
//! mirror REFUSES to publish when prune dropped anything (a drop is a CONSTRUCT bug to see, not
//! absorb); then the layer host's closed shapes. Labels, links, comments, claims, numbers and
//! ledger IRIs cannot appear.
//!
//! # A take that races a withdrawal
//!
//! Both are takes of one tuple, and exactly one wins. If ours does, the offer is withdrawn and
//! the taker was refused. If the taker's does, the take stands: the mirror comments that it
//! happened after a withdrawal was attempted (keyed, so once), claims the item for the taker
//! when it is open and free, and leaves the rest to the dispatcher. It never reverses a take.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ikigai_core::{ArgRef, Capability, Error, Kernel, ListenSpec, Verb};
use ikigai_ledger::claim::ClaimKind;
use ikigai_ledger::json::{Item, ItemsDocument};
use ikigai_ledger::vocabulary as v;
use ikigai_ledger::Ledger;
use ikigai_resolve::Resolver;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::Term;

use crate::layer::{self, Binding, Door};

/// How long a taker's claim is held between its reports: renewed by every `Progress`.
pub const LEASE: &str = "7d";

/// The backstop: a pass at least this often, whatever the feed and the ledger say.
pub const SWEEP: Duration = Duration::from_secs(5 * 60);

/// How long one feed read waits for an event (the layer host's bound is 60 s).
pub const FEED_WAIT_SECS: u64 = 50;

/// How long a failed feed read is held off before the next.
pub const FEED_RETRY: Duration = Duration::from_secs(30);

/// An answer faster than this that did not move the cursor did not wait as it was asked to
/// ([`FEED_WAIT_SECS`]), so the next read is held off for [`FEED_RETRY`].
///
/// ★ The audit of 2026-10-11: an answer that is not JSON, events with no `last`, a host that
/// ignores `wait`, or a `Conflict` at 0 used to be read again AT ONCE, for ever (152,377
/// reads in 300 ms), each read naming events kicking a whole pass. A host that waits answers an
/// idle feed after [`FEED_WAIT_SECS`], and an answer that moves the cursor is never held off.
pub const FEED_QUICK: Duration = Duration::from_secs(1);

/// The most tuples or receipts one `rd` page asks for (the layer host's own maximum).
const PAGE: usize = 1000;

/// The most `rd` pages one listing follows: a page of 1000 (the host's maximum) times this, a
/// hundred thousand tuples or receipts, is far past any layer one collaboration fills (receipts
/// are kept for the life of a layer, so they are the listing that grows).
///
/// ★ Found by the audit of 2026-10-11: the listing followed pages for as long as the host said
/// a page was full and its positions rose, so a hostile or broken host held a pass (and its
/// memory) for ever. A listing past the bound is refused, and says so.
pub const MAX_PAGES: usize = 100;

/// The most bytes one listing reads, whatever the page count: one wire answer is already
/// bounded (64 MiB), and this bounds the sum.
pub const MAX_LISTING_BYTES: usize = 128 * 1024 * 1024;

/// Who the layer door says the caller is (ikigai-layers 0.2.2).
const ME: &str = "urn:layers:me";

const IK: &str = "https://ikigai-rs.dev/ns#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const PROV_ATTRIBUTED: &str = "http://www.w3.org/ns/prov#wasAttributedTo";
const PROV_USED: &str = "http://www.w3.org/ns/prov#used";
const PROV_ASSOCIATED: &str = "http://www.w3.org/ns/prov#wasAssociatedWith";
const PROV_ENDED: &str = "http://www.w3.org/ns/prov#endedAtTime";
const FOAF_PERSON: &str = "http://xmlns.com/foaf/0.1/Person";

fn ik(local: &str) -> String {
    format!("{IK}{local}")
}

/// One tuple, as the layer holds it (pending, or inside a receipt as taken).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tuple {
    /// Its subject: the name its dropper minted.
    pub subject: String,
    /// Its classes (`ik:Offer`, `ik:Progress`, `ik:Release`, `ik:Handback`).
    pub types: BTreeSet<String>,
    /// Who dropped it, as the host stamped it.
    pub dropper: Option<String>,
    /// The offer a report is about.
    pub offer: Option<String>,
    /// Its place in the layer's feed.
    pub seq: u64,
    /// A report's note.
    pub note: Option<String>,
    /// A hand-back's evidence.
    pub evidence: Vec<String>,
    /// An offer's title.
    pub title: Option<String>,
    /// An offer's brief.
    pub brief: Option<String>,
    /// An offer's repository.
    pub repository: Option<String>,
}

impl Tuple {
    /// Whether it is typed `ik:{class}`.
    pub fn is(&self, class: &str) -> bool {
        self.types.contains(&ik(class))
    }

    fn is_report(&self) -> bool {
        self.is("Progress") || self.is("Release") || self.is("Handback")
    }
}

/// One take, from its receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    /// Who took it, as the door named them.
    pub taker: String,
    /// When.
    pub time: String,
    /// The take's place in the feed.
    pub seq: u64,
    /// The tuple as taken.
    pub tuple: Tuple,
}

/// What one pass did, for the log and the tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Lines, one per action, in the order they happened.
    pub actions: Vec<String>,
    /// What could not be done this pass (the next pass tries again).
    pub problems: Vec<String>,
}

impl Report {
    pub(crate) fn did(&mut self, line: String) {
        self.actions.push(line);
    }

    pub(crate) fn failed(&mut self, line: String) {
        self.problems.push(line);
    }
}

/// Whether the binding is usable: checked against the layer's own definition once.
enum Check {
    Unchecked,
    Ready { context: String },
    Refused(String),
}

/// One binding's offer side.
pub struct Mirror {
    binding: Binding,
    layer: LayerClient,
    hub: Arc<Kernel>,
    capability: Capability,
    principal: String,
    /// `urn:jsonld:*`, in a kernel only this mirror issues to.
    prune: Kernel,
}

/// A task layer, through a binding's door: its definition (checked once to be a tuplespace),
/// its pending tuples, its receipts, take and out. Shared by both roles.
pub struct LayerClient {
    door: Arc<dyn Door>,
    layer: String,
    check: Mutex<Check>,
    receipts: Mutex<Receipts>,
}

/// The receipts read so far, and the cursor past them.
///
/// ★ Ledger [#1336](http://localhost:1060/l/default/item/1336): receipts are kept for the life of
/// a layer, and every pass read all of them (twice when it took a report), so a pass cost the
/// layer's whole history. A receipt never changes once the host has written it, and the host
/// numbers them in the order it records them, so what was read is kept and only receipts past
/// the newest held are asked for.
#[derive(Default)]
struct Receipts {
    /// The newest receipt's `ik:seq`: the next listing asks for what comes after it.
    after: u64,
    /// The subject taken in the newest receipt held, re-read before each listing: a host that
    /// no longer answers that receipt, or answers another, is not the history the rest came
    /// from (restored from a backup, or another host at the same address).
    newest: Option<String>,
    taken: BTreeMap<String, Taken>,
}

impl LayerClient {
    /// Layer `layer`, through `door`.
    pub fn new(door: Arc<dyn Door>, layer: &str) -> LayerClient {
        LayerClient {
            door,
            layer: layer.to_string(),
            check: Mutex::new(Check::Unchecked),
            receipts: Mutex::new(Receipts::default()),
        }
    }

    /// The layer's name.
    pub fn layer(&self) -> &str {
        &self.layer
    }

    /// The door, shared.
    pub fn door(&self) -> Arc<dyn Door> {
        Arc::clone(&self.door)
    }

    pub fn iri(&self, part: &str) -> String {
        if part.is_empty() {
            format!("urn:layer:{}", self.layer)
        } else {
            format!("urn:layer:{}:{part}", self.layer)
        }
    }

    /// The layer's context, after checking (once) that the layer is a tuplespace.
    pub fn ready(&self) -> Result<String, String> {
        let mut check = self.check.lock().unwrap_or_else(|p| p.into_inner());
        match &*check {
            Check::Ready { context } => return Ok(context.clone()),
            Check::Refused(why) => return Err(why.clone()),
            Check::Unchecked => {}
        }
        let definition = self
            .door
            .call(layer::request(Verb::Source, &self.iri("")))
            .map_err(|e| format!("reading {}: {e}", self.iri("")))?;
        let triples = parse_turtle(&definition.bytes)?;
        let model = triples
            .iter()
            .find(|(s, p, _)| *s == self.iri("") && *p == ik("layerModel"))
            .map(|(_, _, o)| o.value().to_string());
        if model.as_deref() != Some("tuplespace") {
            let why =
                format!(
                "layer `{}` is {}, and only a task layer (`tuplespace`) is bound to a ledger: the \
                 binding is refused",
                self.layer,
                model.map_or("of no stated model".to_string(), |m| format!("a `{m}` layer"))
            );
            *check = Check::Refused(why.clone());
            return Err(why);
        }
        let context = self
            .door
            .call(layer::request(Verb::Source, &self.iri("context")))
            .map_err(|e| format!("reading {}: {e}", self.iri("context")))?;
        let context = String::from_utf8(context.bytes)
            .map_err(|_| format!("{} is not UTF-8", self.iri("context")))?;
        *check = Check::Ready {
            context: context.clone(),
        };
        Ok(context)
    }

    /// `rd`, every page after `after`: the pending tuples (`taken = false`) or the receipts.
    fn read_tuples(&self, taken: bool, mut after: u64) -> Result<String, String> {
        let mut all = String::new();
        for _ in 0..MAX_PAGES {
            let mut request = layer::request(Verb::Source, &self.iri("tuples"))
                .with_arg("as", inline("text/turtle"))
                .with_arg("after", inline(&after.to_string()))
                .with_arg("limit", inline(&PAGE.to_string()));
            if taken {
                request = request.with_arg("state", inline("taken"));
            }
            let page = self
                .door
                .call(request)
                .map_err(|e| format!("reading {}: {e}", self.iri("tuples")))?;
            let text = String::from_utf8(page.bytes)
                .map_err(|_| format!("{} answered non-UTF-8", self.iri("tuples")))?;
            if all.len() + text.len() > MAX_LISTING_BYTES {
                return Err(format!(
                    "{} answered more than {MAX_LISTING_BYTES} bytes in one listing: refused",
                    self.iri("tuples")
                ));
            }
            let triples = parse_turtle(text.as_bytes())?;
            // The page's positions: a tuple's or a receipt's own ik:seq (a receipt also holds
            // its tuple's, which is never larger than the take's).
            let seqs: Vec<u64> = triples
                .iter()
                .filter(|(_, p, _)| *p == ik("seq"))
                .filter_map(|(_, _, o)| o.value().parse().ok())
                .collect();
            let count = if taken {
                triples
                    .iter()
                    .filter(|(s, p, _)| *p == PROV_USED && is_receipt(s))
                    .count()
            } else {
                triples
                    .iter()
                    .map(|(s, _, _)| s.as_str())
                    .collect::<BTreeSet<_>>()
                    .len()
            };
            all.push_str(&text);
            all.push('\n');
            match seqs.into_iter().max() {
                Some(last) if count >= PAGE && last > after => after = last,
                _ => return Ok(all),
            }
        }
        Err(format!(
            "{} {} still had more after {MAX_PAGES} pages of {PAGE}: refused rather than \
             followed",
            self.iri("tuples"),
            if taken { "(receipts)" } else { "(pending)" }
        ))
    }

    pub fn pending(&self) -> Result<BTreeMap<String, Tuple>, String> {
        let text = self.read_tuples(false, 0)?;
        Ok(tuples(&parse_turtle(text.as_bytes())?))
    }

    /// Every take in the layer, by the subject taken: those held from earlier calls, and only
    /// the receipts past them read from the host: a receipt never changes, so it is read once.
    ///
    /// # Errors
    ///
    /// When the host cannot be read, or the receipts held would pass [`MAX_PAGES`] pages of
    /// a thousand: the bound a single listing has, kept for the sum.
    pub fn receipts(&self) -> Result<BTreeMap<String, Taken>, String> {
        let mut held = self.receipts.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(subject) = held.newest.clone() {
            let still = self.receipt_of(&subject)?;
            if still.as_ref() != held.taken.get(&subject) {
                *held = Receipts::default();
            }
        }
        let text = self.read_tuples(true, held.after)?;
        let fresh = receipts(&parse_turtle(text.as_bytes())?);
        for (subject, take) in fresh {
            if take.seq > held.after {
                held.after = take.seq;
                held.newest = Some(subject.clone());
            }
            held.taken.insert(subject, take);
        }
        if held.taken.len() > MAX_PAGES * PAGE {
            let count = held.taken.len();
            *held = Receipts::default();
            return Err(format!(
                "{} holds {count} receipts, more than {MAX_PAGES} pages of {PAGE}: refused \
                 rather than kept",
                self.iri("tuples")
            ));
        }
        Ok(held.taken.clone())
    }

    /// Take `subject` from the layer. `Ok(true)` when this take won.
    pub fn take(&self, subject: &str) -> Result<bool, Error> {
        let request =
            layer::request(Verb::Delete, &self.iri("tuples")).with_arg("tuple", inline(subject));
        match self.door.call(request) {
            Ok(_) => Ok(true),
            Err(Error::Conflict(_)) | Err(Error::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Who the layer door says THIS server is: the principal it minted for our connection,
    /// read from `urn:layers:me as=application/json` (ikigai-layers 0.2.2, ledger
    /// [#1316](http://localhost:1060/l/default/item/1316)).
    ///
    /// `Ok(None)` only when the host does not serve that resource (a host on ikigai-layers
    /// 0.2.1 or older answers `NotFound` or `Unresolved`): the caller then falls back to
    /// inferring its principal from the records it holds, as gonk did before.
    /// Any other failure is an error, never a guess.
    ///
    /// # Errors
    ///
    /// When the door refuses or fails, or the answer is not the JSON face naming a principal.
    pub fn me(&self) -> Result<Option<String>, String> {
        let request = layer::request(Verb::Source, ME).with_arg("as", inline("application/json"));
        let answer = match self.door.call(request) {
            Ok(answer) => answer,
            Err(Error::NotFound(_)) | Err(Error::Unresolved(_)) => return Ok(None),
            Err(e) => return Err(format!("reading {ME}: {e}")),
        };
        let json: serde_json::Value = serde_json::from_slice(&answer.bytes)
            .map_err(|e| format!("{ME} answered no JSON: {e}"))?;
        json.get("principal")
            .and_then(serde_json::Value::as_str)
            .map(|principal| Some(principal.to_string()))
            .ok_or_else(|| format!("{ME} names no principal: {json}"))
    }

    /// The receipt of `subject`'s take, re-read after a take that lost.
    pub fn receipt_of(&self, subject: &str) -> Result<Option<Taken>, String> {
        let request = layer::request(Verb::Source, &self.iri("tuples"))
            .with_arg("state", inline("taken"))
            .with_arg("tuple", inline(subject))
            .with_arg("as", inline("text/turtle"));
        match self.door.call(request) {
            Ok(r) => Ok(receipts(&parse_turtle(&r.bytes)?).remove(subject)),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(format!("reading the receipt of <{subject}>: {e}")),
        }
    }

    /// `subject` as it is pending in the layer (`rd tuple=`), or `None` when it is not pending:
    /// taken, gone, or never dropped (the host answers `NotFound` for each, and a take says
    /// which).
    ///
    /// # Errors
    ///
    /// When the door fails otherwise, or the answer is not the tuple's Turtle.
    pub fn read_pending(&self, subject: &str) -> Result<Option<Tuple>, String> {
        let request = layer::request(Verb::Source, &self.iri("tuples"))
            .with_arg("tuple", inline(subject))
            .with_arg("as", inline("text/turtle"));
        match self.door.call(request) {
            Ok(r) => Ok(tuples(&parse_turtle(&r.bytes)?).remove(subject)),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(format!("reading <{subject}>: {e}")),
        }
    }

    /// Take `subject`, answering the tuple as taken (Turtle), or the layer's refusal.
    pub fn take_tuple(&self, subject: &str) -> Result<String, Error> {
        let request =
            layer::request(Verb::Delete, &self.iri("tuples")).with_arg("tuple", inline(subject));
        let answer = self.door.call(request)?;
        String::from_utf8(answer.bytes)
            .map_err(|_| Error::Endpoint(format!("the take of <{subject}> answered non-UTF-8")))
    }

    /// Drop one tuple (Turtle or JSON-LD).
    pub fn out(&self, content: Vec<u8>) -> Result<(), String> {
        let request = layer::request(Verb::Sink, &self.iri("tuples"))
            .with_arg("content", ArgRef::Inline(content));
        self.door
            .call(request)
            .map(|_| ())
            .map_err(|e| format!("dropping into {}: {e}", self.iri("tuples")))
    }
}

/// The capability a mirror writes to its ledger under: the worker grant (read, claim, comment,
/// and the ledger graph's store tokens), acting as the mirror's own principal. Never `close`.
pub fn capability(binding: &Binding) -> Result<Capability, String> {
    let scopes = crate::grants::grants_for(&binding.ledger, crate::grants::Authority::Worker)?;
    Capability::scoped(scopes)
        .with_principal(&binding.principal())
        .map_err(|e| format!("the mirror's principal {}: {e:?}", binding.principal()))
}

impl Mirror {
    /// The offer side of `binding`, reaching the layer through `door` and the ledger through
    /// `hub`.
    ///
    /// # Errors
    ///
    /// When the binding is not an offer binding, or its capability cannot be built.
    pub fn new(binding: Binding, door: Arc<dyn Door>, hub: Arc<Kernel>) -> Result<Self, String> {
        if binding.role != layer::Role::Offer {
            return Err(format!(
                "layer `{}` is bound in the {} role, not offer",
                binding.layer,
                binding.role.as_str()
            ));
        }
        let capability = capability(&binding)?;
        Ok(Mirror {
            principal: binding.principal(),
            layer: LayerClient::new(door, &binding.layer),
            binding,
            hub,
            capability,
            prune: Kernel::new(Arc::new(ikigai_jsonld::space())),
        })
    }

    /// The binding this mirror serves.
    pub fn binding(&self) -> &Binding {
        &self.binding
    }

    /// What the collaboration's participants graph says each taker is.
    fn refresh_kinds(&self, report: &mut Report) {
        let Some(collab) = &self.binding.collab else {
            return;
        };
        let iri = format!("urn:collab:{collab}:participants");
        let answer = self
            .layer
            .door
            .call(layer::request(Verb::Source, &iri).with_arg("as", inline("text/turtle")))
            .map_err(|e| e.to_string())
            .and_then(|r| parse_turtle(&r.bytes));
        match answer {
            Ok(triples) => {
                let mut kinds = BTreeMap::new();
                for (s, p, o) in &triples {
                    if *p == RDF_TYPE && !s.starts_with("_:") {
                        let kind = kinds.entry(s.clone()).or_insert(ClaimKind::Machine);
                        if o.value() == FOAF_PERSON {
                            *kind = ClaimKind::Person;
                        }
                    }
                }
                layer::record_kinds(&self.principal, &kinds);
            }
            Err(e) => report.failed(format!(
                "reading {iri} (claim kinds stay as last read): {e}"
            )),
        }
    }

    /// Drop an offer for `item` under `offer`, built and pruned here.
    fn drop_offer(&self, item: &Item, offer: &str, context: &str) -> Result<(), String> {
        let document = self.build_offer(item, offer, context)?;
        self.layer
            .out(document)
            .map_err(|e| format!("<{offer}>: {e}"))
    }

    /// The offer for `item`: the fixed CONSTRUCT, then `urn:jsonld:prune` against the layer's
    /// context, refused when prune dropped anything. Answers the JSON-LD the layer is sent.
    pub fn build_offer(&self, item: &Item, offer: &str, context: &str) -> Result<Vec<u8>, String> {
        let ledger = Ledger::parse(&self.binding.ledger).map_err(|e| e.to_string())?;
        let query = offer_construct(&item.iri, offer);
        let constructed = self.ledger_issue(
            layer::request(Verb::Source, "urn:iki:store:graph-construct")
                .with_arg("graph", inline(ledger.graph().as_str()))
                .with_arg("query", inline(&query))
                .with_arg("as", inline("text/turtle")),
        )?;
        let triples = parse_turtle(&constructed)?;
        if !triples.iter().any(|(_, p, _)| *p == ik("brief")) {
            return Err(format!(
                "{} has no body, and the body is an offer's brief: nothing to offer",
                item.display
            ));
        }
        let expanded = expanded_json_ld(&triples);
        prune_strict(&self.prune, &expanded, context)
    }

    // ---- the ledger side ------------------------------------------------------------------

    fn ledger_issue(&self, request: ikigai_core::Request) -> Result<Vec<u8>, String> {
        let target = request.target.to_string();
        Resolver::issue_as(&*self.hub, request, &self.capability)
            .map(|(r, _)| r.bytes)
            .map_err(|e| format!("{target}: {e}"))
    }

    fn ledger_request(&self, verb: Verb, resource: &str) -> ikigai_core::Request {
        layer::request(
            verb,
            &format!("urn:iki:ledger:{}:{resource}", self.binding.ledger),
        )
    }

    /// Every item of the ledger, open and closed, and whether the listing was the whole set.
    fn items(&self) -> Result<(Vec<Item>, bool), String> {
        let mut items: Vec<Item> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut total = 0;
        loop {
            let bytes = self.ledger_issue(
                self.ledger_request(Verb::Source, "items")
                    .with_arg("status", inline("all"))
                    .with_arg("limit", inline("0"))
                    .with_arg("offset", inline(&items.len().to_string()))
                    .with_arg("as", inline("application/json")),
            )?;
            let page: ItemsDocument = serde_json::from_slice(&bytes)
                .map_err(|e| format!("the ledger's items are not its JSON document: {e}"))?;
            total = page.total.unwrap_or(page.count).max(total);
            let got = page.items.len();
            for item in page.items {
                if seen.insert(item.iri.clone()) {
                    items.push(item);
                }
            }
            if got == 0 || items.len() >= total {
                let complete = items.len() == total;
                return Ok((items, complete));
            }
        }
    }

    /// The offers recorded on each item: `item IRI → offer IRIs`, from the comment keys.
    fn recorded(&self) -> Result<BTreeMap<String, Vec<String>>, String> {
        let ledger = Ledger::parse(&self.binding.ledger).map_err(|e| e.to_string())?;
        let prefix = offered_key(&self.binding.layer, "");
        let query = format!(
            "SELECT ?item ?key WHERE {{ ?c <{on}> ?item ; <{key}> ?key . \
             FILTER(STRSTARTS(STR(?key), \"{prefix}\")) }}",
            on = v::ON_ITEM,
            key = v::COMMENT_KEY,
        );
        let bytes = self.ledger_issue(
            layer::request(Verb::Source, "urn:iki:store:graph-select")
                .with_arg("graph", inline(ledger.graph().as_str()))
                .with_arg("query", inline(&query)),
        )?;
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("the store's SELECT answer is not JSON: {e}"))?;
        let mut recorded: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for binding in json
            .pointer("/results/bindings")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let value = |name: &str| binding.pointer(&format!("/{name}/value"))?.as_str();
            if let (Some(item), Some(key)) = (value("item"), value("key")) {
                if let Some(offer) = key.strip_prefix(&prefix) {
                    recorded
                        .entry(item.to_string())
                        .or_default()
                        .push(offer.to_string());
                }
            }
        }
        for offers in recorded.values_mut() {
            offers.sort();
            offers.dedup();
        }
        Ok(recorded)
    }

    /// A comment on `item` under `key`; a key already used writes nothing.
    fn comment(
        &self,
        item: &Item,
        key: &str,
        text: &str,
        handback: bool,
        report: &mut Report,
    ) -> Result<(), String> {
        let mut request = self
            .ledger_request(Verb::Sink, "comment")
            .with_arg("item", inline(&item.number.to_string()))
            .with_arg("key", inline(key))
            .with_arg("author", inline(&self.principal))
            .with_arg("as", inline("application/json"))
            .with_arg("content", inline(text));
        if handback {
            request = request.with_arg("handback", inline("true"));
        }
        let answer = self.ledger_issue(request)?;
        let outcome = serde_json::from_slice::<serde_json::Value>(&answer)
            .ok()
            .and_then(|j| j.get("outcome")?.as_str().map(str::to_string));
        if outcome.as_deref() != Some("existing") {
            report.did(format!("commented on {} ({key})", item.display));
        }
        Ok(())
    }

    fn claim(&self, item: &Item, taker: &str, renew: bool) -> Result<(), String> {
        let mut request = self
            .ledger_request(Verb::Sink, "claim")
            .with_arg("item", inline(&item.number.to_string()))
            .with_arg("lease", inline(LEASE))
            .with_arg(
                "purpose",
                inline(&format!("taken in layer {}", self.binding.layer)),
            )
            .with_arg("content", inline(taker));
        if renew {
            request = request.with_arg("renew", inline("true"));
        }
        self.ledger_issue(request).map(|_| ())
    }

    fn release(&self, item: &Item, taker: &str) -> Result<(), String> {
        self.ledger_issue(
            self.ledger_request(Verb::Delete, "claim")
                .with_arg("item", inline(&item.number.to_string()))
                .with_arg("from", inline(taker)),
        )
        .map(|_| ())
    }

    // ---- the pass -------------------------------------------------------------------------

    /// One pass. See the module docs for what it does, and why twice is once.
    ///
    /// # Errors
    ///
    /// When the binding is refused (a layer that is not a tuplespace) or what the pass must
    /// read cannot be read; an action that fails is a [`Report::problems`] line instead, and
    /// the next pass tries again.
    pub fn pass(&self) -> Result<Report, String> {
        let context = self.layer.ready()?;
        let mut report = Report::default();
        self.refresh_kinds(&mut report);
        let mut pending = self.layer.pending()?;
        let mut taken = self.layer.receipts()?;
        let recorded = self.recorded()?;
        let (mut items, complete) = self.items()?;

        // Our offers: every offer recorded on a live item.
        let mut ours: BTreeMap<String, String> = BTreeMap::new();
        for (item, offers) in &recorded {
            for offer in offers {
                ours.insert(offer.clone(), item.clone());
            }
        }
        let offerer =
            |offer: &str, pending: &BTreeMap<String, Tuple>, taken: &BTreeMap<String, Taken>| {
                pending
                    .get(offer)
                    .and_then(|t| t.dropper.clone())
                    .or_else(|| taken.get(offer).and_then(|t| t.tuple.dropper.clone()))
            };

        // 1. Take the reports on our offers that are still pending, in feed order.
        let mut reports: Vec<Tuple> = pending
            .values()
            .filter(|t| t.is_report() && t.offer.as_ref().is_some_and(|o| ours.contains_key(o)))
            .cloned()
            .collect();
        reports.sort_by_key(|t| t.seq);
        let mut fresh: BTreeSet<String> = BTreeSet::new();
        for tuple in reports {
            match self.layer.take(&tuple.subject) {
                Ok(true) => {
                    report.did(format!("took report <{}>", tuple.subject));
                    fresh.insert(tuple.subject.clone());
                }
                Ok(false) => {}
                Err(e) => {
                    report.failed(format!("taking report <{}>: {e}", tuple.subject));
                    continue;
                }
            }
            pending.remove(&tuple.subject);
        }
        if !fresh.is_empty() {
            taken = self.layer.receipts()?;
        }

        // 2. Apply every report we hold, in feed order. Idempotent: keyed comments, and a
        //    release only while the offer is its item's newest and the taker still holds it.
        let newest: BTreeMap<String, String> = recorded
            .iter()
            .filter_map(|(item, offers)| Some((item.clone(), offers.last()?.clone())))
            .collect();
        let mut applied: Vec<&Taken> = taken
            .values()
            .filter(|r| r.tuple.is_report())
            .filter(|r| {
                r.tuple.offer.as_ref().is_some_and(|o| {
                    ours.contains_key(o)
                        && offerer(o, &pending, &taken).as_deref() == Some(&r.taker)
                })
            })
            .collect();
        applied.sort_by_key(|r| r.seq);
        let mut released: BTreeSet<String> = BTreeSet::new();
        let mut touched = false;
        let applied: Vec<Taken> = applied.into_iter().cloned().collect();
        for r in &applied {
            let offer = r.tuple.offer.clone().unwrap_or_default();
            let Some(item) = items.iter().find(|i| Some(&i.iri) == ours.get(&offer)) else {
                continue;
            };
            let Some(taker) = taken.get(&offer).map(|t| t.taker.clone()) else {
                continue;
            };
            let note = r
                .tuple
                .note
                .as_deref()
                .map(|n| format!(" Note: {n}"))
                .unwrap_or_default();
            let outcome = if r.tuple.is("Handback") {
                let evidence = r.tuple.evidence.join(" ");
                self.comment(
                    item,
                    &report_key(&self.binding.layer, &r.tuple.subject),
                    &format!(
                        "Handed back in layer {} by <{taker}> with evidence: {evidence}.{note}",
                        self.binding.layer
                    ),
                    true,
                    &mut report,
                )
            } else if r.tuple.is("Release") {
                released.insert(offer.clone());
                let commented = self.comment(
                    item,
                    &report_key(&self.binding.layer, &r.tuple.subject),
                    &format!(
                        "Released in layer {} by <{taker}>; it is offered again.{note}",
                        self.binding.layer
                    ),
                    false,
                    &mut report,
                );
                let held = item.claim.as_ref().is_some_and(|c| c.holder == taker);
                if commented.is_ok() && held && newest.get(&item.iri) == Some(&offer) {
                    match self.release(item, &taker) {
                        Ok(()) => {
                            report.did(format!("released {} from <{taker}>", item.display));
                            touched = true;
                        }
                        Err(e) => report.failed(format!("releasing {}: {e}", item.display)),
                    }
                }
                commented
            } else {
                // Progress: renew a lease only when this pass took the report, so a sweep
                // does not extend a lease nobody is reporting on.
                let held = item.claim.as_ref().is_some_and(|c| c.holder == taker);
                if fresh.contains(&r.tuple.subject) && held && item.status == "open" {
                    match self.claim(item, &taker, true) {
                        Ok(()) => {
                            report.did(format!("renewed {} for <{taker}>", item.display));
                            touched = true;
                        }
                        Err(e) => report.failed(format!("renewing {}: {e}", item.display)),
                    }
                }
                match &r.tuple.note {
                    Some(n) => self.comment(
                        item,
                        &report_key(&self.binding.layer, &r.tuple.subject),
                        &format!(
                            "Progress in layer {} from <{taker}>: {n}",
                            self.binding.layer
                        ),
                        false,
                        &mut report,
                    ),
                    None => Ok(()),
                }
            };
            if let Err(e) = outcome {
                report.failed(format!("applying report <{}>: {e}", r.tuple.subject));
            } else {
                touched = touched || r.tuple.is("Handback");
            }
        }
        if touched {
            items = self.items()?.0;
        }

        // 3. Each item against its newest offer.
        // Who we are at the door: what it says (`urn:layers:me`), and only from a host that
        // does not serve that, the dropper of an offer we recorded, which knows nothing until
        // we have one (ledger #1316).
        let known_offerer = match self.layer.me() {
            Ok(Some(me)) => Some(me),
            Ok(None) => ours.keys().find_map(|o| offerer(o, &pending, &taken)),
            Err(e) => {
                report.failed(format!("{e} (withdrawing orphaned offers waits a pass)"));
                None
            }
        };
        for item in &items {
            let offers = recorded.get(&item.iri).cloned().unwrap_or_default();
            let open = item.status == "open";
            let holder = item.claim.as_ref().map(|c| c.holder.clone());
            let awaiting = item.awaiting.is_some();
            let wants_offer = open && holder.is_none() && !awaiting;
            // An older offer still pending was superseded: withdraw it.
            for older in offers.iter().rev().skip(1) {
                if pending.contains_key(older) {
                    self.withdraw(item, older, "superseded by a newer offer", &mut report);
                }
            }
            let Some(newest) = offers.last() else {
                if wants_offer {
                    self.offer(item, None, &context, &mut report);
                }
                continue;
            };
            if pending.contains_key(newest) {
                let why = if !open {
                    Some("closed")
                } else if awaiting {
                    Some("handed back, awaiting its dispatcher")
                } else if holder.is_some() {
                    Some("claimed outside the layer")
                } else {
                    None
                };
                if let Some(why) = why {
                    self.withdraw(item, newest, why, &mut report);
                }
                continue;
            }
            match taken.get(newest) {
                None => {
                    // Recorded, never dropped (a crash between the two): drop it now, under
                    // the name the record already gives it.
                    if wants_offer {
                        self.offer(item, Some(newest), &context, &mut report);
                    }
                }
                Some(t)
                    if Some(&t.taker) == t.tuple.dropper.as_ref() || released.contains(newest) =>
                {
                    // Withdrawn by us, or released by its taker: a fresh offer when it wants one.
                    if wants_offer {
                        self.offer(item, None, &context, &mut report);
                    }
                }
                Some(t) => {
                    if !open || awaiting {
                        continue;
                    }
                    match holder.as_deref() {
                        None => self.claim_for(item, &t.taker, newest, &mut report),
                        Some(h) if h == t.taker => {}
                        Some(h) => {
                            let text = format!(
                                "Taken in layer {} by <{}> at {}, but held here by {h}: both \
                                 stand, and the dispatcher decides.",
                                self.binding.layer, t.taker, t.time
                            );
                            let key = format!("layer:{}:conflict:{newest}", self.binding.layer);
                            if let Err(e) = self.comment(item, &key, &text, false, &mut report) {
                                report.failed(format!("commenting on {}: {e}", item.display));
                            }
                        }
                    }
                }
            }
        }

        // 4. Our offers pending in the layer for an item that is no longer live (deleted).
        if let (true, Some(us)) = (complete, known_offerer) {
            let live: BTreeSet<&str> = items.iter().map(|i| i.iri.as_str()).collect();
            let orphans: Vec<String> = pending
                .values()
                .filter(|t| t.is("Offer") && t.dropper.as_deref() == Some(us.as_str()))
                .filter(|t| {
                    ours.get(&t.subject)
                        .is_none_or(|i| !live.contains(i.as_str()))
                })
                .map(|t| t.subject.clone())
                .collect();
            for offer in orphans {
                match self.layer.take(&offer) {
                    Ok(true) => report.did(format!("withdrew <{offer}> (its item is gone)")),
                    Ok(false) => {}
                    Err(e) => report.failed(format!("withdrawing <{offer}>: {e}")),
                }
            }
        }
        Ok(report)
    }

    /// Offer `item`: under `existing` (a recorded offer the layer never got), else a new name,
    /// recorded on the item before it is dropped.
    fn offer(&self, item: &Item, existing: Option<&String>, context: &str, report: &mut Report) {
        let offer = existing
            .cloned()
            .unwrap_or_else(|| format!("{}:offer:{}", self.binding.layer_iri(), mint_id()));
        // Built first: an item that cannot be offered records nothing.
        if let Err(e) = self.build_offer(item, &offer, context) {
            report.failed(format!("not offering {}: {e}", item.display));
            return;
        }
        if existing.is_none() {
            let key = offered_key(&self.binding.layer, &offer);
            let text = format!("Offered in layer {} as <{offer}>.", self.binding.layer);
            if let Err(e) = self.comment(item, &key, &text, false, report) {
                report.failed(format!("recording the offer of {}: {e}", item.display));
                return;
            }
        }
        match self.drop_offer(item, &offer, context) {
            Ok(()) => report.did(format!("offered {} as <{offer}>", item.display)),
            Err(e) => report.failed(e),
        }
    }

    /// Withdraw `offer` by taking it back; a take that loses the race is the taker's, and
    /// stands.
    fn withdraw(&self, item: &Item, offer: &str, why: &str, report: &mut Report) {
        match self.layer.take(offer) {
            Ok(true) => {
                report.did(format!("withdrew <{offer}> ({} is {why})", item.display));
                return;
            }
            Ok(false) => {}
            Err(e) => {
                report.failed(format!("withdrawing <{offer}>: {e}"));
                return;
            }
        }
        let taken = match self.layer.receipt_of(offer) {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(e) => {
                report.failed(e);
                return;
            }
        };
        if Some(&taken.taker) == taken.tuple.dropper.as_ref() {
            return; // an earlier withdrawal of ours
        }
        let attempted = ikigai_ledger::sparql::iso8601(now_millis());
        let text = format!(
            "Taken in layer {} by <{}> at {}, after this server tried to withdraw it at \
             {attempted} ({} is {why}). The take stands; the dispatcher decides.",
            self.binding.layer, taken.taker, taken.time, item.display
        );
        let key = format!("layer:{}:late-take:{offer}", self.binding.layer);
        if let Err(e) = self.comment(item, &key, &text, false, report) {
            report.failed(format!("commenting on {}: {e}", item.display));
        }
        let free = item.status == "open" && item.claim.is_none() && item.awaiting.is_none();
        if free {
            self.claim_for(item, &taken.taker, offer, report);
        }
    }

    fn claim_for(&self, item: &Item, taker: &str, offer: &str, report: &mut Report) {
        match self.claim(item, taker, false) {
            Ok(()) => report.did(format!(
                "claimed {} for <{taker}> (took <{offer}>)",
                item.display
            )),
            Err(e) => report.failed(format!("claiming {} for <{taker}>: {e}", item.display)),
        }
    }
}

/// The comment key a report is applied under: the report's own IRI (design section 10), or,
/// when the taker minted one a ledger key cannot spell (keys are `[A-Za-z0-9-._~:]`, at most
/// 256 characters), `layer:{layer}:report:sha256:<hex>` of it.
///
/// ```
/// use ikigai_gonk::mirror::report_key;
/// let plain = "urn:layer:tasks:report:01m4abc";
/// assert_eq!(report_key("tasks", plain), plain);
/// assert!(report_key("tasks", "urn:layer:tasks:report/a b").starts_with("layer:tasks:report:sha256:"));
/// ```
pub fn report_key(layer: &str, report: &str) -> String {
    if ikigai_ledger::model::parse_key(report).is_ok() {
        return report.to_string();
    }
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(report.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("layer:{layer}:report:sha256:{hex}")
}

/// The comment key an offer is recorded under on its item.
pub fn offered_key(layer: &str, offer: &str) -> String {
    format!("layer:{layer}:offered:{offer}")
}

/// What an offer's repository must be, whole: `https://github.com/<owner>/<repo>` and nothing
/// else, in GitHub's own name characters (an owner is letters, digits and hyphens, with `_` for
/// an enterprise-managed account; a repository adds `.`).
///
/// ★ Ledger [#1336](http://localhost:1060/l/default/item/1336), from the audit of 2026-10-11:
/// the pattern was `^https://github[.]com/[^/]+/[^/]+$`, which admits a query string, a fragment
/// and a percent-escape, so `https://github.com/o/r?access_token=…` on an item's `about` crossed
/// to the layer in its offer. No `?`, `#`, `%`, `@` or further path can match this.
pub const REPOSITORY: &str = "^https://github[.]com/[A-Za-z0-9_-]+/[A-Za-z0-9._-]+$";

/// The fixed CONSTRUCT an offer is built from: the item's title (at most 200 characters, the
/// shape's bound), its body as the brief, and one GitHub repository it is about ([`REPOSITORY`]).
fn offer_construct(item: &str, offer: &str) -> String {
    format!(
        "PREFIX dcterms: <http://purl.org/dc/terms/>\n\
         PREFIX ik: <{IK}>\n\
         CONSTRUCT {{ <{offer}> a ik:Offer ; dcterms:title ?title ; ik:brief ?brief ; \
         ik:repository ?repository . }}\n\
         WHERE {{\n\
           <{item}> dcterms:title ?t .\n\
           OPTIONAL {{ <{item}> <{body}> ?b }}\n\
           BIND(SUBSTR(STR(?t), 1, 200) AS ?title)\n\
           BIND(STR(?b) AS ?brief)\n\
           OPTIONAL {{ SELECT (MIN(STR(?a)) AS ?r) WHERE {{ <{item}> <{about}> ?a . \
             FILTER(isIRI(?a) && REGEX(STR(?a), \"{REPOSITORY}\")) }} }}\n\
           BIND(IRI(?r) AS ?repository)\n\
         }}",
        body = v::BODY,
        about = v::ABOUT,
    )
}

/// Turtle, as `(subject, predicate, object)`; a blank node's subject is `_:label`.
pub(crate) fn parse_turtle(bytes: &[u8]) -> Result<Vec<(String, String, Term)>, String> {
    RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(bytes)
        .map(|quad| {
            let quad = quad.map_err(|e| format!("not Turtle: {e}"))?;
            Ok((
                match &quad.subject {
                    oxigraph::model::NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
                    oxigraph::model::NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
                },
                quad.predicate.as_str().to_string(),
                quad.object,
            ))
        })
        .collect()
}

trait TermValue {
    fn value(&self) -> &str;
}

impl TermValue for Term {
    fn value(&self) -> &str {
        match self {
            Term::NamedNode(n) => n.as_str(),
            Term::BlankNode(b) => b.as_str(),
            Term::Literal(l) => l.value(),
            #[allow(unreachable_patterns)] // oxigraph's rdf-12 feature adds a triple term
            _ => "",
        }
    }
}

/// Whether `subject` is a receipt's: `urn:layer:{layer}:receipt:{id}`, a name only the layer
/// host mints.
///
/// ★ The audit of 2026-10-11: a receipt holds the tuple it took, and the tuple is its DROPPER's
/// content. Recognized by its statements (`prov:used`, `prov:wasAssociatedWith`), a receipt could
/// be forged from inside a report whose layer's shapes name those properties, and the mirror
/// claimed an item for a principal that never took anything. A tuple can never carry this name:
/// the host refuses `receipt` as the first word of a tuple's subject.
///
/// ```
/// use ikigai_gonk::mirror::is_receipt;
/// assert!(is_receipt("urn:layer:tasks:receipt:01m4abc"));
/// assert!(!is_receipt("urn:layer:tasks:report:01m4abc"));
/// assert!(!is_receipt("urn:layer:tasks:report:receipt:1"));
/// assert!(!is_receipt("urn:example:receipt:1"));
/// ```
pub fn is_receipt(subject: &str) -> bool {
    subject
        .strip_prefix("urn:layer:")
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(_, local)| local.starts_with("receipt:"))
}

/// The tuples in `triples`, by subject (receipt subjects excluded).
pub(crate) fn tuples(triples: &[(String, String, Term)]) -> BTreeMap<String, Tuple> {
    let mut out: BTreeMap<String, Tuple> = BTreeMap::new();
    for (s, p, o) in triples {
        if s.starts_with("_:") || is_receipt(s) || *p == PROV_USED {
            continue;
        }
        let t = out.entry(s.clone()).or_insert_with(|| Tuple {
            subject: s.clone(),
            ..Tuple::default()
        });
        match p.as_str() {
            RDF_TYPE => {
                t.types.insert(o.value().to_string());
            }
            PROV_ATTRIBUTED => t.dropper = Some(o.value().to_string()),
            p if p == ik("offer") => t.offer = Some(o.value().to_string()),
            p if p == ik("seq") => t.seq = o.value().parse().unwrap_or(0),
            p if p == ik("note") => t.note = Some(o.value().to_string()),
            p if p == ik("evidence") => t.evidence.push(o.value().to_string()),
            p if p == ik("brief") => t.brief = Some(o.value().to_string()),
            p if p == ik("repository") => t.repository = Some(o.value().to_string()),
            "http://purl.org/dc/terms/title" => t.title = Some(o.value().to_string()),
            _ => {}
        }
    }
    for t in out.values_mut() {
        t.evidence.sort();
    }
    // A receipt's subject is under `…:receipt:`; anything typed only as a prov:Activity is one.
    out.retain(|_, t| !t.types.contains("http://www.w3.org/ns/prov#Activity"));
    out
}

/// The takes in `triples` (receipt documents), by the subject taken.
fn receipts(triples: &[(String, String, Term)]) -> BTreeMap<String, Taken> {
    let tuples = tuples(triples);
    /// One receipt's own statements: what it used, who took it, when, and its place.
    #[derive(Default)]
    struct Take {
        used: Option<String>,
        taker: Option<String>,
        time: Option<String>,
        seq: u64,
    }
    let mut by_receipt: BTreeMap<&str, Take> = BTreeMap::new();
    for (s, p, o) in triples {
        if !is_receipt(s) {
            continue; // a tuple's own statements, never a take
        }
        let entry = by_receipt.entry(s.as_str()).or_default();
        match p.as_str() {
            PROV_USED => entry.used = Some(o.value().to_string()),
            PROV_ASSOCIATED => entry.taker = Some(o.value().to_string()),
            PROV_ENDED => entry.time = Some(o.value().to_string()),
            p if p == ik("seq") => entry.seq = o.value().parse().unwrap_or(0),
            _ => {}
        }
    }
    by_receipt
        .into_values()
        .filter_map(|take| {
            let used = take.used?;
            Some((
                used.clone(),
                Taken {
                    taker: take.taker?,
                    time: take.time.unwrap_or_default(),
                    seq: take.seq,
                    tuple: tuples.get(&used).cloned().unwrap_or(Tuple {
                        subject: used,
                        ..Tuple::default()
                    }),
                },
            ))
        })
        .collect()
}

/// `triples` as an expanded JSON-LD document, one node object per subject.
fn expanded_json_ld(triples: &[(String, String, Term)]) -> Vec<u8> {
    let mut nodes: BTreeMap<&str, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for (s, p, o) in triples {
        let node = nodes.entry(s.as_str()).or_insert_with(|| {
            let mut m = serde_json::Map::new();
            m.insert("@id".into(), s.clone().into());
            m
        });
        let (key, value) = if *p == RDF_TYPE {
            ("@type".to_string(), serde_json::Value::from(o.value()))
        } else {
            let value = match o {
                Term::Literal(l) => {
                    let mut v = serde_json::Map::new();
                    v.insert("@value".into(), l.value().into());
                    if let Some(lang) = l.language() {
                        v.insert("@language".into(), lang.into());
                    } else if l.datatype().as_str() != XSD_STRING {
                        v.insert("@type".into(), l.datatype().as_str().into());
                    }
                    serde_json::Value::Object(v)
                }
                other => serde_json::json!({ "@id": other.value() }),
            };
            (p.clone(), value)
        };
        node.entry(key)
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
            .expect("every value here is an array")
            .push(value);
    }
    serde_json::to_vec(&nodes.into_values().collect::<Vec<_>>()).expect("JSON serializes")
}

/// `urn:jsonld:prune` of `document` against `context`, REFUSED when prune dropped anything:
/// what the CONSTRUCT emits is exactly what the context allows, so a drop is a bug to see.
///
/// # Errors
///
/// A drop (naming what prune removed), or prune refusing the context or the document.
pub fn prune_strict(prune: &Kernel, document: &[u8], context: &str) -> Result<Vec<u8>, String> {
    let call = |face: &str| {
        let request = layer::request(Verb::Source, "urn:jsonld:prune")
            .with_arg("content", ArgRef::Inline(document.to_vec()))
            .with_arg("context", inline(context))
            .with_arg("face", inline(face));
        Resolver::issue(prune, request)
            .map(|(r, _)| r.bytes)
            .map_err(|e| format!("urn:jsonld:prune: {e}"))
    };
    let removed: serde_json::Value = serde_json::from_slice(&call("report")?)
        .map_err(|e| format!("urn:jsonld:prune's report is not JSON: {e}"))?;
    let dropped = removed
        .get("dropped")
        .and_then(serde_json::Value::as_u64)
        .ok_or("urn:jsonld:prune's report has no `dropped`")?;
    if dropped != 0 {
        return Err(format!(
            "refused to publish: urn:jsonld:prune removed {dropped} thing(s) the layer's context \
             does not define ({removed}); the offer's CONSTRUCT must emit only what the layer \
             allows"
        ));
    }
    call("document")
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

/// A new offer's local name: the time in milliseconds (10 Crockford base-32 characters, so the
/// names sort by when they were minted) and 16 random ones.
pub fn mint_id() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut out = String::with_capacity(26);
    let millis = now_millis();
    for i in (0..10).rev() {
        out.push(ALPHABET[((millis >> (i * 5)) & 31) as usize] as char);
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).expect("the operating system's random source");
    for byte in random {
        out.push(ALPHABET[(byte & 31) as usize] as char);
    }
    out
}

// ---- running it ---------------------------------------------------------------------------

/// Run `mirror` for the life of the process: a pass now, on every event in the layer's feed, on
/// every cut under the ledger's names, and every [`SWEEP`] whatever happens.
pub fn start(mirror: Mirror) {
    let mirror = Arc::new(mirror);
    let (kick, kicks) = sync_channel::<()>(1);
    let layer = mirror.binding.layer.clone();
    spawn_feed(
        mirror.layer.door(),
        layer.clone(),
        format!("gonk-layer-feed-{layer}"),
        kick.clone(),
    );
    if let Err(e) = spawn_listener(
        &mirror.hub,
        &mirror.binding.ledger,
        format!("gonk-layer-cuts-{layer}"),
        kick.clone(),
    ) {
        log(&mirror, &e);
    }
    let _ = kick.try_send(());
    let worker = Arc::clone(&mirror);
    std::thread::Builder::new()
        .name(format!("gonk-layer-{layer}"))
        .spawn(move || run_passes(&worker, &kicks))
        .expect("spawning the mirror's thread");
}

fn run_passes(mirror: &Mirror, kicks: &Receiver<()>) {
    let mut last = String::new();
    loop {
        match kicks.recv_timeout(SWEEP) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let said = match mirror.pass() {
            Ok(report) => {
                for line in &report.actions {
                    log(mirror, line);
                }
                report.problems.join("; ")
            }
            Err(e) => e,
        };
        // A problem is said once, and again only when it changes: a door that is down for an
        // hour must not fill the log.
        if !said.is_empty() && said != last {
            log(mirror, &said);
        }
        last = said;
    }
}

fn log(mirror: &Mirror, line: &str) {
    let _ = writeln!(
        std::io::stderr(),
        "ikigai-gonk: layer {} → ledger {}: {line}",
        mirror.binding.layer,
        mirror.binding.ledger
    );
}

fn nudge(kick: &SyncSender<()>) -> bool {
    !matches!(kick.try_send(()), Err(TrySendError::Disconnected(_)))
}

/// Follow `layer`'s feed through `door`: each event after the cursor is a kick.
pub(crate) fn spawn_feed(door: Arc<dyn Door>, layer: String, thread: String, kick: SyncSender<()>) {
    std::thread::Builder::new()
        .name(thread)
        .spawn(move || {
            let feed = format!("urn:layer:{layer}:feed");
            let mut after = 0u64;
            loop {
                let request = layer::request(Verb::Source, &feed)
                    .with_arg("after", inline(&after.to_string()))
                    .with_arg("wait", inline(&FEED_WAIT_SECS.to_string()))
                    .with_arg("as", inline("application/json"));
                let asked = std::time::Instant::now();
                match door.call(request) {
                    Ok(answer) => {
                        let json: serde_json::Value =
                            serde_json::from_slice(&answer.bytes).unwrap_or_default();
                        let events = json
                            .get("events")
                            .and_then(serde_json::Value::as_array)
                            .map_or(0, Vec::len);
                        let last = json.get("last").and_then(serde_json::Value::as_u64);
                        let moved = last.is_some_and(|last| last > after);
                        if let Some(last) = last {
                            after = last;
                        }
                        if events > 0 && !nudge(&kick) {
                            return;
                        }
                        if !moved && asked.elapsed() < FEED_QUICK {
                            std::thread::sleep(FEED_RETRY);
                        }
                    }
                    // The host's feed is not the one the cursor came from: start over, once.
                    Err(Error::Conflict(_)) if after > 0 => after = 0,
                    Err(_) => std::thread::sleep(FEED_RETRY),
                }
            }
        })
        .expect("spawning a layer feed thread");
}

/// Listen for cuts under `ledger`'s names on `hub`: every write to it is a kick.
///
/// The default ledger is also written through the short forms (`urn:iki:ledger:append`), which
/// are not under `urn:iki:ledger:default:`, so for it the listener hears every ledger; an extra
/// pass changes nothing. `Err` when the kernel refuses the listener (then the feed and the
/// sweep remain).
pub(crate) fn spawn_listener(
    hub: &Kernel,
    ledger: &str,
    thread: String,
    kick: SyncSender<()>,
) -> Result<(), String> {
    let prefix = if ledger == ikigai_gonk_default_ledger() {
        "urn:iki:ledger:".to_string()
    } else {
        format!("urn:iki:ledger:{ledger}:")
    };
    let listener = hub
        .listen(ListenSpec::new().prefix(prefix), &Capability::root())
        .map_err(|e| format!("no cut listener ({e}): the feed and the sweep remain"))?;
    std::thread::Builder::new()
        .name(thread)
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
                return;
            };
            loop {
                runtime.block_on(listener.wait());
                let _ = listener.drain();
                if !nudge(&kick) {
                    return;
                }
            }
        })
        .expect("spawning a ledger listener thread");
    Ok(())
}

fn ikigai_gonk_default_ledger() -> &'static str {
    crate::config::DEFAULT_LEDGER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_ids_sort_by_time_and_differ() {
        let a = mint_id();
        std::thread::sleep(Duration::from_millis(2));
        let b = mint_id();
        assert_eq!(a.len(), 26);
        assert!(a < b, "{a} then {b}");
        assert_ne!(a[10..], b[10..]);
    }

    /// A layer door whose every feed read answers at once with `answer`, counting the reads.
    struct AtOnce<F> {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        answer: F,
    }

    impl<F> Door for AtOnce<F>
    where
        F: Fn(u64) -> Result<ikigai_core::Representation, Error> + Send + Sync,
    {
        fn call(
            &self,
            request: ikigai_core::Request,
        ) -> Result<ikigai_core::Representation, Error> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let after = match request.args.get("after") {
                Some(ArgRef::Inline(bytes)) => std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|a| a.parse().ok())
                    .unwrap_or(0),
                _ => 0,
            };
            (self.answer)(after)
        }
    }

    fn json(text: String) -> Result<ikigai_core::Representation, Error> {
        Ok(ikigai_core::Representation::new(
            ikigai_core::ReprType::new("application/json"),
            text.into_bytes(),
        ))
    }

    /// Follow a feed through a door answering with `answer` for a moment; the reads it made.
    fn reads_in_a_moment<F>(answer: F) -> usize
    where
        F: Fn(u64) -> Result<ikigai_core::Representation, Error> + Send + Sync + 'static,
    {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let door = Arc::new(AtOnce {
            calls: Arc::clone(&calls),
            answer,
        });
        let (kick, kicks) = sync_channel::<()>(1);
        spawn_feed(door, "t".to_string(), "test-feed".to_string(), kick);
        // Drain the kicks so a feed that kicks is never stopped by a full channel.
        let drained = std::thread::spawn(move || while kicks.recv().is_ok() {});
        std::thread::sleep(Duration::from_millis(300));
        drop(drained);
        calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The audit of 2026-10-11: an answer that came back at once and did not move the cursor
    /// (not JSON, events with no `last`, a host that ignores `wait`, a `Conflict` at 0) was
    /// read again at once, for ever, and kicked a whole pass each time it named events.
    #[test]
    fn a_feed_that_answers_at_once_without_moving_is_held_off() {
        let garbage = reads_in_a_moment(|_| json("not json".to_string()));
        let unmoving = reads_in_a_moment(|_| json(r#"{"events":[{"seq":1}]}"#.to_string()));
        let no_wait = reads_in_a_moment(|after| json(format!(r#"{{"last":{after},"events":[]}}"#)));
        let conflict = reads_in_a_moment(|_| Err(Error::Conflict("another feed".to_string())));
        for (what, reads) in [
            ("not JSON", garbage),
            ("events that do not move the cursor", unmoving),
            ("an empty answer at once", no_wait),
            ("a Conflict at 0", conflict),
        ] {
            assert!(reads <= 2, "{what}: {reads} reads in 300 ms");
        }
        // And a feed that MOVES is followed at once, however fast it answers.
        let moving = reads_in_a_moment(|after| {
            json(format!(
                r#"{{"last":{},"events":[{{"seq":{}}}]}}"#,
                after + 1,
                after + 1
            ))
        });
        assert!(moving > 10, "a moving feed was held off: {moving} reads");
    }

    #[test]
    fn receipts_and_tuples_are_read_apart() {
        let text = r#"
            @prefix ik: <https://ikigai-rs.dev/ns#> .
            @prefix prov: <http://www.w3.org/ns/prov#> .
            <urn:layer:t:receipt:1> a prov:Activity ; prov:used <urn:layer:t:offer:a> ;
                prov:wasAssociatedWith <urn:p:bob> ; prov:endedAtTime "2026-10-11T00:00:00Z" ;
                ik:seq 3 .
            <urn:layer:t:offer:a> a ik:Offer ; prov:wasAttributedTo <urn:p:us> ; ik:seq 1 .
        "#;
        let triples = parse_turtle(text.as_bytes()).unwrap();
        let taken = receipts(&triples);
        let a = &taken["urn:layer:t:offer:a"];
        assert_eq!(a.taker, "urn:p:bob");
        assert_eq!(a.seq, 3);
        assert_eq!(a.tuple.dropper.as_deref(), Some("urn:p:us"));
        assert!(a.tuple.is("Offer"));
        assert_eq!(tuples(&triples).len(), 1, "the receipt is not a tuple");
    }
}
