//! Where a name in item text goes when someone follows it (ledger
//! [#3238](http://localhost:1062/l/default/item/3238)).
//!
//! ```text
//! /resolution/iri/{+iri}   urn:iki:gonk:resolution:iri:{iri}   Source  200 with an instant refresh, or the one 404
//! ```
//!
//! Stored text keeps the NAME, never a URL. [`crate::render::linkify`] links every full IRI of
//! the families below (and any other `urn:` or `tag:` IRI) to this one route, with no lookup at
//! render time, so a page never says whether a name exists. The route decides where the name
//! goes when it is followed, under the CALLER's capability. When resolution improves (a
//! mounted ikigai-name, urnid.org), every old link improves with no rewrite.
//!
//! | name | rule ([`Resolver::resolve`]) |
//! |---|---|
//! | `urn:git:commit:{sha}`, `urn:git:blob:{sha}` | the `git-link` map ([`crate::git_object`]), configured clones then browse roots, under the caller's grants |
//! | `urn:repo:file:{root}/{path}` | browse's page for the file when the caller holds `urn:cap:browse:read:{root}` and `root` is a browse root; else the root's `gonk.repository.file` template; else unknown |
//! | `urn:iki:ledger:{ledger}:item:{id}` | the item page, when the caller may read that ledger |
//! | `urn:layer:{layer}:entry:{id}` | `/layers/{layer}#entry-{id}`, when the caller may read that layer on the Layers page |
//! | `gh:{owner}/{repo}#{n}` | `https://github.com/{owner}/{repo}/issues/{n}`, [`crate::render::github_ref_at`]'s mapping |
//! | anything else | unknown, for now |
//!
//! # ★ The answer is `200`, for now ([`answer`])
//!
//! A name that resolves answers `200` with a small page in gonk's shell: a
//! `<meta http-equiv="refresh" content="0; url=…">` in its head (WCAG technique H76, an instant
//! client-side redirect) and a sentence with a visible link to the same target, for a browser
//! that does not follow the refresh. `303 See Other` is what the route means, but ikigai-web
//! 0.4.0 refuses an endpoint's `Location` on a read. When it allows one, [`answer`] is the one
//! function that changes: the rule table above does not move and nothing stored changes.
//!
//! # ★ A target is only ever built here
//!
//! Every rule builds its target from a validated piece of the name: a same-origin gonk path, a
//! configured template whose host is fixed at start, or GitHub's fixed origin. A URL is never
//! taken from the name, so this route is not an open redirect: an `http(s):` IRI in text is
//! linked directly by `linkify` and is unknown here. `built_here` holds every target to that.
//!
//! # ★ One 404 for unknown and unreadable
//!
//! A name nobody here resolves and a name the caller may not read get the SAME answer,
//! [`unknown`]: a `404` whose sentence shows the name and says both. Never a different status or
//! wording, so the route cannot be asked whether a name the caller cannot read exists.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, EndpointSpace, Error, Invocation, Representation, Result,
    UriTemplate, Verb,
};
use ikigai_fn::html::{attr, html_text, safe_href, url_segment};
use ikigai_ledger::Ledger;

use crate::layers_view::LayersView;
use crate::render::{self, envelope};
use crate::web::{self, Web};

/// The resource: one IRI's resolution, the IRI the template's tail.
pub const TEMPLATE: &str = "urn:iki:gonk:resolution:iri:{iri}";

/// The route's path, before the IRI.
pub const PATH: &str = "/resolution/iri/";

/// The route pattern and its IRI template ([`crate::doors`]). `{+iri}` is RAW: it carries the
/// name's colons and slashes, and [`href`] encodes the whole name as one segment.
pub const ROUTE: (&str, &str) = (
    "/resolution/iri/{+iri}",
    "urn:iki:gonk:resolution:iri:{+iri}",
);

/// The longest name [`crate::render::linkify`] links. Longer text stays text.
pub const MAX_NAME: usize = 1024;

/// The link to the route for `iri`: the whole name as ONE encoded path segment, so its `/`,
/// `#` and `?` stay part of it.
///
/// ```
/// use ikigai_gonk::resolution::href;
/// assert_eq!(href("urn:repo:file:core/src/lib.rs"), "/resolution/iri/urn%3Arepo%3Afile%3Acore%2Fsrc%2Flib.rs");
/// assert_eq!(href("gh:o/r#7"), "/resolution/iri/gh%3Ao%2Fr%237");
/// ```
pub fn href(iri: &str) -> String {
    format!("{PATH}{}", url_segment(iri))
}

/// Whether `iri` is a name this route can be asked about: the resource IRI it makes parses, so
/// the route reaches its resource for it rather than the door refusing the path.
pub fn routable(iri: &str) -> bool {
    iri.len() <= MAX_NAME
        && !iri.chars().any(|c| c.is_whitespace() || c.is_control())
        && ikigai_core::Iri::parse(TEMPLATE.replace("{iri}", iri)).is_ok()
}

/// The one answer for a name that does not resolve for this caller: unknown, or not readable.
pub fn unknown(iri: &str) -> Error {
    Error::NotFound(format!(
        "No resolver here knows the name {iri}, or it is not readable to you."
    ))
}

/// Whether `target` is one this module builds: a same-origin path, or an http(s) URL (only a
/// configured template or GitHub's fixed origin makes one).
fn built_here(target: &str) -> bool {
    let Some(safe) = safe_href(target) else {
        return false;
    };
    (safe.starts_with('/') && !safe.starts_with("//"))
        || safe.starts_with("https://")
        || safe.starts_with("http://")
}

/// An id a ledger item or layer entry can have: letters, digits, `-`, `_` and `.`, not empty.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && id != "."
        && id != ".."
}

/// The `{layer}` and `{id}` of `urn:layer:{layer}:entry:{id}`.
fn layer_entry(iri: &str) -> Option<(&str, &str)> {
    let (layer, id) = iri.strip_prefix("urn:layer:")?.split_once(":entry:")?;
    (crate::layer::is_layer_name(layer) && plain_id(id)).then_some((layer, id))
}

/// The GitHub issue page `gh:{owner}/{repo}#{n}` names, by [`render::github_ref_at`]'s rules.
fn github_alias(iri: &str) -> Option<String> {
    let rest = iri.strip_prefix("gh:")?;
    let (len, href) = render::github_ref_at(rest)?;
    (len == rest.len() && rest.contains('#')).then_some(href)
}

/// What the rule table needs besides the invocation.
pub struct Resolver {
    web: Arc<Web>,
    layers: Option<Arc<LayersView>>,
}

impl Resolver {
    /// Where `iri` goes for the caller of `inv`, or `None`: the module docs' table.
    pub async fn resolve(&self, inv: &Invocation<'_>, iri: &str) -> Option<String> {
        let target = if let Some((kind, sha)) = crate::git_object::git_iri(iri) {
            web::git_href(inv, kind, &sha).await
        } else if let Some((root, path)) = web::repo_file(iri) {
            match web::repo_file_href(iri, inv.capability)
                .filter(|_| self.web.browse_roots.contains(root))
            {
                Some(href) => Some(href),
                None => file_template(inv, root, path).await,
            }
        } else if let Some((ledger, id)) = ledger_item(iri) {
            web::readable_ledgers(&self.web, inv.capability)
                .iter()
                .any(|l| l.name() == ledger.name())
                .then(|| format!("/l/{}/item/{}", url_segment(ledger.name()), url_segment(id)))
        } else if let Some((layer, id)) = layer_entry(iri) {
            match &self.layers {
                Some(view)
                    if crate::layers_view::reads_layer(view, inv.capability, layer).await =>
                {
                    Some(format!(
                        "{}/{}#entry-{}",
                        crate::layers_view::PAGE_PATH,
                        url_segment(layer),
                        url_segment(id)
                    ))
                }
                _ => None,
            }
        } else {
            github_alias(iri)
        };
        target.filter(|t| built_here(t))
    }
}

/// The resolution rules' view of a ledger item IRI: its ledger and its plain id.
fn ledger_item(iri: &str) -> Option<(Ledger, &str)> {
    let (ledger, _) = Ledger::item_iri(iri)?;
    let id = iri.rsplit_once(":item:")?.1;
    plain_id(id).then_some((ledger, id))
}

/// The root's `gonk.repository.file` template, expanded, through the configured map.
async fn file_template(inv: &Invocation<'_>, root: &str, path: &str) -> Option<String> {
    let read = web::with(
        web::request(
            Verb::Source,
            &crate::git_object::file_iri("configured", root),
        )
        .ok()?,
        "path",
        path,
    );
    let bytes = web::fetch(inv, read).await.ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    safe_href(text)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The page a resolved name answers: `200`, gonk's shell, an instant refresh to `target` in the
/// head and a visible link to it. The ONE function that becomes a `303` when ikigai-web allows
/// a read to redirect (see the module docs).
///
/// `target` is in an attribute and in text, escaped by `ikigai_fn::html`'s [`attr`] and
/// [`html_text`] and spliced into the stylesheet's slots ([`render::splice`]); the name is
/// text the stylesheet writes. `Cache-Control: private`, because the target depends on the
/// viewer's grants.
pub fn answer(web: &Web, inv: &Invocation<'_>, iri: &str, target: &str) -> Result<Representation> {
    let mut children = web::nav_for(web, inv.capability);
    children.push_str(&render::slot("refresh"));
    children.push_str(&render::wrap(
        "resolution",
        &[("name", iri)],
        &render::slot("resolved"),
    ));
    let document = envelope(
        "page",
        &[
            ("view", "resolution"),
            ("full", "true"),
            ("ui-theme", render::theme_of(inv)),
            ("title", "Resolving a name"),
        ],
        &children,
    );
    let shell = render::render(&document, true).map_err(web::render_err)?;
    let refresh = format!(
        "<meta http-equiv='refresh' content='0; url={}'>",
        attr(target)
    );
    let resolved = format!(
        "<p>This name resolves to <a href='{}'>{}</a>.</p>",
        attr(target),
        html_text(target)
    );
    let page = render::splice(
        shell,
        &[
            ("refresh".to_string(), refresh),
            ("resolved".to_string(), resolved),
        ],
    )
    .map_err(web::render_err)?;
    ikigai_web::with_headers(web::html(page), &[("Cache-Control", "private, no-cache")])
        .map_err(|e| Error::Endpoint(e.to_string()))
}

/// `urn:iki:gonk:resolution:iri:{iri}`: [`Resolver::resolve`], then [`answer`] or [`unknown`].
struct Resolution {
    resolver: Resolver,
}

#[async_trait]
impl Endpoint for Resolution {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        web::html_only(inv)?;
        let iri = web::binding(inv, "iri")?;
        match self.resolver.resolve(inv, &iri).await {
            Some(target) => answer(&self.resolver.web, inv, &iri, &target),
            None => Err(unknown(&iri)),
        }
    }

    fn name(&self) -> &str {
        "gonk-resolution"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("Where a name goes when it is followed")
            .summary(
                "One IRI's resolution under the caller's capability: a page that sends the \
                 browser on to its target at once, or the same 404 for a name nobody here \
                 resolves and one the caller may not read.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("iri")
                    .binding()
                    .summary("the name, as written")
                    .class("http://www.w3.org/2001/XMLSchema#string"),
            )
            .input(web::as_html_arg())
            .output(ikigai_web::HTTP_ENTITY)
    }
}

/// Bind the route's resource into the HTTP face's page space.
pub fn bind(
    space: EndpointSpace,
    web: &Arc<Web>,
    layers: Option<Arc<LayersView>>,
) -> EndpointSpace {
    space.bind(
        UriTemplate::parse(TEMPLATE).expect("a constant template"),
        Resolution {
            resolver: Resolver {
                web: Arc::clone(web),
                layers,
            },
        },
    )
}
