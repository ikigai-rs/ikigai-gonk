//! "On GitHub": a browse root's repository on GitHub, and the page there for one directory or
//! file (ledger [#1907](http://localhost:1062/l/default/item/1907)).
//!
//! ```text
//! urn:iki:gonk:github:{root}                 Source  the root's GitHub repository and the ref a link names
//! urn:iki:gonk:github:{root}:tree[:{path}]   Source  a directory's page on GitHub (`tree/{ref}/{path}`)
//! urn:iki:gonk:github:{root}:file:{path}     Source  a file's page on GitHub (`blob/{ref}/{path}`)
//! ```
//!
//! # The resource map
//!
//! | set | name | kind | how |
//! |---|---|---|---|
//! | a root's HEAD | `urn:repo:{root}:state` | composed, not built | browse's own probe, cached by gonk under the root's wide thread ([`crate::browse::cached_reads`]) |
//! | a root's repository and ref | `urn:iki:gonk:github:{root}` | **the one computation** | the HEAD from `state`, then four local `git` reads: `remote.origin.url`, `--show-prefix`, whether a `refs/remotes/origin/*` ref contains the HEAD, and `origin/HEAD`; or the operator's `gonk.browse.github` override for the repository |
//! | one view's page on GitHub | `urn:iki:gonk:github:{root}:{tree,file}…` | composition | the root's answer above, the path decoded from browse's own spelling, each segment through [`ikigai_fn::html::url_segment`], the whole through [`safe_href`] |
//!
//! **No atom of gonk's own.** The state is the repository's: `.git/config` holds the origin and
//! `.git/refs` the commits origin is known to have, and `git` writes both, never the kernel. The
//! root's answer is cacheable because it READS `state` first: the kernel hangs it from what that
//! read depended on, the root's wide thread, which the root watch cuts on every change to
//! `.git/HEAD`, `config`, `packed-refs` and `refs/…` ([`crate::watch`]). So a commit, a push
//! (which moves `refs/remotes/origin/…`) and an edited origin each recompute it, and a root the
//! watch could not start is served live, as its `state` is.
//!
//! # The ref, and the note
//!
//! A link names the HEAD commit when a remote-tracking ref of `origin` contains it: a PERMALINK,
//! which shows on GitHub exactly what the checkout holds. Otherwise the local tree is ahead (or
//! its commit is unknown), and the link names origin's default branch (`refs/remotes/origin/HEAD`,
//! else GitHub's own `HEAD`, which GitHub resolves to the default branch) and says so in words,
//! because the page there may differ from the one here. ⚠ "Origin has it" is as of the last
//! fetch or push: this server runs no network `git`, so a commit pushed from another clone is
//! not known until this one fetches.
//!
//! # Where the repository comes from
//!
//! The root's `remote.origin.url`, in any of the forms GitHub hands out ([`repository`]),
//! normalized to `https://github.com/{owner}/{repo}` with any userinfo dropped. A
//! `gonk.browse.github = "<root>=<repository>"` line overrides it, for a root whose origin is
//! somewhere else. Neither: no link. A private repository links the same way; GitHub decides
//! who may see it.
//!
//! # Authority is browse's
//!
//! Naming a root's repository is reading the root, so every name here declares
//! `urn:cap:browse:read:*` and enforces `urn:cap:browse:read:{root}`, as
//! [`crate::history`] does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Result, UriTemplate, Verb,
};
use ikigai_fn::html::{safe_href, url_segment};

use crate::render::{self, element, envelope};

/// The family's prefix.
pub const PREFIX: &str = "urn:iki:gonk:github:";

/// The config key that names a root's repository when its origin does not.
pub const OVERRIDE_KEY: &str = "gonk.browse.github";

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const TEXT: &str = "text/plain";
const HTML: &str = "text/html";

/// `urn:iki:gonk:github:{root}` — the root's repository and the ref a link names.
pub fn root_iri(root: &str) -> String {
    format!("{PREFIX}{root}")
}

/// The GitHub name for a browse view, when the view is a root's tree, a directory or a file:
/// `urn:repo:{root}:tree[:{path}]` and `urn:repo:{root}:file:{path}` map to the same suffix
/// under [`PREFIX`], so the path keeps browse's own spelling. `None` for any other name.
pub fn link_iri(view: &str) -> Option<String> {
    let rest = view.strip_prefix("urn:repo:")?;
    let (root, kind) = rest.split_once(':')?;
    let viewed = kind == "tree" || kind.starts_with("tree:") || kind.starts_with("file:");
    (viewed && !root.is_empty()).then(|| format!("{PREFIX}{rest}"))
}

// ---------------------------------------------------------------- the repository's name

/// `origin` as `https://github.com/{owner}/{repo}`, or `None` when it does not name a
/// repository on GitHub.
///
/// The forms GitHub hands out, and nothing else: `git@github.com:o/r(.git)` (and any user
/// before the `@`), `ssh://[user@]github.com[:port]/o/r(.git)` and
/// `https://[userinfo@]github.com[:port]/o/r(.git)`, with a trailing `/` allowed. The host is
/// matched without regard to case. Userinfo is dropped, never carried: an `https` origin can
/// hold a token, and the answer goes into an `href`. The owner is ASCII letters, digits and
/// `-`; the repository ASCII letters, digits, `-`, `_` and `.`, and never `.` or `..` (the
/// rule `crate::render` holds a GitHub reference to).
///
/// ```
/// use ikigai_gonk::github::repository;
/// let want = Some("https://github.com/ikigai-rs/ikigai-gonk".to_string());
/// assert_eq!(repository("git@github.com:ikigai-rs/ikigai-gonk.git"), want);
/// assert_eq!(repository("ssh://git@github.com/ikigai-rs/ikigai-gonk"), want);
/// assert_eq!(repository("https://github.com/ikigai-rs/ikigai-gonk.git"), want);
/// assert_eq!(repository("https://brian:t0ken@github.com/ikigai-rs/ikigai-gonk"), want);
/// assert_eq!(repository("https://gitlab.com/ikigai-rs/ikigai-gonk"), None);
/// assert_eq!(repository("/srv/git/ikigai-gonk.git"), None);
/// ```
pub fn repository(origin: &str) -> Option<String> {
    let origin = origin.trim();
    let (host, path) = if let Some(rest) = strip_scheme(origin, "https://") {
        authority_and_path(rest)?
    } else if let Some(rest) = strip_scheme(origin, "ssh://") {
        authority_and_path(rest)?
    } else if origin.contains("://") {
        return None;
    } else {
        // The scp-like form `[user@]host:path`. A path with no `:` is a local directory.
        let (authority, path) = origin.split_once(':')?;
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        (host, path)
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    owner_and_repo(path)
}

/// An override's value: any form [`repository`] reads, or the bare `owner/repo`.
///
/// ```
/// use ikigai_gonk::github::override_repository;
/// let want = Some("https://github.com/o/r".to_string());
/// assert_eq!(override_repository("o/r"), want);
/// assert_eq!(override_repository("git@github.com:o/r.git"), want);
/// assert_eq!(override_repository("o/r/s"), None);
/// ```
pub fn override_repository(value: &str) -> Option<String> {
    let value = value.trim();
    repository(value).or_else(|| {
        if value.contains(':') {
            return None;
        }
        owner_and_repo(value)
    })
}

fn strip_scheme<'a>(text: &'a str, scheme: &str) -> Option<&'a str> {
    let head = text.get(..scheme.len())?;
    head.eq_ignore_ascii_case(scheme)
        .then(|| &text[scheme.len()..])
}

/// `[userinfo@]host[:port]/path` → `(host, path)`.
fn authority_and_path(rest: &str) -> Option<(&str, &str)> {
    let (authority, path) = rest.split_once('/')?;
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host_port.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        Some(_) => return None,
        None => host_port,
    };
    Some((host, path))
}

fn owner_and_repo(path: &str) -> Option<String> {
    let path = path.strip_suffix('/').unwrap_or(path);
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    let owner_ok = !owner.is_empty()
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let repo_ok = !repo.is_empty()
        && repo != "."
        && repo != ".."
        && repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    (owner_ok && repo_ok).then(|| {
        format!(
            "https://github.com/{}/{}",
            url_segment(owner),
            url_segment(repo)
        )
    })
}

// ---------------------------------------------------------------- the family

/// The family over `roots`, with each `(root, repository)` override in `overrides` (the
/// repository already normalized by [`override_repository`]).
pub fn space(roots: &[(String, PathBuf)], overrides: &[(String, String)]) -> EndpointSpace {
    let mut space = EndpointSpace::new();
    for (root, dir) in roots {
        let configured = overrides
            .iter()
            .find(|(name, _)| name == root)
            .map(|(_, repository)| repository.clone());
        space = space
            .bind(
                Exact::new(root_iri(root)),
                Repo {
                    root: root.clone(),
                    dir: dir.clone(),
                    configured,
                },
            )
            .bind(
                Exact::new(format!("{PREFIX}{root}:tree")),
                Link {
                    root: root.clone(),
                    kind: Kind::Tree,
                },
            )
            .bind(
                template(&format!("{PREFIX}{root}:tree:{{path}}")),
                Link {
                    root: root.clone(),
                    kind: Kind::Tree,
                },
            )
            .bind(
                template(&format!("{PREFIX}{root}:file:{{path}}")),
                Link {
                    root: root.clone(),
                    kind: Kind::File,
                },
            );
    }
    space.named(crate::spaces::iri(crate::spaces::GITHUB))
}

fn template(text: &str) -> UriTemplate {
    UriTemplate::parse(text).expect("a root name holds no `:`, `/`, `{` or `}`")
}

/// The per-root check behind the declared wildcard — browse's own rule, for browse's root.
fn granted(inv: &Invocation<'_>, root: &str) -> Result<()> {
    let scope = format!("{}{root}", ikigai_browse::CAP_PREFIX);
    if inv.capability.allows(&scope) || inv.capability.allows(ikigai_browse::CAP_WILDCARD) {
        return Ok(());
    }
    Err(Error::Denied(format!(
        "github: capability does not grant `{scope}`"
    )))
}

fn iri(text: &str) -> Result<Iri> {
    Iri::parse(text).map_err(|e| Error::Endpoint(format!("github: `{text}` is not an IRI: {e}")))
}

/// The `key value` lines both text faces are written in, read back; `None` for an answer that
/// is `none {reason}`.
///
/// ```
/// use ikigai_gonk::github::fields;
/// let read = fields("href https://github.com/o/r/tree/main\nref main\n").expect("a link");
/// assert_eq!(read["href"], "https://github.com/o/r/tree/main");
/// assert_eq!(read["ref"], "main");
/// assert!(fields("none the root's origin is not on GitHub\n").is_none());
/// ```
pub fn fields(text: &str) -> Option<BTreeMap<String, String>> {
    if text.starts_with("none") {
        return None;
    }
    Some(
        text.lines()
            .filter(|line| !line.is_empty())
            .map(|line| match line.split_once(' ') {
                Some((key, value)) => (key.to_string(), value.to_string()),
                None => (line.to_string(), String::new()),
            })
            .collect(),
    )
}

fn plain(body: String) -> Representation {
    Representation::new(ReprType::new(TEXT), body).cacheable()
}

fn as_spec() -> ArgSpec {
    ArgSpec::new("as")
        .summary("the representation to answer in")
        .class(XSD_STRING)
        .one_of([TEXT, HTML])
        .default_value(TEXT)
        .optional()
}

/// Which face was asked for. Anything but the two is refused by name.
fn face(inv: &Invocation<'_>) -> Result<&'static str> {
    match inv.request.args.get("as") {
        None => Ok(TEXT),
        Some(_) => {
            let asked = inv.inline_str("as")?;
            match asked.split(';').next().unwrap_or(asked).trim() {
                TEXT => Ok(TEXT),
                HTML => Ok(HTML),
                other => Err(Error::InvalidArgument {
                    name: "as".to_string(),
                    detail: format!("`{other}` is not one of {TEXT}, {HTML}"),
                }),
            }
        }
    }
}

// ---------------------------------------------------------------- the root's answer

/// `urn:iki:gonk:github:{root}`.
struct Repo {
    root: String,
    dir: PathBuf,
    /// The `gonk.browse.github` override, normalized.
    configured: Option<String>,
}

/// What `git` says about a root, read once.
struct Facts {
    origin: Option<String>,
    prefix: String,
    on_origin: bool,
    default_branch: Option<String>,
}

#[async_trait]
impl Endpoint for Repo {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        granted(inv, &self.root)?;
        if face(inv)? != TEXT {
            return Err(Error::InvalidArgument {
                name: "as".to_string(),
                detail: format!("`{}` answers {TEXT} only", root_iri(&self.root)),
            });
        }
        // ★ Read FIRST, for the dependency as much as for the HEAD: this answer hangs from what
        // `state` hung from, the root's wide thread, so a commit, a push or an edited origin
        // recomputes it (the module's note).
        let state = inv
            .source(&iri(&format!("urn:repo:{}:state", self.root))?)
            .await?;
        let state = String::from_utf8_lossy(&state.bytes).trim().to_string();
        let head = state
            .split_whitespace()
            .next()
            .filter(|first| is_full_sha(first))
            .map(str::to_string);
        let git = state != "not a git repository";
        let facts = if git {
            let (dir, head) = (self.dir.clone(), head.clone());
            let job = move || read_facts(&dir, head.as_deref());
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => runtime
                    .spawn_blocking(job)
                    .await
                    .map_err(|e| Error::Endpoint(format!("github: git did not finish: {e}")))?,
                Err(_) => job(),
            }
        } else {
            Facts {
                origin: None,
                prefix: String::new(),
                on_origin: false,
                default_branch: None,
            }
        };
        let found = self
            .configured
            .clone()
            .or_else(|| facts.origin.as_deref().and_then(repository));
        let Some(repository) = found else {
            let reason = match &facts.origin {
                None => format!(
                    "the root has no origin remote and no `{OVERRIDE_KEY}` line names its \
                     repository"
                ),
                Some(_) => format!(
                    "the root's origin is not a repository on GitHub, and no `{OVERRIDE_KEY}` \
                     line names one"
                ),
            };
            return Ok(plain(format!("none {reason}\n")));
        };
        let permalink = facts.on_origin && head.is_some();
        let reference = match (&head, permalink) {
            (Some(sha), true) => sha.clone(),
            _ => facts.default_branch.unwrap_or_else(|| "HEAD".to_string()),
        };
        Ok(plain(format!(
            "repository {repository}\nref {reference}\npermalink {permalink}\nahead {}\nprefix {}\n",
            head.is_some() && !permalink,
            facts.prefix
        )))
    }

    fn name(&self) -> &str {
        "gonk-github"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-github")
            .title("A browse root's repository on GitHub, and the ref a link names")
            .summary(format!(
                "urn:iki:gonk:github:{{root}}. The root's GitHub repository (its \
                 `remote.origin.url` normalized to https://github.com/{{owner}}/{{repo}}, or the \
                 `{OVERRIDE_KEY}` line naming it), and the ref a link to it names: the HEAD \
                 commit when a ref of origin contains it (a permalink), else origin's default \
                 branch. Plain text, one `key value` per line (repository, ref, permalink, \
                 ahead, prefix), or `none {{reason}}` when the root has no repository on \
                 GitHub. Reads `urn:repo:{{root}}:state` first, so it is cached until the root's \
                 refs, HEAD or config change."
            ))
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .input(
                ArgSpec::new("as")
                    .summary("the representation to answer in")
                    .class(XSD_STRING)
                    .one_of([TEXT])
                    .default_value(TEXT)
                    .optional(),
            )
            .output(TEXT)
    }
}

fn is_full_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The four local reads. None of them reaches the network: `remote.origin.url` is the
/// configured value (no `insteadOf` rewriting), and "origin has the HEAD" is as of the last
/// fetch or push.
fn read_facts(dir: &Path, head: Option<&str>) -> Facts {
    let origin = git_line(dir, &["config", "--get", "remote.origin.url"]);
    let prefix = git_line(dir, &["rev-parse", "--show-prefix"])
        .filter(|prefix| !prefix.contains('\n'))
        .unwrap_or_default();
    let on_origin = head.is_some_and(|sha| {
        git_line(
            dir,
            &[
                "for-each-ref",
                "--count=1",
                "--format=%(refname)",
                "--contains",
                sha,
                "refs/remotes/origin/",
            ],
        )
        .is_some()
    });
    let default_branch = git_line(
        dir,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .and_then(|name| name.strip_prefix("origin/").map(str::to_string))
    .filter(|name| !name.is_empty());
    Facts {
        origin,
        prefix,
        on_origin,
        default_branch,
    }
}

/// `git -C dir args…`'s output, trimmed, when it succeeded and said something.
fn git_line(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (out.status.success() && !text.is_empty()).then(|| text.to_string())
}

// ---------------------------------------------------------------- one view's page

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tree,
    File,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Tree => "tree",
            Kind::File => "file",
        }
    }

    /// GitHub's path word: `tree` for a directory, `blob` for a file.
    fn github(self) -> &'static str {
        match self {
            Kind::Tree => "tree",
            Kind::File => "blob",
        }
    }
}

/// `urn:iki:gonk:github:{root}:tree[:{path}]` and `…:file:{path}`.
struct Link {
    root: String,
    kind: Kind,
}

impl Link {
    /// The text face: `href`, `ref`, `permalink`, `kind` and, when the link is not a
    /// permalink, `note`; or `none {reason}`.
    async fn text(&self, inv: &Invocation<'_>) -> Result<String> {
        let rel = match inv.bindings.get("path") {
            None => Some(Vec::new()),
            Some(encoded) => crate::browse::decode_rel(encoded).map(|rel| {
                rel.components()
                    .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
                    .collect::<Vec<_>>()
            }),
        };
        let Some(rel) = rel else {
            return Ok("none the path is not a plain relative path\n".to_string());
        };
        let root = inv.source(&iri(&root_iri(&self.root))?).await?;
        let root = String::from_utf8_lossy(&root.bytes).into_owned();
        let Some(facts) = fields(&root) else {
            return Ok(root);
        };
        let (Some(repository), Some(reference)) = (facts.get("repository"), facts.get("ref"))
        else {
            return Err(Error::Endpoint(format!(
                "github: `{}` answered without a repository and a ref",
                root_iri(&self.root)
            )));
        };
        // Every segment through the one encoder: the ref's (a branch may hold `/`), the root's
        // place in its repository, then the path.
        let prefix = facts.get("prefix").map(String::as_str).unwrap_or_default();
        let mut href = format!("{repository}/{}", self.kind.github());
        for segment in reference
            .split('/')
            .chain(prefix.split('/'))
            .chain(rel.iter().map(String::as_str))
            .filter(|segment| !segment.is_empty())
        {
            href.push('/');
            href.push_str(&url_segment(segment));
        }
        let Some(href) = safe_href(&href) else {
            return Ok("none the link is not an http(s) URL\n".to_string());
        };
        let permalink = facts.get("permalink").is_some_and(|p| p == "true");
        let mut text = format!(
            "href {href}\nref {reference}\npermalink {permalink}\nkind {}\n",
            self.kind.word()
        );
        if !permalink {
            let note = if facts.get("ahead").is_some_and(|a| a == "true") {
                format!(
                    "The local tree is ahead of origin, so this links to {} and GitHub may show \
                     something different.",
                    branch_words(reference)
                )
            } else {
                format!(
                    "This checkout's commit is not known to be on GitHub, so this links to {} \
                     and GitHub may show something different.",
                    branch_words(reference)
                )
            };
            text.push_str(&format!("note {note}\n"));
        }
        Ok(text)
    }
}

/// How a note names the ref a fallback link names.
fn branch_words(reference: &str) -> String {
    if reference == "HEAD" {
        "the default branch".to_string()
    } else {
        format!("the {reference} branch")
    }
}

/// The view element the browse shell and the html face both render: `view:github`, or
/// nothing when there is no link.
pub fn view(text: &str) -> String {
    let Some(read) = fields(text) else {
        return String::new();
    };
    let Some(href) = read.get("href") else {
        return String::new();
    };
    let get = |key: &str| read.get(key).map(String::as_str).unwrap_or_default();
    let mut attributes = vec![
        ("href", href.as_str()),
        ("ref", get("ref")),
        ("permalink", get("permalink")),
        ("kind", get("kind")),
    ];
    if let Some(note) = read.get("note") {
        attributes.push(("note", note.as_str()));
    }
    element("github", &attributes, "")
}

#[async_trait]
impl Endpoint for Link {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        granted(inv, &self.root)?;
        let asked = face(inv)?;
        let text = self.text(inv).await?;
        if asked == TEXT {
            return Ok(plain(text));
        }
        // The fragment the browse page swaps in when its region moves (`web/gonk.js`).
        let doc = envelope(
            "page",
            &[("view", "github"), ("full", "false")],
            &view(&text),
        );
        let html = render::render(&doc, false).map_err(crate::web::render_err)?;
        Ok(crate::web::html(html).cacheable())
    }

    fn name(&self) -> &str {
        "gonk-github-link"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-github-link")
            .title("A browse view's page on GitHub")
            .summary(
                "urn:iki:gonk:github:{root}:tree[:{path}] and …:file:{path}, the path in \
                 browse's own spelling. The same directory or file on GitHub, at the ref \
                 urn:iki:gonk:github:{root} names: `tree/{ref}/{path}` or `blob/{ref}/{path}`, \
                 each segment percent-encoded. Plain text, one `key value` per line (href, \
                 ref, permalink, kind, and a note when the link is not a permalink), or \
                 `none {reason}`; text/html is the link the browse page shows.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .input(as_spec())
            .output(TEXT)
            .output(HTML)
            // The root's own tree binds no path; every other row does.
            .input(
                ArgSpec::new("path")
                    .summary("the root-relative path, in browse's spelling")
                    .class(XSD_STRING)
                    .binding()
                    .optional(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_name_mirrors_the_view_it_is_for() {
        assert_eq!(
            link_iri("urn:repo:core:tree").as_deref(),
            Some("urn:iki:gonk:github:core:tree")
        );
        assert_eq!(
            link_iri("urn:repo:core:file:src/a%20b.rs").as_deref(),
            Some("urn:iki:gonk:github:core:file:src/a%20b.rs")
        );
        for other in [
            "urn:repo:core:explain:src/lib.rs",
            "urn:repo:core:prs",
            "urn:repo:core:treehouse",
            "urn:iki:finding:x",
        ] {
            assert_eq!(link_iri(other), None, "{other}");
        }
    }
}
