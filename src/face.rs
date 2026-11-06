//! A deployment's own pages, served from gonk's origin (ledger
//! [#3199](http://localhost:1062/l/default/item/3199), part of #3189): a **face** is a directory
//! of HTML, scripts and styles the deployment writes, and gonk serves it read-only, jailed to
//! that directory, under the door's default policy.
//!
//! ```toml
//! gonk.face = "ops ~/.local/share/ops-face"
//! gonk.face = "intake /srv/intake-face"
//! ```
//!
//! One line per face, `<name> <directory>`, repeatable. No line, no face: nothing is bound, no
//! route is added, no grant is minted. A line naming something that is not a directory stops the
//! server at start, naming the key ([`Face::at`]), as does a name given twice.
//!
//! ```text
//! /face/name/{name}/                urn:face:name:{name}:index.html   the face's front page
//! /face/name/{name}/{a}/…/{file}    urn:face:name:{name}:{a}:…:{file} one file, {a}/…/{file} on disk
//! ```
//!
//! The second row is the door's mechanical mapping (path segments joined by `:`), so a face
//! may have subdirectories; [`FaceFile`] turns the `:`s back into `/`s. That is unambiguous
//! because ikigai-fs refuses a `:` in a file name.
//!
//! # ★ Why it is served from gonk's origin, and what it can reach
//!
//! A face's scripts read and write through gonk's existing routes (`/l/…`, `/sparql`, `/k`, …)
//! with the viewer's own passkey session, which a page on another origin cannot do (ledger
//! #880 refuses a read another origin's page loads). Everything they do runs under the
//! VIEWER's capability, exactly as if the viewer had typed the request: a face reaches nothing
//! its viewer could not reach by hand. There is no collaborator Rust in gonk and no plugin API:
//! a face is files.
//!
//! # ★ Jailed, read-only, and refused anything else
//!
//! Each directory is bound at `urn:iki:face:name:{name}:dir:{path}` with ikigai-fs's
//! `FileEndpoint`, a jail at that directory: it refuses `..`, an absolute path, any
//! non-canonical spelling and a symbolic link anywhere in the path, in the jail or out of it.
//! The door's anonymous loopback caller holds `urn:cap:fs:read:{directory}` and nothing else
//! there ([`Face::grants`]): nobody holds an `fs` write or delete token, so a face is read-only
//! at every name. On top of that, [`FaceFile`] serves only names whose extension is in
//! [`MEDIA_TYPES`] (which fixes the media type) and refuses every hidden name (a segment that
//! starts with `.`, such as `.git` or `.env`), each as `404` with no sub-request.
//!
//! The rows carry no [`Route::csp`]: a face gets the door's default policy
//! (`script-src 'self'`, no inline script or style, no framing), which is what lets its scripts
//! run at all and nothing more.
//!
//! Like the toolbox ([`crate::toolbox`]), a face is bound in the HTTP door's kernel only, never
//! in the hub: the socket and QUIC doors serve exactly what they served before.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Body, Description, Endpoint, EndpointSpace, Error, Invocation, Iri, ReprType,
    Representation, Request, Space, UriTemplate, Verb,
};
use ikigai_web::{EdgeConfig, Route};

/// The config key: one `<name> <directory>` line per face.
pub const KEY: &str = "gonk.face";

/// Where faces are served: `/face/name/{name}/…`.
pub const PATH: &str = "/face/name";

/// The IRI prefix a face's files answer under: `urn:face:name:{name}:{path}`.
pub const IRI: &str = "urn:face:name:";

/// The file `/face/name/{name}/` answers.
pub const INDEX: &str = "index.html";

/// The extensions a face may serve, and the media type each one is served with. Anything else
/// is `404`: a fixed allowlist, so a face never serves a file under a type nobody chose.
pub const MEDIA_TYPES: [(&str, &str); 14] = [
    ("html", "text/html"),
    ("css", "text/css"),
    ("js", "text/javascript"),
    ("mjs", "text/javascript"),
    ("json", "application/json"),
    ("jsonld", "application/ld+json"),
    ("ttl", "text/turtle"),
    ("txt", "text/plain"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("webp", "image/webp"),
    ("ico", "image/x-icon"),
    ("woff2", "font/woff2"),
];

/// The media type a face serves `path` with, or `None` when its extension is not in
/// [`MEDIA_TYPES`].
///
/// ```
/// use ikigai_gonk::face::media_type;
/// assert_eq!(media_type("index.html"), Some("text/html"));
/// assert_eq!(media_type("js/app.js"), Some("text/javascript"));
/// assert_eq!(media_type("secret.key"), None);
/// assert_eq!(media_type("Makefile"), None);
/// ```
pub fn media_type(path: &str) -> Option<&'static str> {
    let leaf = path.rsplit('/').next()?;
    let (_, extension) = leaf.rsplit_once('.')?;
    MEDIA_TYPES
        .iter()
        .find(|(known, _)| *known == extension)
        .map(|(_, media)| *media)
}

/// A configured face: its name and its directory, checked to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Face {
    name: String,
    dir: PathBuf,
}

impl Face {
    /// The face `name` over the directory `dir` (already `~/`-expanded).
    ///
    /// # Errors
    ///
    /// When `name` is not a name (lowercase ASCII letters, digits and `-`, starting with a letter
    /// or digit), when `dir` is not a directory, or when it is not UTF-8 (a capability scope is
    /// a string, and a lossy one would name another path).
    pub fn at(name: &str, dir: PathBuf) -> Result<Face, String> {
        let well_formed = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !well_formed {
            return Err(format!(
                "{KEY}: `{name}` is not a face name: lowercase letters, digits and `-`, \
                 starting with a letter or digit"
            ));
        }
        if !dir.is_dir() {
            return Err(format!(
                "{KEY}: face `{name}`: {} is not a directory. Create it, or remove the line",
                dir.display()
            ));
        }
        if dir.to_str().is_none() {
            return Err(format!(
                "{KEY}: face `{name}`: {} is not UTF-8, so no capability can name it",
                dir.display()
            ));
        }
        Ok(Face {
            name: name.to_string(),
            dir,
        })
    }

    /// The face's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The face's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the face's front page is served: `/face/name/{name}/`.
    pub fn href(&self) -> String {
        format!("{PATH}/{}/", self.name)
    }

    /// The IRI the face's directory is bound at: `urn:iki:face:name:{name}:dir:`.
    fn dir_base(&self) -> String {
        format!("urn:iki:face:name:{}:dir:", self.name)
    }

    /// The spaces the HTTP door binds for this face: [`FaceFile`] at
    /// `urn:face:name:{name}:{path}`, and the directory at `urn:iki:face:name:{name}:dir:{path}`.
    pub fn spaces(&self) -> Vec<Arc<dyn Space>> {
        let files = EndpointSpace::new().bind(
            UriTemplate::parse(format!("{IRI}{}:{{path}}", self.name))
                .expect("a checked face name makes a template"),
            FaceFile {
                base: self.dir_base(),
            },
        );
        let dir = EndpointSpace::new().bind(
            UriTemplate::parse(format!("{}{{path}}", self.dir_base()))
                .expect("a checked face name makes a template"),
            ikigai_fs::FileEndpoint::new(self.dir.clone()),
        );
        vec![Arc::new(files), Arc::new(dir)]
    }

    /// The read the HTTP door's anonymous caller needs: `urn:cap:fs:read:{directory}`, and no
    /// write or delete anywhere.
    ///
    /// ```
    /// use std::path::PathBuf;
    /// let dir = std::env::temp_dir();
    /// let face = ikigai_gonk::face::Face::at("ops", dir.clone()).unwrap();
    /// assert_eq!(face.grants(), [format!("urn:cap:fs:read:{}", dir.to_str().unwrap())]);
    /// ```
    pub fn grants(&self) -> Vec<String> {
        vec![format!(
            "urn:cap:fs:read:{}",
            self.dir.to_str().expect("Face::at checked UTF-8")
        )]
    }
}

/// Every `gonk.face` line, parsed: `<name> <directory>`, the directory `~/`-expanded against
/// `home`.
///
/// # Errors
///
/// A line that is not two words, a name or directory [`Face::at`] refuses, or a name given
/// twice.
pub fn parse_all(lines: &[String], home: &Path) -> Result<Vec<Face>, String> {
    let mut faces: Vec<Face> = Vec::new();
    for line in lines {
        let words: Vec<&str> = line.split_whitespace().collect();
        let [name, dir] = words.as_slice() else {
            return Err(format!(
                "{KEY} = \"{line}\" is not `<name> <directory>` (for example \
                 `{KEY} = \"ops ~/ops-face\"`)"
            ));
        };
        let face = Face::at(name, crate::config::expand_home(dir, home))?;
        if faces.iter().any(|seen| seen.name == face.name) {
            return Err(format!(
                "{KEY}: face `{name}` is given twice: one line per face"
            ));
        }
        faces.push(face);
    }
    Ok(faces)
}

/// Every face's spaces, for the HTTP door ([`Face::spaces`]).
pub fn spaces(faces: &[Face]) -> Vec<Arc<dyn Space>> {
    faces.iter().flat_map(Face::spaces).collect()
}

/// `urn:face:name:{name}:{path}`: Source the file at `{path}` (its `:`s read as `/`) from the
/// face's directory under the CALLER's capability, and serve it with the type
/// [`MEDIA_TYPES`] fixes. A hidden name or an extension outside the allowlist is `NotFound`
/// with no sub-request; every other failure (the jail's refusals, a read the caller is not
/// granted) passes through as it came.
pub struct FaceFile {
    base: String,
}

impl FaceFile {
    /// The media type and the request for the file, or `NotFound`.
    fn plan(&self, inv: &Invocation<'_>) -> Result<(&'static str, Request), Error> {
        let asked = inv.request.target.as_str();
        let path = inv
            .bindings
            .get("path")
            .unwrap_or_default()
            .replace(':', "/");
        if path.split('/').any(|segment| segment.starts_with('.')) {
            return Err(Error::NotFound(format!(
                "{asked}: a face serves no hidden name"
            )));
        }
        let Some(media) = media_type(&path) else {
            return Err(Error::NotFound(format!(
                "{asked}: a face serves only {}",
                MEDIA_TYPES
                    .iter()
                    .map(|(extension, _)| format!(".{extension}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };
        let base = &self.base;
        let source = Iri::parse(format!("{base}{path}"))
            .map_err(|e| Error::Endpoint(format!("{asked}: no IRI for the file: {e}")))?;
        let request = Request::new(Verb::Source, source)
            .with_arg("as", ArgRef::Inline(b"application/octet-stream".to_vec()));
        Ok((media, request))
    }
}

/// The directory's `NotFound` as the face file's: it names the IRI asked, never the host path.
fn as_asked(inv: &Invocation<'_>, error: Error) -> Error {
    match error {
        Error::NotFound(why) => Error::NotFound(format!("{}: {why}", inv.request.target.as_str())),
        other => other,
    }
}

#[async_trait]
impl Endpoint for FaceFile {
    async fn invoke(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        let (media, request) = self.plan(inv)?;
        let rep = inv.issue(request).await.map_err(|e| as_asked(inv, e))?;
        Ok(Representation::new(ReprType::new(media), rep.bytes))
    }

    async fn invoke_body(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation<Body>> {
        let (media, request) = self.plan(inv)?;
        let rep = inv
            .issue_body(request)
            .await
            .map_err(|e| as_asked(inv, e))?;
        Ok(Representation::from_body(ReprType::new(media), rep.bytes))
    }

    fn name(&self) -> &str {
        "gonk-face-file"
    }

    fn describe(&self) -> Description {
        let mut description = Description::new(self.name())
            .title("A face file")
            .summary(
                "One file of a deployment's own pages (a `gonk.face` directory), read-only and \
                 jailed to that directory, read under the caller's capability and served with \
                 the media type its extension fixes.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("path")
                    .summary("the file's path in the face's directory, `:` for `/`")
                    .class("http://www.w3.org/2001/XMLSchema#string")
                    .binding(),
            );
        for (_, media) in MEDIA_TYPES {
            description = description.output(media);
        }
        description
    }
}

/// The index row: a face's front page, [`INDEX`]. The door matches it with or without
/// the trailing slash; [`route`] redirects configured bare names before dispatch. Its files
/// take the door's mechanical mapping. No row carries a CSP of its own, so every face is
/// under the door's default policy.
///
/// ```
/// let rows = ikigai_gonk::face::routes();
/// assert_eq!(rows.len(), 1);
/// assert_eq!(rows[0].pattern, "/face/name/{name}");
/// assert_eq!(rows[0].iri_template, "urn:face:name:{name}:index.html");
/// assert!(rows[0].csp.is_none());
/// ```
pub fn routes() -> Vec<Route> {
    vec![Route {
        pattern: format!("{PATH}/{{name}}"),
        iri_template: format!("{IRI}{{name}}:{INDEX}"),
        redirect: None,
        cap: None,
        cors: None,
        csp: None,
    }]
}

/// `config` with the [`routes`] appended and GET/HEAD redirects from each configured
/// face's bare URL to its directory URL, keeping the query. Unknown names still route
/// to an unbound face and answer 404.
pub fn route(mut config: EdgeConfig, faces: &[Face]) -> EdgeConfig {
    config.routes.routes.extend(routes());
    config
        .directory_redirects
        .extend(faces.iter().map(|face| format!("{PATH}/{}", face.name())));
    config
}
