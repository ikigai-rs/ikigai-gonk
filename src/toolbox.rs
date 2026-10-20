//! The RDF Toolbox at `/toolbox` (ledger [#1933](http://localhost:1062/l/default/item/1933)):
//! turtle-lsp's in-browser Validate, Format, SHACL and SPARQL page, served as `ikigai-toolbox`'s
//! `urn:toolbox:*` resources over a build directory the operator installs.
//!
//! ```toml
//! gonk.toolbox.dist = "~/.local/share/ikigai-toolbox/dist"
//! ```
//!
//! No line, no toolbox: nothing is bound, no route is added, no grant is minted, and
//! `/toolbox/…` takes the door's mechanical mapping to a name nothing binds, which is the
//! library's `404`. A line naming something that is not a directory stops the server at start,
//! naming the key ([`Toolbox::at`]).
//!
//! ```text
//! /toolbox                 urn:toolbox:page            the empty toolbox
//! /toolbox/{name}          urn:toolbox:{name}          a built file (toolbox.html, app.js, engine.wasm, …)
//! /toolbox/check/{iri}     urn:toolbox:check:{iri}     the toolbox checking one resource (shapes=, theme=)
//! /toolbox/input/{iri}     urn:toolbox:input:{iri}     a resource's Turtle face, for the page to read
//! ```
//!
//! The routes are the mechanical rule written out (path segments joined by `:`), because each
//! needs a row of its own to carry [`ikigai_toolbox::TOOLBOX_CSP`]: the engine is WebAssembly
//! compiled in a worker, which the door's default policy refuses. `TOOLBOX_CSP` is on these rows
//! and no others.
//!
//! # ★ Bound in the HTTP door only, and read under the caller's capability
//!
//! The toolbox is an HTML face, so it is bound beside gonk's pages in the HTTP door's kernel
//! ([`crate::doors::http_kernel_serving`]), never in the hub: the socket and QUIC doors serve
//! exactly the catalog they served before. Its build directory is bound at
//! [`ikigai_toolbox::DIST`] with ikigai-fs's `FileEndpoint`, a jail at that directory, and every
//! read the family makes, the files and every `input:`, runs under the CALLER's capability. So
//! the door's anonymous caller holds a read on exactly the toolbox's files
//! ([`crate::grants::toolbox_read_grants`]), and an `input:` shows a viewer only what that viewer
//! could read anyway.
//!
//! ⚠ The bound directory is also reachable by its own name (`/iki/toolbox/dist/app.js`, the
//! mechanical mapping of `urn:iki:toolbox:dist:app.js`). That reaches nothing more: the grant
//! names the toolbox's files one by one, so any other file the directory holds is refused, and
//! nobody holds an `fs` write or delete token here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ikigai_core::{EndpointSpace, Space, UriTemplate};
use ikigai_web::{EdgeConfig, Route};

/// The config key naming the build directory.
pub const KEY: &str = "gonk.toolbox.dist";

/// Where the toolbox is served.
pub const PATH: &str = "/toolbox";

/// The empty toolbox, what [`PATH`] itself answers.
pub const PAGE_IRI: &str = "urn:toolbox:page";

/// A configured toolbox: the build directory, checked to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolbox {
    dist: PathBuf,
}

impl Toolbox {
    /// The toolbox over the build directory `dist` (already `~/`-expanded).
    ///
    /// # Errors
    ///
    /// When `dist` is not a directory, or is not UTF-8 (a capability scope is a string, and a
    /// lossy one would name another path).
    pub fn at(dist: PathBuf) -> Result<Toolbox, String> {
        if !dist.is_dir() {
            return Err(format!(
                "{KEY}: {} is not a directory. Build it with ikigai-toolbox's build.sh and copy \
                 dist/ there, or remove the line",
                dist.display()
            ));
        }
        if dist.to_str().is_none() {
            return Err(format!(
                "{KEY}: {} is not UTF-8, so no capability can name it",
                dist.display()
            ));
        }
        Ok(Toolbox { dist })
    }

    /// The build directory.
    pub fn dist(&self) -> &Path {
        &self.dist
    }

    /// The spaces the HTTP door binds: the toolbox family, then its build directory at
    /// [`ikigai_toolbox::DIST`].
    ///
    /// The directory's space is left anonymous: its doors read THIS directory, so only this
    /// process knows which instance it is (ledger #987's rule, as `ikigai_toolbox::space_at`
    /// states it). The family names itself `urn:iki:space:toolbox`.
    pub fn spaces(&self) -> Vec<Arc<dyn Space>> {
        let pattern = format!("{}{{path}}", ikigai_toolbox::DIST);
        let dist = EndpointSpace::new().bind(
            UriTemplate::parse(&pattern).expect("ikigai_toolbox::DIST makes a template"),
            ikigai_fs::FileEndpoint::new(self.dist.clone()),
        );
        vec![Arc::new(ikigai_toolbox::space()), Arc::new(dist)]
    }

    /// The read the HTTP door's caller needs: [`crate::grants::toolbox_read_grants`].
    pub fn grants(&self) -> Vec<String> {
        crate::grants::toolbox_read_grants(&self.dist)
    }
}

/// The `/toolbox` rows, each carrying [`ikigai_toolbox::TOOLBOX_CSP`].
///
/// Every capture is the RAW spelling, `{+pN}`, because an `{iri}` carries `:` (a strict
/// `{var}` refuses it with a `400`, ledger #740), and what it expands to stays under
/// `urn:toolbox:`, which resolves only the family's own names. A deeper path than two segments
/// takes the mechanical mapping and the door's default policy, which is stricter, never looser.
///
/// ```
/// let rows = ikigai_gonk::toolbox::routes();
/// let pairs: Vec<(&str, &str)> =
///     rows.iter().map(|r| (r.pattern.as_str(), r.iri_template.as_str())).collect();
/// assert_eq!(
///     pairs,
///     [
///         ("/toolbox", "urn:toolbox:page"),
///         ("/toolbox/{+p1}", "urn:toolbox:{+p1}"),
///         ("/toolbox/{+p1}/{+p2}", "urn:toolbox:{+p1}:{+p2}"),
///     ]
/// );
/// assert!(rows.iter().all(|r| r.csp.as_deref() == Some(ikigai_toolbox::TOOLBOX_CSP)));
/// ```
pub fn routes() -> Vec<Route> {
    let route = |pattern: String, iri_template: &str| Route {
        pattern,
        iri_template: iri_template.to_string(),
        cap: None,
        cors: None,
        csp: Some(ikigai_toolbox::TOOLBOX_CSP.to_string()),
    };
    vec![
        route(PATH.to_string(), PAGE_IRI),
        route(format!("{PATH}/{{+p1}}"), "urn:toolbox:{+p1}"),
        route(format!("{PATH}/{{+p1}}/{{+p2}}"), "urn:toolbox:{+p1}:{+p2}"),
    ]
}

/// `config` with the [`routes`] appended, for a door that serves the toolbox.
///
/// Appended after every row of gonk's own: no page route starts with [`PATH`], so the order
/// decides nothing, and gonk's table reads the same as it did with the toolbox off.
pub fn route(mut config: EdgeConfig) -> EdgeConfig {
    config.routes.routes.extend(routes());
    config
}
