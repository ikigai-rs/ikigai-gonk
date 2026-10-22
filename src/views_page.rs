//! **The Views page** (ledger [#2099](http://localhost:1062/l/default/item/2099)): the named
//! views ([`crate::views`], ledger #2006) where a person already looks, beside Layers in the
//! header.
//!
//! ```text
//! /views        urn:iki:gonk:page:views   Source  the views in force, the status door, and (with the write token) the forms
//! /views/save   urn:iki:gonk:views:save   Sink    one form → the whole declaration rewritten, then the status door's verdict
//! ```
//!
//! # The map, as built
//!
//! - **What the page lists is what is IN FORCE**: the rules of the arrangement the views space
//!   has bound, read from the hub's topology ([`crate::views::in_force`]), not the text in
//!   the store. After a refused write the two differ, and the page says so beside the refusal:
//!   the old views still answer, and the list shows those.
//! - **The status is the status door's** ([`STATUS`]), read through the invocation like any
//!   other resource: what is in force, how many declarations have bound, and the last refusal.
//! - **The authority is the store's per-graph write token, and nothing of the page's.** A save
//!   reads the declaration and writes it through [`DECLARATION`]'s own `Sink`, both under the
//!   CALLER's capability, so the declaration's contract and the store's
//!   `urn:iki:store:graph-update` beneath it decide. The page offers the forms only to a caller
//!   whose capability holds [`write_grant`]; anyone else gets the page with no controls at
//!   all (not disabled ones), and a post that arrives anyway is refused by the store.
//! - **A save writes the WHOLE declaration, built by core** ([`Topology::to_turtle`]): the
//!   rules in force with one added, changed or removed, as one `ik:Alias` over an empty
//!   `ik:EndpointSpace`. The page never assembles Turtle by hand, so a target cannot inject a
//!   triple. An arrangement of another shape (an operator's own, over the socket) is listed
//!   and never rewritten: the page says it cannot change it without losing what it does not
//!   understand.
//! - **A save carries the digest of the declaration it was drawn from** (`base`); if the
//!   store's has changed since, nothing is written and the page says so, so two people editing
//!   cannot silently undo each other.
//! - **The verdict is waited for**: the rebuild runs on its own thread, so after the write the
//!   save watches the status door until it reports the new declaration's digest as in force
//!   or as refused ([`VERDICT_WAIT`]), and renders the page with that verdict.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, ContentId, Description, Endpoint, Error, Invocation, Iri,
    Representation, Request, ResolveAt, Result, RuleKind, SpaceKind, Topology, TopologyRule, Verb,
};

use std::sync::Arc;

use crate::render::{self, element, envelope};
use crate::views::{read_grant, write_grant, DECLARATION, STATUS, VIEW_PREFIX};
use crate::web::{self, html, html_only, nav_on_views, readable_ledgers, Lacking, Web};

/// The page's path, the header's "Views".
pub const PAGE_PATH: &str = "/views";
/// The page.
pub const PAGE_IRI: &str = "urn:iki:gonk:page:views";
/// Where the page's forms post.
pub const SAVE_PATH: &str = "/views/save";
/// The form adapter.
pub const SAVE_IRI: &str = "urn:iki:gonk:views:save";
/// How long a save waits for the views space to report the new declaration's verdict before
/// it renders the page saying the verdict has not arrived yet. The rebuild is one store read
/// and a build, milliseconds; the bound is for a host under load.
pub const VERDICT_WAIT: Duration = Duration::from_secs(5);
/// The `base` a form carries when no declaration is stored.
const NO_DECLARATION: &str = "none";
/// What a declaration the page writes uses for `ik:maxHops` when nothing is in force.
const MAX_HOPS: usize = 8;

const HTML: &str = "text/html";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The page and its form adapter.
pub struct ViewsPage {
    pub web: Arc<Web>,
}

/// [`SAVE_IRI`]: one form in, the declaration rewritten, the page with the verdict out.
pub struct SaveView {
    pub web: Arc<Web>,
}

// ------------------------------------------------------------------------- the model

/// One rule as the page shows and edits it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// `exact` or `prefix`.
    pub kind: &'static str,
    /// The view's name (or name prefix), under [`VIEW_PREFIX`].
    pub name: String,
    /// What it forwards to.
    pub target: String,
    /// `root` or `child`.
    pub at: &'static str,
}

/// The views in force, as far as the page can say.
#[derive(Debug, Default)]
pub struct InForce {
    /// The rules, in core's table order (most specific first, which is precedence).
    pub rules: Vec<Rule>,
    /// The table's `ik:maxHops`, kept when the page rewrites it.
    pub max_hops: Option<usize>,
    /// Whether the arrangement is the shape the page writes (one alias over an empty endpoint
    /// space), so that a rewrite loses nothing.
    pub rewritable: bool,
}

fn kind_word(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Exact => "exact",
        _ => "prefix",
    }
}

fn at_word(at: ResolveAt) -> &'static str {
    match at {
        ResolveAt::Root => "root",
        _ => "child",
    }
}

/// Read the rules out of an arrangement the views space has bound.
pub fn rules_of(topology: &Topology) -> InForce {
    let SpaceKind::Alias { rules, max_hops } = &topology.kind else {
        return InForce::default();
    };
    let rewritable = matches!(
        topology.children.as_slice(),
        [child] if child.children.is_empty()
            && matches!(&child.kind, SpaceKind::EndpointSpace { doors } if doors.is_empty())
    );
    InForce {
        rules: rules
            .iter()
            .map(|rule| Rule {
                kind: kind_word(rule.kind),
                name: rule.from.clone(),
                target: rule.to.clone(),
                at: at_word(rule.at),
            })
            .collect(),
        max_hops: Some(*max_hops),
        rewritable,
    }
}

/// The declaration a save writes: `rules` as one `ik:Alias` over an empty
/// `ik:EndpointSpace`, rendered by core.
///
/// ```
/// use ikigai_gonk::views_page::{declaration, Rule};
///
/// let turtle = declaration(
///     &[Rule {
///         kind: "exact",
///         name: "urn:iki:gonk:view:open".to_string(),
///         target: "urn:iki:ledger:default:items".to_string(),
///         at: "root",
///     }],
///     8,
/// );
/// assert!(turtle.contains("ik:logical \"urn:iki:gonk:view:open\""));
/// assert!(turtle.contains("ik:resolveAt \"root\""));
/// ```
pub fn declaration(rules: &[Rule], max_hops: usize) -> String {
    let rules = rules
        .iter()
        .map(|rule| {
            let kind = if rule.kind == "prefix" {
                RuleKind::Prefix
            } else {
                RuleKind::Exact
            };
            let at = if rule.at == "child" {
                ResolveAt::Child
            } else {
                ResolveAt::Root
            };
            TopologyRule::new(kind, rule.name.clone(), rule.target.clone()).resolving_at(at)
        })
        .collect();
    Topology::new(SpaceKind::Alias { rules, max_hops })
        .child(Topology::new(SpaceKind::EndpointSpace {
            doors: Vec::new(),
        }))
        .to_turtle()
}

/// The status door's text face, read back (core pins its lines in `tests/dynamic.rs`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Status {
    pub in_force: Option<String>,
    pub installs: u64,
    pub installed_at_ms: Option<u64>,
    pub refused: Option<String>,
    pub refused_at: Option<String>,
    pub refused_digest: Option<String>,
}

/// Parse the status door's `key: value` lines.
///
/// ```
/// use ikigai_gonk::views_page::parse_status;
///
/// let status = parse_status(
///     "space: urn:iki:space:dynamic:views\n\
///      declaration: urn:iki:gonk:views:declaration\n\
///      in force: nothing (the space is empty)\n\
///      installs: 0\n\
///      refused: the declaration could not be read: no views are declared\n",
/// );
/// assert_eq!(status.in_force, None);
/// assert_eq!(status.installs, 0);
/// assert!(status.refused.unwrap().contains("no views are declared"));
/// ```
pub fn parse_status(text: &str) -> Status {
    let mut status = Status::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(": ") else {
            continue;
        };
        let value = value.trim();
        match key {
            "in force" if value.starts_with("b3:") => status.in_force = Some(value.to_string()),
            "installs" => status.installs = value.parse().unwrap_or(0),
            "installed at" => {
                status.installed_at_ms = value
                    .split_whitespace()
                    .next()
                    .and_then(|ms| ms.parse().ok())
            }
            "refused" if value != "nothing since the last bind" => {
                status.refused = Some(value.to_string())
            }
            "refused at" => status.refused_at = Some(value.to_string()),
            "refused digest" => status.refused_digest = Some(value.to_string()),
            _ => {}
        }
    }
    status
}

// ------------------------------------------------------------------------ the reads

/// The status door, or `None` when this hub serves no views.
async fn status(inv: &Invocation<'_>) -> Option<Status> {
    let request = Request::new(Verb::Source, iri(STATUS))
        .with_arg("as", ArgRef::Inline(b"text/plain".to_vec()));
    let answer = inv.issue(request).await.ok()?;
    Some(parse_status(&String::from_utf8_lossy(&answer.bytes)))
}

/// The stored declaration's digest — the one the status door reports for it, since the
/// rebuild reads exactly this representation — or `None` when nothing is stored.
async fn stored(inv: &Invocation<'_>) -> Result<Option<String>> {
    match inv
        .issue(Request::new(Verb::Source, iri(DECLARATION)))
        .await
    {
        Ok(answer) => Ok(Some(ContentId::of(&answer.bytes).to_string())),
        Err(Error::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Wait for something that takes no runtime of its own: a thread sleeps and wakes the
/// future, so this works under whatever executor drives the invocation.
async fn pause(duration: Duration) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("gonk-views-wait".to_string())
        .spawn(move || {
            std::thread::sleep(duration);
            let _ = tx.send(());
        });
    if spawned.is_ok() {
        let _ = rx.await;
    }
}

// ------------------------------------------------------------------------ the page

/// What a caller without the read token is told.
fn lacking() -> Lacking {
    Lacking {
        signed_out: "Sign in to see the named views.".to_string(),
        signed_in: "Your passkey's grant doesn't include the named views.".to_string(),
        needs: vec![(read_grant(), "to read the views declaration".to_string())],
    }
}

fn require_read(inv: &Invocation<'_>) -> Result<()> {
    if inv.capability.allows(&read_grant()) {
        return Ok(());
    }
    let signed_in = inv.capability.principal().is_some();
    Err(Error::Denied(lacking().plain(signed_in)))
}

/// The local part of a view's name, for its form and its `/view/{name}` link.
fn local(name: &str) -> &str {
    name.strip_prefix(VIEW_PREFIX).unwrap_or(name)
}

/// `/view/{local}`, when one path segment can carry the name: the route's `{name}` refuses
/// `: / ? # [ ] @`, so only a name of unreserved characters gets a link.
fn view_href(name: &str) -> Option<String> {
    let local = name.strip_prefix(VIEW_PREFIX)?;
    let plain = !local.is_empty()
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
    plain.then(|| format!("/view/{local}"))
}

/// One flash on the page: `ok` or `error`, and its sentence.
type Flash<'a> = Option<(&'a str, &'a str)>;

impl ViewsPage {
    /// The page, with `flash` at its top when a save produced one.
    pub(crate) async fn body(&self, inv: &Invocation<'_>, flash: Flash<'_>) -> Result<String> {
        require_read(inv)?;
        let ledgers = readable_ledgers(&self.web, inv.capability);
        let mut children = nav_on_views(&self.web, inv.capability, &ledgers);
        if let Some((kind, text)) = flash {
            children.push_str(&element("flash", &[("kind", kind)], text));
        }
        let mut attributes: Vec<(&str, String)> = vec![
            ("view", "views".to_string()),
            ("full", "true".to_string()),
            ("title", "Views".to_string()),
            (
                "intro",
                format!(
                    "A view is a name under {VIEW_PREFIX} that answers what its target answers, \
                     under your own grant. Changing one rewrites the declaration {DECLARATION}; \
                     the views space rebuilds from it, and its status says whether the new \
                     declaration is in force or why it was refused."
                ),
            ),
        ];
        let Some(status) = status(inv).await else {
            children.push_str(&element(
                "notice",
                &[("kind", "error")],
                "This server serves no named views: its views space is not bound.",
            ));
            return page(&attributes, &children);
        };
        let stored = stored(inv).await?;
        let in_force = crate::views::in_force(&self.web.hub)
            .filter(|_| status.installs > 0)
            .map(|topology| rules_of(&topology))
            .unwrap_or(InForce {
                rewritable: true,
                ..InForce::default()
            });
        children.push_str(&status_element(&status, stored.as_deref()));

        let may_write = inv.capability.allows(&write_grant());
        let editable = may_write && in_force.rewritable;
        if !may_write {
            children.push_str(&element(
                "notice",
                &[("kind", "ok")],
                &format!(
                    "You may read the views but not change them: that takes the grant {}.",
                    write_grant()
                ),
            ));
        } else if !in_force.rewritable {
            children.push_str(&element(
                "notice",
                &[("kind", "error")],
                "The arrangement in force is not one alias over an empty endpoint space, so \
                 this page cannot rewrite it without losing what it does not show. Change it \
                 over the socket (see the README's \"Named views\").",
            ));
        }
        let base = stored.unwrap_or_else(|| NO_DECLARATION.to_string());
        if in_force.rules.is_empty() {
            attributes.push(("empty", "No views are in force.".to_string()));
        } else {
            attributes.push(("listed", "true".to_string()));
        }
        if editable {
            attributes.push(("editable", "true".to_string()));
        }
        for (index, rule) in in_force.rules.iter().enumerate() {
            let index = (index + 1).to_string();
            let mut row = vec![
                ("index", index.as_str()),
                ("name", rule.name.as_str()),
                ("local", local(&rule.name)),
                ("kind", rule.kind),
                ("target", rule.target.as_str()),
                ("at", rule.at),
                ("action", SAVE_PATH),
                ("base", base.as_str()),
            ];
            let href = view_href(&rule.name);
            if let (Some(href), "exact") = (href.as_deref(), rule.kind) {
                row.push(("href", href));
            }
            children.push_str(&element("view-rule", &row, ""));
        }
        if editable {
            children.push_str(&element(
                "views-add",
                &[
                    ("action", SAVE_PATH),
                    ("base", &base),
                    ("prefix", VIEW_PREFIX),
                ],
                "",
            ));
        }
        page(&attributes, &children)
    }
}

fn page(attributes: &[(&str, String)], children: &str) -> Result<String> {
    let attributes: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let doc = envelope("page", &attributes, children);
    render::render(&doc, true).map_err(web::render_err)
}

/// The status door's state, in words.
fn status_element(status: &Status, stored: Option<&str>) -> String {
    let in_force = match (&status.in_force, status.installs) {
        (Some(digest), n) => format!("declaration {digest}, the {} to bind", ordinal(n)),
        (None, _) => "nothing: no declaration has bound since gonk started".to_string(),
    };
    let when = status
        .installed_at_ms
        .map(|ms| web::when(&crate::backup::stamp_iso_ms(ms)))
        .unwrap_or_default();
    let refusal = status
        .refused
        .clone()
        .unwrap_or_else(|| "none since the last bind".to_string());
    let stored_sentence = match (stored, &status.in_force) {
        (None, _) => "No declaration is stored.".to_string(),
        (Some(stored), Some(digest)) if stored == digest => {
            "The stored declaration is the one in force.".to_string()
        }
        (Some(stored), _) if status.refused_digest.as_deref() == Some(stored) => {
            "The stored declaration was refused, so the views listed below are the ones that \
             bound before it."
                .to_string()
        }
        (Some(_), _) => "The stored declaration is not in force yet.".to_string(),
    };
    let mut attributes = vec![
        ("in-force", in_force.as_str()),
        ("refusal", refusal.as_str()),
        ("stored", stored_sentence.as_str()),
    ];
    if !when.is_empty() {
        attributes.push(("when", when.as_str()));
    }
    if let Some(at) = status.refused_at.as_deref() {
        attributes.push(("refused-at", at));
    }
    element("views-status", &attributes, "")
}

fn ordinal(n: u64) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[async_trait]
impl Endpoint for ViewsPage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        Ok(html(self.body(inv, None).await?))
    }

    fn name(&self) -> &str {
        "gonk-page-views"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("The named views, as a page")
            .summary(
                "Every named view in force (name, exact or prefix, target, where it resolves), \
                 each exact one a link to /view/{name}, and the views space's status: the \
                 declaration in force and the last refusal and why. A caller holding the \
                 declaration's write token also gets a form per view and one to add a view.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("the page")
                    .requires(read_grant())
                    .input(web::as_html_arg())
                    .output(HTML),
            )
            .verb(Verb::Meta)
    }
}

// ------------------------------------------------------------------------ the save

/// What a form asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Add(Rule),
    Change { was: (String, String), to: Rule },
    Remove { was: (String, String) },
}

fn refused(detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: "content".to_string(),
        detail: detail.into(),
    }
}

/// Read one form: `op` (add, change or remove), `name` (under [`VIEW_PREFIX`], which may be
/// typed or left off), `target`, `kind`, and for a change or removal `was` and `was-kind`.
///
/// ```
/// use ikigai_gonk::views_page::{change_of, Change};
///
/// let fields = [("op", "add"), ("name", "open"), ("target", "urn:iki:ledger:default:items"), ("kind", "exact")];
/// let Change::Add(rule) = change_of(fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()).unwrap() else {
///     panic!()
/// };
/// assert_eq!(rule.name, "urn:iki:gonk:view:open");
/// ```
pub fn change_of(mut fields: std::collections::BTreeMap<String, String>) -> Result<Change> {
    let mut take = |name: &str| {
        fields
            .remove(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let op = take("op").unwrap_or_else(|| "change".to_string());
    let was = match (take("was"), take("was-kind")) {
        (Some(name), Some(kind)) => Some((name, kind)),
        _ => None,
    };
    if op == "remove" {
        let was = was.ok_or_else(|| refused("the form names no view to remove"))?;
        return Ok(Change::Remove { was });
    }
    let name = take("name").ok_or_else(|| refused("a view needs a name"))?;
    let name = format!(
        "{VIEW_PREFIX}{}",
        name.strip_prefix(VIEW_PREFIX).unwrap_or(&name)
    );
    Iri::parse(&name).map_err(|e| refused(format!("`{name}` is not a name: {e}")))?;
    let target = take("target").ok_or_else(|| refused("a view needs a target"))?;
    Iri::parse(&target).map_err(|e| refused(format!("`{target}` is not a name: {e}")))?;
    let kind = match take("kind").as_deref() {
        Some("prefix") => "prefix",
        Some("exact") | None => "exact",
        Some(other) => return Err(refused(format!("`{other}` is not exact or prefix"))),
    };
    let rule = Rule {
        kind,
        name,
        target,
        at: "root",
    };
    match op.as_str() {
        "add" => Ok(Change::Add(rule)),
        "change" => {
            let was = was.ok_or_else(|| refused("the form names no view to change"))?;
            Ok(Change::Change { was, to: rule })
        }
        other => Err(refused(format!("`{other}` is not add, change or remove"))),
    }
}

/// `rules` with `change` made, and what to say about it; or why it cannot be made.
pub fn apply(
    mut rules: Vec<Rule>,
    change: &Change,
) -> std::result::Result<(Vec<Rule>, String), String> {
    let position = |rules: &[Rule], (name, kind): &(String, String)| {
        rules
            .iter()
            .position(|rule| &rule.name == name && rule.kind == kind)
    };
    let taken = |rules: &[Rule], rule: &Rule, except: Option<usize>| {
        rules
            .iter()
            .enumerate()
            .any(|(i, r)| Some(i) != except && r.name == rule.name && r.kind == rule.kind)
    };
    match change {
        Change::Add(rule) => {
            if taken(&rules, rule, None) {
                return Err(format!(
                    "An {} view named {} is already declared; change that one instead.",
                    rule.kind, rule.name
                ));
            }
            rules.push(rule.clone());
            Ok((rules, format!("Added the view {}", rule.name)))
        }
        Change::Change { was, to } => {
            let at = position(&rules, was).ok_or_else(|| {
                format!("No {} view named {} is in force any more.", was.1, was.0)
            })?;
            if taken(&rules, to, Some(at)) {
                return Err(format!(
                    "An {} view named {} is already declared.",
                    to.kind, to.name
                ));
            }
            rules[at] = to.clone();
            Ok((rules, format!("Changed the view {}", to.name)))
        }
        Change::Remove { was } => {
            let at = position(&rules, was).ok_or_else(|| {
                format!("No {} view named {} is in force any more.", was.1, was.0)
            })?;
            rules.remove(at);
            Ok((rules, format!("Removed the view {}", was.0)))
        }
    }
}

impl SaveView {
    /// Wait until the status door reports `digest` in force or refused, or [`VERDICT_WAIT`]
    /// passes; the status it last read.
    async fn verdict(inv: &Invocation<'_>, digest: &str) -> Option<Status> {
        let deadline = Instant::now() + VERDICT_WAIT;
        loop {
            let status = status(inv).await?;
            let decided = status.in_force.as_deref() == Some(digest)
                || status.refused_digest.as_deref() == Some(digest);
            if decided || Instant::now() >= deadline {
                return Some(status);
            }
            pause(Duration::from_millis(25)).await;
        }
    }

    async fn save(&self, inv: &Invocation<'_>) -> Result<(&'static str, String)> {
        let mut fields = web::form(inv.inline_str("content")?);
        let base = fields.remove("base").unwrap_or_default();
        let change = change_of(fields)?;
        // Read first, under the caller's capability: a caller who may not read the
        // declaration is refused here, before anything is written.
        let current = stored(inv).await?;
        if base.trim() != current.as_deref().unwrap_or(NO_DECLARATION) {
            return Ok((
                "error",
                "Nothing was saved: the declaration changed after this page was drawn. The page \
                 below shows it as it is now; make the change again."
                    .to_string(),
            ));
        }
        let in_force = status(inv)
            .await
            .filter(|status| status.installs > 0)
            .and_then(|_| crate::views::in_force(&self.web.hub))
            .map(|topology| rules_of(&topology))
            .unwrap_or(InForce {
                rewritable: true,
                ..InForce::default()
            });
        if !in_force.rewritable {
            return Ok((
                "error",
                "Nothing was saved: the arrangement in force is not one this page can rewrite."
                    .to_string(),
            ));
        }
        let (rules, done) = match apply(in_force.rules, &change) {
            Ok(applied) => applied,
            Err(why) => return Ok(("error", format!("Nothing was saved: {why}"))),
        };
        let turtle = declaration(&rules, in_force.max_hops.unwrap_or(MAX_HOPS));
        // The declaration's own Sink, so the rebuild hears it, under the caller's capability:
        // the declaration's contract and the store's graph write decide.
        let write = Request::new(Verb::Sink, iri(DECLARATION))
            .with_arg("content", ArgRef::Inline(turtle.into_bytes()));
        inv.issue(write).await?;
        let Some(digest) = stored(inv).await? else {
            return Ok((
                "error",
                format!("{done}, but the declaration reads back empty."),
            ));
        };
        Ok(match Self::verdict(inv, &digest).await {
            Some(status) if status.in_force.as_deref() == Some(digest.as_str()) => (
                "ok",
                format!("{done}. The views space bound it: it is in force."),
            ),
            Some(status) if status.refused_digest.as_deref() == Some(digest.as_str()) => (
                "error",
                format!(
                    "{done} in the declaration, but the views space refused it, so the views \
                     in force are unchanged. Why: {}",
                    status.refused.unwrap_or_default()
                ),
            ),
            _ => (
                "error",
                format!(
                    "{done} in the declaration, but the views space has not reported on it \
                     yet. Reload this page to see whether it is in force."
                ),
            ),
        })
    }
}

#[async_trait]
impl Endpoint for SaveView {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Endpoint(format!("`{SAVE_IRI}` answers Sink only")));
        }
        let (kind, text) = match self.save(inv).await {
            Ok(flash) => flash,
            // A refusal to write is the store's answer, and it is returned as one: the
            // caller gets a 403 (gonk's refusal page in a browser), never a page that looks
            // like a save.
            Err(e @ Error::Denied(_)) => return Err(e),
            Err(e) => ("error", format!("Nothing was saved: {e}")),
        };
        let page = ViewsPage {
            web: Arc::clone(&self.web),
        };
        Ok(html(page.body(inv, Some((kind, &text))).await?))
    }

    fn name(&self) -> &str {
        "gonk-views-save"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("Add, change or remove one named view")
            .summary(
                "The Views page's form adapter: an urlencoded body naming `op` (add, change or \
                 remove), `name` (under urn:iki:gonk:view:), `target`, `kind` (exact or \
                 prefix), `was` and `was-kind` (the view a change or removal is about) and \
                 `base` (the digest of the declaration the form was drawn from). It rewrites \
                 the whole declaration through urn:iki:gonk:views:declaration's Sink under the \
                 caller's own capability, waits for the views space's verdict, and answers \
                 with the page and that verdict. The authority is the declaration graph's \
                 store read and write tokens, which it declares; it adds none of its own.",
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("change the views and re-render the page")
                    // Every save reads the declaration and writes it, so both of the store's
                    // tokens for its graph are its floor; the store checks them again beneath.
                    .requires(read_grant())
                    .requires(write_grant())
                    .input(
                        ArgSpec::new("content")
                            .class(XSD_STRING)
                            .summary("the form: op, name, target, kind, was, was-kind, base"),
                    )
                    .output(HTML),
            )
            .verb(Verb::Meta)
    }
}

fn iri(name: &str) -> Iri {
    Iri::parse(name).expect("a constant IRI")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: &'static str, name: &str, target: &str) -> Rule {
        Rule {
            kind,
            name: format!("{VIEW_PREFIX}{name}"),
            target: target.to_string(),
            at: "root",
        }
    }

    #[test]
    fn a_declaration_the_page_writes_reads_back_as_the_same_rules() {
        let rules = vec![
            rule("exact", "next", "urn:iki:ledger:default:next"),
            rule("prefix", "rule:", "urn:iki:gonk:views:"),
        ];
        let turtle = declaration(&rules, 8);
        let read = rules_of(&Topology::from_turtle(&turtle).unwrap());
        assert!(read.rewritable);
        assert_eq!(read.max_hops, Some(8));
        let mut want = rules.clone();
        want.sort_by(|a, b| a.name.cmp(&b.name));
        let mut got = read.rules;
        got.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(got, want);
    }

    #[test]
    fn a_target_cannot_add_a_triple() {
        let hostile = "urn:x\" ; ik:logical \"urn:iki:ledger:items";
        assert!(change_of(
            [("op", "add"), ("name", "x"), ("target", hostile)]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        )
        .is_err());
    }

    #[test]
    fn add_change_and_remove_refuse_what_they_cannot_do() {
        let rules = vec![rule("exact", "a", "urn:x:a")];
        assert!(apply(rules.clone(), &Change::Add(rule("exact", "a", "urn:x:b"))).is_err());
        assert!(apply(rules.clone(), &Change::Add(rule("prefix", "a", "urn:x:b"))).is_ok());
        let gone = (format!("{VIEW_PREFIX}b"), "exact".to_string());
        assert!(apply(rules.clone(), &Change::Remove { was: gone }).is_err());
        let (after, said) = apply(
            rules,
            &Change::Remove {
                was: (format!("{VIEW_PREFIX}a"), "exact".to_string()),
            },
        )
        .unwrap();
        assert!(after.is_empty());
        assert!(said.contains("Removed"));
    }

    #[test]
    fn only_a_plain_name_gets_a_link() {
        assert_eq!(
            view_href("urn:iki:gonk:view:open").as_deref(),
            Some("/view/open")
        );
        assert_eq!(view_href("urn:iki:gonk:view:rule:x"), None);
        assert_eq!(view_href("urn:iki:gonk:view:"), None);
    }

    #[test]
    fn ordinals_read_as_words_do() {
        let words: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 101, 111]
            .map(ordinal)
            .into();
        assert_eq!(
            words,
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "101st",
                "111th"
            ]
        );
    }
}
