//! One name's resolution (ledger #3238): `urn:iki:gonk:resolution:iri:{iri}`, served at
//! `GET /resolution/iri/{iri}`.
//!
//! Stored text keeps the NAME, never a URL. [`crate::render::linkify`] links every full IRI it
//! recognizes to this route with no lookup, and the route decides where the name goes when
//! someone follows it, under the CALLER's capability. When resolution improves (a mounted
//! ikigai-name), every old link improves with no rewrite.
//!
//! [`resolve`] is the rule table and [`answer`] is the page, kept apart so that the day
//! ikigai-web lets a read redirect, `answer` becomes a `303` and nothing else changes. Until
//! then a resolved name answers `200` with an instant client-side refresh (WCAG H76) and a
//! visible link. The target is only ever built here: a same-origin gonk path or a configured
//! template with a fixed host, never a URL taken from the name, so the route is no open
//! redirect. An unknown name and an unreadable one get the same `404`, word for word.
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Description, Endpoint, EndpointSpace, Error, Invocation, Representation, Result,
    UriTemplate, Verb,
};
use ikigai_fn::html::url_segment;
use ikigai_ledger::Ledger;

use crate::layers_view::{LayersView, Views};
use crate::render::{self, element, envelope};
use crate::web::{self, Web};

/// The route's IRI, as the door's route table expands it (`{+iri}`: a raw capture).
pub const TEMPLATE: &str = "urn:iki:gonk:resolution:iri:{+iri}";
/// The HTTP path of one name's resolution.
pub const PATH_TEMPLATE: &str = "/resolution/iri/{+iri}";
const PREFIX: &str = "urn:iki:gonk:resolution:iri:";
const BINDING: &str = "urn:iki:gonk:resolution:iri:{iri}";

/// The link a page draws for `iri`: the route, the IRI as ONE percent-encoded segment.
///
/// ```
/// use ikigai_gonk::resolution::href;
/// assert_eq!(href("urn:repo:file:demo/src/lib.rs"), "/resolution/iri/urn%3Arepo%3Afile%3Ademo%2Fsrc%2Flib.rs");
/// assert_eq!(href("gh:o/r#7"), "/resolution/iri/gh%3Ao%2Fr%237");
/// ```
pub fn href(iri: &str) -> String {
    let mut out = String::from("/resolution/iri/");
    for b in iri.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Bind the route into the HTTP face's page space.
pub fn bind(space: EndpointSpace, web: &Arc<Web>, layers: Views) -> EndpointSpace {
    space.bind(
        UriTemplate::parse(BINDING).expect("a constant template"),
        Resolve {
            web: Arc::clone(web),
            layers,
        },
    )
}

/// The rule table: where `iri` goes for this caller, or `None` when no rule here knows it or it
/// is not readable to the caller (the two are never told apart).
pub async fn resolve(
    layers: Option<&Arc<LayersView>>,
    inv: &Invocation<'_>,
    iri: &str,
) -> Option<String> {
    if let Some((kind, sha)) = crate::git_object::git_iri(iri) {
        return web::git_href(inv, kind, &sha).await;
    }
    if let Some((root, path)) = web::repo_file(iri) {
        if let Some(href) = web::repo_file_href(iri, inv.capability) {
            return Some(href);
        }
        let read = web::request(
            Verb::Source,
            &crate::git_object::file_iri("configured", root),
        )
        .ok()?
        .with_arg("path", ArgRef::Inline(path.as_bytes().to_vec()));
        let bytes = web::fetch(inv, read).await.ok()?;
        let text = std::str::from_utf8(&bytes).ok()?;
        return ikigai_fn::html::safe_href(text)
            .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
            .map(str::to_string);
    }
    if let Some((ledger, id)) = ledger_item(iri) {
        let ledger = Ledger::parse(ledger).ok()?;
        web::require_read(inv.capability, &ledger).ok()?;
        return Some(format!("/l/{}/item/{id}", url_segment(ledger.name())));
    }
    if let Some((layer, id)) = layer_entry(iri) {
        return crate::layers_view::may_read_layer(layers, inv, layer)
            .await
            .then(|| format!("/layers/{}#entry-{}", url_segment(layer), url_segment(id)));
    }
    github_alias(iri)
}

/// `urn:iki:ledger:{ledger}:item:{id}`, the id a run of ASCII letters and digits.
fn ledger_item(iri: &str) -> Option<(&str, &str)> {
    let (ledger, id) = iri.strip_prefix("urn:iki:ledger:")?.split_once(":item:")?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())).then_some((ledger, id))
}

/// `urn:layer:{layer}:entry:{id}`, a valid layer name and an id of letters, digits, `-`, `_`.
fn layer_entry(iri: &str) -> Option<(&str, &str)> {
    let (layer, id) = iri.strip_prefix("urn:layer:")?.split_once(":entry:")?;
    (crate::layer::is_layer_name(layer)
        && !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
    .then_some((layer, id))
}

/// `gh:{owner}/{repo}#{n}`: the GitHub issue (GitHub forwards a pull request's number), by the
/// mapping [`render::github_ref_at`] uses, which validates owner and repository as GitHub names.
///
/// ```
/// use ikigai_gonk::resolution::github_alias;
/// assert_eq!(github_alias("gh:ikigai-rs/ikigai-gonk#319").as_deref(), Some("https://github.com/ikigai-rs/ikigai-gonk/issues/319"));
/// assert_eq!(github_alias("gh:o/r#7x"), None);
/// assert_eq!(github_alias("gh:o/..#7"), None);
/// assert_eq!(github_alias("gh:evil.example/r#7"), None);
/// assert_eq!(github_alias("gh:o/r#7/../../x"), None);
/// ```
pub fn github_alias(iri: &str) -> Option<String> {
    let rest = iri.strip_prefix("gh:")?;
    let (len, href) = render::github_ref_at(rest)?;
    (len == rest.len() && href.contains("/issues/")).then_some(href)
}

/// The words of the one 404, for an unknown name and an unreadable one alike.
pub fn not_found(iri: &str) -> String {
    format!("No resolver here knows the name {iri}, or it is not readable to you.")
}

/// The interim answer for a resolved name: `200`, an instant refresh to `target` and a visible
/// link to it, never stored by a shared cache (the target depends on the viewer's grants).
pub fn answer(web: &Web, inv: &Invocation<'_>, iri: &str, target: &str) -> Result<Representation> {
    let ledgers = web::readable_ledgers(web, inv.capability);
    let children = format!(
        "{}{}",
        web::nav(web, inv.capability, &ledgers, None),
        element("resolution", &[("iri", iri), ("target", target)], "")
    );
    let doc = envelope(
        "page",
        &[
            ("view", "resolution"),
            ("full", "true"),
            ("title", "Name resolved"),
            ("refresh", target),
            ("ui-theme", render::theme_of(inv)),
        ],
        &children,
    );
    let page = web::html(render::render(&doc, true).map_err(web::render_err)?);
    ikigai_web::with_headers(page, &[("Cache-Control", "private, no-store")])
        .map_err(|e| Error::Endpoint(format!("the resolution page's headers: {}", e.why)))
}

struct Resolve {
    web: Arc<Web>,
    layers: Views,
}

#[async_trait]
impl Endpoint for Resolve {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        web::html_only(inv)?;
        // The whole name, from the request's own IRI: the binding could stop at a `#`, and a
        // `gh:` alias carries one.
        let target = inv.request.target.as_str();
        let iri = target
            .strip_prefix(PREFIX)
            .map(str::to_string)
            .or_else(|| inv.bindings.get("iri").map(str::to_string))
            .unwrap_or_default();
        match resolve(self.layers.current().as_ref(), inv, &iri).await {
            Some(href) => answer(&self.web, inv, &iri, &href),
            None => Err(Error::NotFound(not_found(&iri))),
        }
    }
    fn name(&self) -> &str {
        "gonk-resolution"
    }
    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("Where one name resolves, for this caller")
            .verb(Verb::Source)
            .input(web::arg("iri", "the full IRI to resolve").binding())
            .input(web::as_html_arg())
            .output("text/html")
    }
}
