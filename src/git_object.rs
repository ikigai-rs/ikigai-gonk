//! Git IRI links (#3198), composed over ikigai-repo's SHA-keyed object resource.
//!
//! `git-object:map:{map}:repository:{name}:sha:{sha}` is one immutable object;
//! `git-link:map:{map}:kind:{kind}:sha:{sha}` searches readable repositories in order.
//! No capability elevation lives here. Each map owns a confined repo kernel, issued under the
//! viewer's unchanged capability. The one git subprocess is the commit page's read of a
//! commit's message and changed files (ledger #3233), which no `urn:repo:*` resource answers;
//! it runs only after `granted` admits the viewer, reads a fixed argv, and is bounded. Presence is cached by SHA;
//! misses retain ikigai-repo's five-second bound. The search itself is never cached.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Invocation, Kernel, ReprType,
    Representation, Request, Result, UriTemplate, Verb,
};
use ikigai_fn::html::{attr, html_text, safe_href, url_segment};
use serde_json::Value;

const OBJECT: &str = "urn:iki:gonk:git-object:map:";
const LINK: &str = "urn:iki:gonk:git-link:map:";
const FILE: &str = "urn:iki:gonk:git-file:map:";
const STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// One configured clone, with optional external URL templates and local browse fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub dir: PathBuf,
    pub commit: Option<String>,
    pub blob: Option<String>,
    /// `gonk.repository.file` (ledger #3238): where a working-tree file IRI resolves when the
    /// viewer may not read the browse root. `{path}` is its only placeholder.
    pub file: Option<String>,
    pub browse: bool,
}

/// Browse roots double as an object map. No origin is guessed.
pub fn browse_repositories(roots: &[(String, PathBuf)]) -> Vec<Repository> {
    roots
        .iter()
        .map(|(name, dir)| Repository {
            name: name.clone(),
            dir: dir.clone(),
            commit: None,
            blob: None,
            file: None,
            browse: true,
        })
        .collect()
}

/// Refuse unsafe schemes, unknown placeholders and control characters at configuration time.
pub fn validate_template(template: &str, blob: bool) -> std::result::Result<(), String> {
    let mut sample = template.replace("{sha}", &"a".repeat(40));
    if blob {
        sample = sample
            .replace("{commit}", &"b".repeat(40))
            .replace("{path}", "src/file.rs");
    }
    if sample.contains(['{', '}'])
        || sample.chars().any(char::is_control)
        || !(sample.starts_with("https://") || sample.starts_with("http://"))
        || safe_href(&sample).is_none()
        || url::Url::parse(&sample).map_or(true, |u| {
            u.host_str().is_none() || !u.username().is_empty() || u.password().is_some()
        })
    {
        return Err("expected an http(s) URL without credentials; placeholders: {sha}, and for blobs {commit} and {path}".into());
    }
    Ok(())
}

/// A `gonk.repository.file` template (ledger #3238): an http(s) URL with a fixed host, no
/// credentials, and `{path}` as its only placeholder.
pub fn validate_file_template(template: &str) -> std::result::Result<(), String> {
    let sample = template.replace("{path}", "src/file.rs");
    if !template.contains("{path}")
        || sample.contains(['{', '}'])
        || sample.chars().any(char::is_control)
        || !(sample.starts_with("https://") || sample.starts_with("http://"))
        || safe_href(&sample).is_none()
        || url::Url::parse(&sample).map_or(true, |u| {
            u.host_str().is_none() || !u.username().is_empty() || u.password().is_some()
        })
    {
        return Err(
            "expected an http(s) URL without credentials, with the placeholder {path}".into(),
        );
    }
    Ok(())
}

/// The configured file template of `name`, `{path}` filled segment by segment.
pub fn file_iri(map: &str, name: &str) -> String {
    format!("{FILE}{map}:repository:{}", url_segment(name))
}

/// `template` with `{path}` filled, each segment URL encoded; `None` when unsafe.
pub fn expand_file(template: &str, path: &str) -> Option<String> {
    let path = path
        .split('/')
        .map(url_segment)
        .collect::<Vec<_>>()
        .join("/");
    safe_href(&template.replace("{path}", &path)).map(str::to_string)
}

/// A recognized full Git IRI, normalized to a lowercase SHA. Abbreviations are not identities.
pub fn git_iri(target: &str) -> Option<(&str, String)> {
    let (kind, sha) = target.strip_prefix("urn:git:")?.split_once(':')?;
    (matches!(kind, "commit" | "blob")
        && matches!(sha.len(), 40 | 64)
        && sha.bytes().all(|b| b.is_ascii_hexdigit()))
    .then(|| (kind, sha.to_ascii_lowercase()))
}

pub fn link_iri(map: &str, kind: &str, sha: &str) -> String {
    format!("{LINK}{map}:kind:{kind}:sha:{sha}")
}

pub fn object_iri(map: &str, name: &str, sha: &str) -> String {
    format!("{OBJECT}{map}:repository:{}:sha:{sha}", url_segment(name))
}

fn template(text: &str) -> UriTemplate {
    UriTemplate::parse(text).expect("a fixed object template")
}

/// The map's object and link resources. A browse reconfiguration replaces this whole space.
pub fn space(map: &str, repositories: &[Repository]) -> EndpointSpace {
    let kernel = Arc::new(Kernel::new(Arc::new(ikigai_repo::space_with_roots(
        repositories.iter().map(|r| &r.dir),
    ))));
    let mut space = EndpointSpace::new();
    for repository in repositories {
        space = space.bind(
            template(&object_iri(map, &repository.name, "{sha}")),
            Object {
                map: map.to_string(),
                repository: repository.clone(),
                kernel: Arc::clone(&kernel),
            },
        );
    }
    for repository in repositories {
        if let Some(file) = &repository.file {
            space = space.bind(
                template(&file_iri(map, &repository.name)),
                FileLink {
                    template: file.clone(),
                },
            );
        }
    }
    space.bind(
        template(&link_iri(map, "{kind}", "{sha}")),
        Link {
            map: map.to_string(),
            repositories: repositories.to_vec(),
        },
    )
}

fn granted(inv: &Invocation<'_>, name: &str) -> Result<()> {
    if !(inv
        .capability
        .allows(&format!("{}{name}", ikigai_browse::CAP_PREFIX))
        || inv.capability.allows(ikigai_browse::CAP_WILDCARD))
    {
        return Err(Error::Denied(
            "Git object: repository is not readable".into(),
        ));
    }
    if !inv.capability.allows("urn:cap:exec:git") {
        return Err(Error::Denied(
            "Git object: urn:cap:exec:git is required".into(),
        ));
    }
    Ok(())
}

fn request(target: &str) -> Result<Request> {
    crate::web::request(Verb::Source, target)
}
fn arg(request: Request, name: &str, value: &str) -> Request {
    request.with_arg(name, ArgRef::Inline(value.as_bytes().to_vec()))
}

struct Object {
    map: String,
    repository: Repository,
    kernel: Arc<Kernel>,
}

#[async_trait]
impl Endpoint for Object {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let repo = &self.repository;
        granted(inv, &repo.name)?;
        let sha = inv
            .bindings
            .get("sha")
            .ok_or_else(|| Error::MissingArgument("sha".into()))?;
        if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::InvalidArgument {
                name: "sha".into(),
                detail: "expected a full SHA".into(),
            });
        }
        let dir = repo
            .dir
            .canonicalize()
            .map_err(|_| Error::NotFound("repository unavailable".into()))?;
        let basename = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::NotFound("repository unavailable".into()))?;
        let read = arg(
            arg(
                request(&format!(
                    "urn:repo:{}:object:sha:{sha}",
                    url_segment(basename)
                ))?,
                "dir",
                &dir.to_string_lossy(),
            ),
            "as",
            "application/json",
        );
        let answer = Kernel::issue(&self.kernel, read, inv.capability).await?;
        let mut value: Value =
            serde_json::from_slice(&answer.bytes).map_err(|e| Error::Endpoint(e.to_string()))?;
        let kind = value["type"].as_str().unwrap_or("");
        let normalized = value["sha"].as_str().unwrap_or(sha);
        let configured = match kind {
            "commit" => repo.commit.as_ref(),
            "blob" => repo.blob.as_ref(),
            _ => None,
        };
        let href = if let Some(configured) = configured {
            expand(configured, normalized, &value)
        } else if repo.browse && matches!(kind, "commit" | "blob") {
            Some(format!("/browse/{}", inv.request.target))
        } else {
            None
        };
        value["href"] = href.map_or(Value::Null, Value::String);
        if inv.inline_str("as").unwrap_or("application/json") == "text/html" {
            let kind = value["type"].as_str().unwrap_or("object");
            let sha = value["sha"].as_str().unwrap_or(sha);
            let mut html = format!("<section class='panel'><h1>Git {}</h1><p><code class='iri'>urn:git:{}:{}</code></p>", html_text(kind), html_text(kind), html_text(sha));
            if kind == "blob" {
                if let (Some(commit), Some(path)) = (
                    value["firstAppearance"]["commit"].as_str(),
                    value["firstAppearance"]["path"].as_str(),
                ) {
                    let read = arg(
                        arg(
                            arg(request("urn:repo:show")?, "dir", &dir.to_string_lossy()),
                            "rev",
                            commit,
                        ),
                        "path",
                        path,
                    );
                    let body = Kernel::issue(&self.kernel, read, inv.capability).await?;
                    html.push_str(&format!("<p>File: <code class='iri'>{}</code> at commit <code class='iri'>{}</code></p><pre class='graph' tabindex='0' role='region' aria-label='File contents'><code>{}</code></pre>", html_text(path), html_text(commit), html_text(&String::from_utf8_lossy(&body.bytes))));
                } else {
                    html.push_str("<p>This blob has no path in retained reachable history.</p>");
                }
            }
            if kind == "commit" {
                html.push_str(&self.commit_html(inv, &dir, sha).await?);
            }
            if repo.browse {
                html.push_str(&format!("<p><a href='/browse/urn:repo:{}:tree'>Browse {} (current working tree)</a></p>", url_segment(&repo.name), html_text(&repo.name)));
            }
            html.push_str("</section>");
            return Ok(Representation::new(ReprType::new("text/html"), html).cacheable());
        }
        Ok(Representation::new(ReprType::new("application/json"), value.to_string()).cacheable())
    }
    fn name(&self) -> &str {
        "gonk-git-object"
    }
    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("A Git object in a configured repository")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .requires("urn:cap:exec:git")
            .input(ArgSpec::new("sha").binding().class(STRING))
            .input(
                ArgSpec::new("as")
                    .class(STRING)
                    .one_of(["application/json", "text/html"])
                    .default_value("application/json")
                    .optional(),
            )
            .output("application/json")
            .output("text/html")
    }
}

/// The most changed files a commit page lists (ledger #3233); a line counts the rest.
pub const COMMIT_FILES_SHOWN: usize = 50;
/// The most bytes read from git for a commit's message, or for its list of changed files.
const COMMIT_READ_LIMIT: usize = 1024 * 1024;

impl Object {
    /// The commit page's body: the message (first line as a heading, the rest preformatted),
    /// the author and date, and the changed files against the first parent, each linked to its
    /// blob page through this map's link resource under the viewer's capability.
    async fn commit_html(&self, inv: &Invocation<'_>, dir: &Path, sha: &str) -> Result<String> {
        let (head, _) = git_read(
            dir,
            &[
                "-c",
                "log.showSignature=false",
                "show",
                "-s",
                "--no-color",
                "--encoding=UTF-8",
                "--format=%an%x00%aI%x00%B",
                sha,
            ],
        )?;
        let head = String::from_utf8_lossy(&head);
        let mut fields = head.splitn(3, '\0');
        let author = fields.next().unwrap_or("").trim();
        let date = fields.next().unwrap_or("").trim();
        let message = fields.next().unwrap_or("").trim_end();
        let (summary, body) = message.split_once('\n').unwrap_or((message, ""));
        let body = body.trim_matches('\n');
        let mut html = format!(
            "<h2>{}</h2><p>Author: {} · Date: <time datetime='{}'>{}</time></p>",
            html_text(summary.trim()),
            html_text(author),
            attr(date),
            html_text(date)
        );
        if !body.is_empty() {
            html.push_str(&format!("<pre class='graph' tabindex='0' role='region' aria-label='Commit message'><code>{}</code></pre>", html_text(body)));
        }
        let (raw, cut) = git_read(
            dir,
            &[
                "diff-tree",
                "-r",
                "-z",
                "--raw",
                "--no-renames",
                "--root",
                "--diff-merges=first-parent",
                "--no-commit-id",
                sha,
            ],
        )?;
        let changes = changed_files(&raw, cut);
        html.push_str("<h2>Changed files</h2>");
        if changes.is_empty() {
            html.push_str("<p>No files changed against the first parent.</p>");
            return Ok(html);
        }
        html.push_str("<ul class='changed-files'>");
        for change in changes.iter().take(COMMIT_FILES_SHOWN) {
            let href = match change.blob() {
                Some(blob) => self.blob_href(inv, blob).await,
                None => None,
            };
            let path = html_text(&change.path);
            let name = match href {
                Some(href) => format!("<a href='{}'><code>{path}</code></a>", attr(&href)),
                None => format!("<code>{path}</code>"),
            };
            html.push_str(&format!("<li>{name} ({})</li>", change.status()));
        }
        html.push_str("</ul>");
        let more = changes.len().saturating_sub(COMMIT_FILES_SHOWN);
        if cut {
            html.push_str(&format!(
                "<p>And at least {more} more changed files, not listed.</p>"
            ));
        } else if more > 0 {
            let files = if more == 1 { "file" } else { "files" };
            html.push_str(&format!(
                "<p>And {more} more changed {files}, not listed.</p>"
            ));
        }
        Ok(html)
    }

    /// The blob page's href, through `git-link` in this map, or `None` when no repository the
    /// viewer may read links it.
    async fn blob_href(&self, inv: &Invocation<'_>, blob: &str) -> Option<String> {
        let answer = inv
            .issue(request(&link_iri(&self.map, "blob", blob)).ok()?)
            .await
            .ok()?;
        let text = std::str::from_utf8(&answer.bytes).ok()?;
        safe_href(text)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

/// One entry of `git diff-tree --raw`: the status letter, the new mode and blob, and the path.
#[derive(Debug, PartialEq, Eq)]
struct Change {
    status: char,
    mode: String,
    blob: String,
    path: String,
}

impl Change {
    /// The blob to link: the file as the commit leaves it. A deleted file, a submodule and a
    /// symlink link nothing.
    fn blob(&self) -> Option<&str> {
        (self.status != 'D' && self.mode.starts_with("100") && self.blob.bytes().any(|b| b != b'0'))
            .then_some(self.blob.as_str())
    }
    fn status(&self) -> &'static str {
        match self.status {
            'A' => "added",
            'D' => "deleted",
            'M' => "modified",
            'T' => "type changed",
            _ => "changed",
        }
    }
}

/// The entries of `git diff-tree -r -z --raw --no-renames` output. When the read was `cut` at
/// its limit, the last entry may be partial, so it is dropped.
fn changed_files(raw: &[u8], cut: bool) -> Vec<Change> {
    let mut fields = raw.split(|b| *b == 0);
    let mut changes = Vec::new();
    while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
        let meta = String::from_utf8_lossy(meta);
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [_, mode, _, blob, status] = parts[..] else {
            continue;
        };
        changes.push(Change {
            status: status.chars().next().unwrap_or('?'),
            mode: mode.to_string(),
            blob: blob.to_string(),
            path: String::from_utf8_lossy(path).into_owned(),
        });
    }
    if cut {
        changes.pop();
    }
    changes
}

/// `git -C dir --no-replace-objects args…`, its stdout up to [`COMMIT_READ_LIMIT`] and whether
/// it was cut there. Never fetches, never prompts, reads no stdin.
fn git_read(dir: &Path, args: &[&str]) -> Result<(Vec<u8>, bool)> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("--no-replace-objects")
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::Endpoint(format!("Git commit: could not run git: {e}")))?;
    let mut out = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("stdout is piped")
        .take(COMMIT_READ_LIMIT as u64 + 1)
        .read_to_end(&mut out);
    let cut = out.len() > COMMIT_READ_LIMIT;
    if cut {
        out.truncate(COMMIT_READ_LIMIT);
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|e| Error::Endpoint(format!("Git commit: git did not finish: {e}")))?;
    read.map_err(|e| Error::Endpoint(format!("Git commit: could not read git: {e}")))?;
    if !cut && !status.success() {
        return Err(Error::Endpoint(
            "Git commit: git could not read the commit".into(),
        ));
    }
    Ok((out, cut))
}

fn expand(template: &str, sha: &str, object: &Value) -> Option<String> {
    let mut href = template.replace("{sha}", sha);
    if href.contains("{commit}") || href.contains("{path}") {
        let commit = object["firstAppearance"]["commit"].as_str()?;
        let path = object["firstAppearance"]["path"].as_str()?;
        let path = path
            .split('/')
            .map(url_segment)
            .collect::<Vec<_>>()
            .join("/");
        href = href.replace("{commit}", commit).replace("{path}", &path);
    }
    safe_href(&href).map(str::to_string)
}

struct Link {
    map: String,
    repositories: Vec<Repository>,
}
#[async_trait]
impl Endpoint for Link {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let kind = inv.bindings.get("kind").unwrap_or("");
        let sha = inv.bindings.get("sha").unwrap_or("");
        if git_iri(&format!("urn:git:{kind}:{sha}")).is_none() {
            return Ok(Representation::new(ReprType::new("text/plain"), ""));
        }
        for repo in &self.repositories {
            if granted(inv, &repo.name).is_err() {
                continue;
            }
            let read = request(&object_iri(&self.map, &repo.name, sha))?;
            if let Ok(answer) = inv.issue(read).await {
                if let Ok(value) = serde_json::from_slice::<Value>(&answer.bytes) {
                    if value["type"].as_str() == Some(kind) {
                        if let Some(href) = value["href"].as_str().and_then(safe_href) {
                            return Ok(Representation::new(ReprType::new("text/plain"), href));
                        }
                    }
                }
            }
        }
        Ok(Representation::new(ReprType::new("text/plain"), ""))
    }
    fn name(&self) -> &str {
        "gonk-git-link"
    }
    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("A readable Git object's code link")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("kind")
                    .binding()
                    .class(STRING)
                    .one_of(["commit", "blob"]),
            )
            .input(ArgSpec::new("sha").binding().class(STRING))
            .output("text/plain")
    }
}

/// A configured `gonk.repository.file` template, filled with the `path` argument (ledger
/// #3238). It reads nothing: the template is configuration, and whether the file exists is the
/// target's to answer when it is followed. Answers the href, or empty.
struct FileLink {
    template: String,
}
#[async_trait]
impl Endpoint for FileLink {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let path = inv.inline_str("path").unwrap_or("");
        let ok = !path.is_empty() && path.split('/').all(|s| !matches!(s, "" | "." | ".."));
        let href = ok
            .then(|| expand_file(&self.template, path))
            .flatten()
            .unwrap_or_default();
        Ok(Representation::new(ReprType::new("text/plain"), href))
    }
    fn name(&self) -> &str {
        "gonk-git-file-link"
    }
    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("A working-tree file's configured web link")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(ArgSpec::new("path").class(STRING))
            .output("text/plain")
    }
}

/// Configured clones and URL templates. Unknown names and duplicate entries are errors.
pub fn configured(
    text: &str,
    home: &Path,
    roots: &[(String, PathBuf)],
) -> std::result::Result<Vec<Repository>, String> {
    let mut repositories = Vec::<Repository>::new();
    for line in crate::config::values_for(text, "gonk.repository") {
        let (name, path) = line
            .split_once('=')
            .ok_or("gonk.repository: expected name=path")?;
        let (name, path) = (name.trim(), path.trim());
        crate::browse::check_root_name(name)?;
        if repositories.iter().any(|r| r.name == name) {
            return Err(format!("gonk.repository: duplicate name `{name}`"));
        }
        let dir = if let Some(rest) = path.strip_prefix("~/") {
            home.join(rest)
        } else {
            PathBuf::from(path)
        };
        let dir = dir
            .canonicalize()
            .map_err(|_| format!("gonk.repository: `{name}` is not an available directory"))?;
        if !dir.is_dir() || dir.to_str().is_none() {
            return Err(format!(
                "gonk.repository: `{name}` must be a UTF-8 directory"
            ));
        }
        let browse = roots
            .iter()
            .any(|(n, p)| n == name && p.canonicalize().ok().as_ref() == Some(&dir));
        repositories.push(Repository {
            name: name.into(),
            dir,
            commit: None,
            blob: None,
            file: None,
            browse,
        });
    }
    for kind in ["commit", "blob"] {
        let key = format!("gonk.repository.{kind}");
        for line in crate::config::values_for(text, &key) {
            let (name, template) = line
                .split_once('=')
                .ok_or_else(|| format!("{key}: expected name=URL-template"))?;
            let repo = repositories
                .iter_mut()
                .find(|r| r.name == name.trim())
                .ok_or_else(|| format!("{key}: unknown repository `{}`", name.trim()))?;
            validate_template(template.trim(), kind == "blob")
                .map_err(|e| format!("{key}: {e}"))?;
            let slot = if kind == "commit" {
                &mut repo.commit
            } else {
                &mut repo.blob
            };
            if slot.replace(template.trim().into()).is_some() {
                return Err(format!("{key}: duplicate template for `{}`", repo.name));
            }
        }
    }
    for line in crate::config::values_for(text, "gonk.repository.file") {
        let key = "gonk.repository.file";
        let (name, template) = line
            .split_once('=')
            .ok_or_else(|| format!("{key}: expected name=URL-template"))?;
        let repo = repositories
            .iter_mut()
            .find(|r| r.name == name.trim())
            .ok_or_else(|| format!("{key}: unknown repository `{}`", name.trim()))?;
        validate_file_template(template.trim()).map_err(|e| format!("{key}: {e}"))?;
        if repo.file.replace(template.trim().into()).is_some() {
            return Err(format!("{key}: duplicate template for `{}`", repo.name));
        }
    }
    Ok(repositories)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_is_keyed_and_validated() {
        let dir = tempfile::tempdir().unwrap();
        let text = format!("gonk.repository = \"demo={}\"\ngonk.repository.commit = \"demo=https://code.example/commit/{{sha}}\"\ngonk.repository.blob = \"demo=https://code.example/blob/{{commit}}/{{path}}\"", dir.path().display());
        let roots = vec![("demo".into(), dir.path().to_path_buf())];
        let parsed = configured(&text, dir.path(), &roots).unwrap();
        assert!(parsed[0].browse);
        assert_eq!(parsed[0].name, "demo");
        for suffix in [
            "\ngonk.repository = \"demo=/\"".to_string(),
            "\ngonk.repository.commit = \"unknown=https://code.example/{sha}\"".to_string(),
            "\ngonk.repository.blob = \"demo=https://other.example/{sha}\"".to_string(),
        ] {
            assert!(configured(&format!("{text}{suffix}"), dir.path(), &roots).is_err());
        }
        assert!(configured(
            "gonk.repository = \"bad=/a/nonexistent/clone\"",
            dir.path(),
            &[]
        )
        .is_err());
        assert!(configured("gonk.repository = \"missing-equals\"", dir.path(), &[]).is_err());
        let expanded = configured("gonk.repository = \"demo=~/\"", dir.path(), &[]).unwrap();
        assert_eq!(expanded[0].dir, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn templates_and_iris_accept_only_their_declared_grammar() {
        for bad in [
            "javascript:alert(1)",
            "//code.example/{sha}",
            "https://user:secret@code.example/{sha}",
            "https://code.example/{unknown}",
            "https://code.example/\n{sha}",
            "https://",
            "https://code.example/{commit}",
        ] {
            assert!(validate_template(bad, false).is_err(), "{bad}");
        }
        for good in [
            "https://code.example/commit/{sha}",
            "http://localhost:1062/object/{sha}",
        ] {
            assert!(validate_template(good, false).is_ok());
        }
        assert!(
            validate_template("https://code.example/blob/{commit}/{path}?sha={sha}", true).is_ok()
        );
        for bad in [
            "urn:git:commit:main",
            "urn:git:blob:123",
            "urn:git:tree:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "urn:git:commit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa;",
        ] {
            assert!(git_iri(bad).is_none());
        }
        assert_eq!(
            git_iri(&format!("urn:git:blob:{}", "A".repeat(64))),
            Some(("blob", "a".repeat(64)))
        );
    }
}
