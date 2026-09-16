//! The repository browse family, wired onto gonk's one dataset — and cached, because gonk
//! watches the disk.
//!
//! ```text
//! urn:repo:{root}:tree[:{path}]        Source  a directory listing
//! urn:repo:{root}:file:{path}          Source  one file
//! urn:repo:{root}:hash[:{path}]        Source  the content hash the archive keys on
//! urn:repo:{root}:state                Source  git HEAD and the dirty set
//! urn:repo:{root}:prs[:{path}], :pr:{n}  Source  the pull-request family (through ikigai-repo)
//! urn:repo:style                       Source  the a11y-layered stylesheet
//! urn:iki:annotation[:{id}]            Source/Sink/Delete  W3C annotations, in THIS dataset
//! ```
//!
//! # One dataset, and what that buys
//!
//! The annotation family takes an `Arc<Store>` — the same dataset the ledger's named graphs
//! live in, handed over by `DurableStore::open_shared_declaring`. That is the whole point of
//! the arc: a ledger item's `ledger:about <urn:repo:…>` and an annotation on that file are
//! two graphs in ONE store, so the join is a local SPARQL query and not a federation problem.
//! What the shared handle costs the ledger's read cache is **nothing**, and the reason is the
//! declaration on that call: `SharerWrites::only_the_default_graph` names where this family
//! writes, so `ikigai-store` keeps every scoped read of every other graph cacheable under its
//! own write threads (`src/main.rs`, and `tests/browse.rs` prints the numbers).
//!
//! ⚠ **Where browse's quads land is browse's decision, not this server's, and it is the
//! default graph** — so that decision is also what the declaration above promises, and a
//! change to it makes this server's cache wrong rather than merely different.
//! `ikigai-browse` hard-codes `GraphName::DefaultGraph` in all three of its
//! writers. gonk cannot give the family its own named graph (`urn:iki:browse:graph:…`, which
//! is what the ledger's graph-per-tenant shape would suggest) without a change to that
//! crate. The consequence is a capability one, and it is stated on the doors: `ikigai-store`
//! gates named graphs exactly (`urn:cap:store:read:graph:<iri>`) and the default graph has no
//! IRI, so a scoped reader cannot reach browse's quads at all — only the BROAD
//! `urn:iki:store:select` under `urn:cap:store:read` can, which this server hands to nobody
//! but the socket door's root. A join across ledger and browse data is therefore an
//! owner-only query today.
//!
//! # Explanations, and what binds them
//!
//! `Mount::explain` binds `urn:repo:{root}:{explain,explain-versions,review}` and the
//! PR-derived layers, every one of which derives through `urn:llm:{provider}:ask`. gonk
//! links no LLM client and never will — it MOUNTS one ([`crate::mount`]) — so those rows are
//! bound **only when a `gonk.mount` line names the peer that serves them**
//! ([`crate::config::Settings::explains`]). With no mount they would be actions the manifold
//! offers and the kernel can never satisfy: an over-offer, which is the one direction the
//! module recipe calls worse than a missing feature.
//!
//! ## ⚠ The spend gate is a capability, and it is `ikigai-browse`'s, not gonk's
//!
//! Every derivation declares TWO capabilities: `urn:cap:browse:read:*` (the wildcard
//! offering — enforcement checks the target's root) and `urn:cap:net:*`, because calling a
//! model is a network act even against localhost. `urn:repo:{root}:review:{path}` declares a
//! third, `urn:cap:annotate`, because its findings are minted as real annotations in this
//! dataset. Since `declared = enforced`, the kernel refuses before dispatch, and
//! `urn:kernel:actions` — capability-scoped by construction — does not offer an explain row
//! to a caller who could not invoke it.
//!
//! So the whole gate is which door's capability carries a net grant. gonk mints none:
//! `ikigai-gonk grants`, `client add` and `passkey invite` write per-ledger tokens only, and
//! `grants.json` refuses the wildcard `urn:cap:net:*` as a GRANT the way it refuses
//! `urn:cap:exec:*` ([`crate::grants::unbounded_net_scopes`]). The per-door table is in the
//! README; the short form is that **an anonymous HTTP caller cannot reach a browse row at
//! all**, so it can neither derive an explanation nor read an archived one, and the socket
//! door's root can do both.
//!
//! ⚠ **The archive read and the derivation are ONE action to a capability.** `version=`
//! addresses an archived entry and provably derives nothing (`ikigai-browse` returns
//! `NotFound` on a miss rather than falling back to a model), but it is the same
//! `urn:repo:{root}:explain` row and therefore carries the same `urn:cap:net:*`
//! requirement — so "free to read what was already paid for" cannot be granted separately
//! today. That matters for the HTTP browse face (#258), where an anonymous reader is exactly
//! the caller who should see archived text and never spend.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_browse::{ExplainConfig, Mount, StyleWatch};
use ikigai_core::{
    Description, Endpoint, EndpointSpace, Invocation, Representation, Request, Resolution, Result,
    Scope, Space, SpaceEntry, Verb,
};
use ikigai_store::Store;

use crate::config::ExplainTiers;
use crate::watch::{root_thread, Watched};

/// Root names this server refuses, because `ikigai-repo`'s own resources start with the same
/// segment and a reader could not tell which family answered.
///
/// `ikigai-browse` would accept them (its rows are per-root and would simply sit beside the
/// facades), so this is gonk's composition talking, not browse's grammar.
pub const RESERVED_ROOTS: [&str; 2] = ["pr", "style"];

/// Check a root name the way `ikigai-browse` does — before it can panic at mount time.
///
/// `build_roots` asserts on an empty name, a name containing `:`, `/`, `{` or `}`, and a
/// duplicate. An assert is the right shape for a library and the wrong one for a server: it
/// arrives as a panic in the banner's place, with a Rust backtrace instead of the config line
/// to edit.
///
/// # Errors
///
/// When the name could not be a root, or is one of [`RESERVED_ROOTS`].
pub fn check_root_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() || name.contains([':', '/', '{', '}']) {
        return Err(format!(
            "browse root `{name}`: a root name must be non-empty and contain no `:`, `/`, \
             `{{` or `}}` — it is spliced into `urn:repo:<name>:…`"
        ));
    }
    if RESERVED_ROOTS.contains(&name) {
        return Err(format!(
            "browse root `{name}`: that name is reserved — `urn:repo:{name}:…` is already \
             ikigai-repo's, so a root by this name would answer under a name a reader would \
             read as the other family's"
        ));
    }
    Ok(())
}

/// Wire the family: the space to compose, and the watch that keeps `urn:repo:style` fresh.
///
/// `watched` is what [`crate::watch::RootWatch::start`] actually got — the reads of those
/// roots, and only those, are made cacheable ([`cached_reads`]).
///
/// `explain` is `Some` when a peer serves `urn:llm:*` — see the module docs for why that is
/// the switch. The tiers ride in as [`crate::config::ExplainTiers`] rather than as an
/// `ExplainConfig`, so the one place that turns an operator's ceilings into browse's builder
/// is here, beside the store handle the archive needs.
pub fn wire(
    roots: Vec<(String, PathBuf)>,
    store: Arc<Store>,
    watched: &[Watched],
    explain: Option<&ExplainTiers>,
) -> Wired {
    let mount = Mount::new(roots)
        .annotations(Arc::clone(&store))
        // The PROCESS's name: it selects the `gonk.a11y.toml` layer `urn:repo:style` reads
        // its themes and its contrast floor from. Without it that file would sit on disk
        // doing nothing — the quietest kind of wrong.
        .app("gonk");
    let mount = match explain {
        Some(tiers) => mount.explain(explain_config(store, tiers)),
        None => mount,
    };
    let (space, style) = mount.space_watched();
    Wired {
        space: cached_reads(space, watched),
        style,
    }
}

/// The operator's tiers as `ikigai-browse`'s config.
///
/// ★ **No `allow_provider` call, deliberately.** A `provider=` argument may name only what
/// this host already asks with — the two configured tiers — and widening that set is
/// documented by that crate as THE authority boundary: `explain`'s declared capability
/// cannot vary by argument value, so a caller who may derive at all could otherwise point
/// this server at any backend the peer's registry happens to hold. gonk's tiers are the
/// operator's choice already; a caller does not get a second one.
///
/// The model LABELS are left unset on purpose too. Unset, `ikigai-browse` resolves the true
/// configured model id through `urn:llm:{provider}:model` — over the mount — at explain
/// time, so swapping the peer's model re-keys the archive with no gonk-side config. An
/// operator override here would pin tags to a string that can silently stop being true.
fn explain_config(store: Arc<Store>, tiers: &ExplainTiers) -> ExplainConfig {
    ExplainConfig::new(store)
        .file_provider(&tiers.file.provider)
        .file_max_tokens(tiers.file.max_tokens)
        .dir_provider(&tiers.dir.provider)
        .dir_max_tokens(tiers.dir.max_tokens)
        .review_provider(&tiers.review.provider)
        .review_max_tokens(tiers.review.max_tokens)
        .pr_provider(&tiers.pr.provider)
        .pr_max_tokens(tiers.pr.max_tokens)
        .max_prompt_bytes(tiers.max_prompt_bytes)
}

/// What [`wire`] hands back.
pub struct Wired {
    /// The family, with the cacheable overlay in front.
    pub space: CachedReads,
    /// browse's own watch over the config home's `a11y.toml` layers. A declared thread is a
    /// promise that something cuts it, and this is that something: the host starts it once
    /// its kernel exists.
    pub style: StyleWatch,
}

/// Declare the filesystem reads of a WATCHED root cacheable, under the thread
/// [`crate::watch::RootWatch`] cuts.
///
/// # ★ What is cached, and what is deliberately not
///
/// | read | cached? | why |
/// |---|---|---|
/// | `Source urn:repo:{root}:{tree,file,hash,state}`, **no arguments**, watched root | yes | a pure function of bytes under the root, and the watcher sees every change to those |
/// | the same with any argument (`as=text/html`, `annotations=include`, `version=`, …) | no | those faces read the annotation overlay out of the store — and `ikigai-browse` REWRITES an annotation during a Source when the file it anchors to has drifted, so the answer depends on state this thread does not track |
/// | `urn:repo:{root}:prs`, `:pr:{n}` | no | they resolve `urn:repo:pr:*` through the kernel, which runs `gh`: the input is GitHub, not the disk, and a filesystem thread would hold a stale PR list until someone touched a file |
/// | `urn:repo:{root}:annotations[:{path}]`, `urn:iki:annotation:{id}` | no | store-derived, and the same drift rewrite applies |
/// | `urn:repo:{root}:explain[:{path}]`, `:explain-versions`, `:review:{path}` | no | the ARCHIVE is what makes these cheap the second time — keyed on `(path, content-hash, version-tag)` in the store, which re-keys on an edit by construction. A kernel cache entry in front of it would be a second, weaker copy of that, and one whose thread this watcher could not cut when the model changed |
/// | `urn:repo:style` | already | `ikigai-browse` declares it cacheable under one thread per `a11y.toml` candidate, and ships the watch that cuts them ([`Wired::style`]) |
/// | any read of an UNWATCHED root | no | fail closed: no watcher, no cache — the two halves move together or this is ledger #246 again |
///
/// The argument test is deliberately blunt ("no arguments at all") rather than a list of the
/// arguments that are safe. A new face in a later `ikigai-browse` arrives as a new argument,
/// and the failure of the permissive rule is a silently stale read; the failure of this one
/// is a read that is merely not cached.
pub fn cached_reads(inner: EndpointSpace, watched: &[Watched]) -> CachedReads {
    CachedReads {
        inner,
        roots: watched.iter().map(|root| root.name.clone()).collect(),
    }
}

/// The space [`cached_reads`] builds.
pub struct CachedReads {
    inner: EndpointSpace,
    roots: Vec<String>,
}

impl Space for CachedReads {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        let resolution = self.inner.resolve(request, scope);
        match self.thread_for(request) {
            Some(thread) => resolution.map_endpoint(move |endpoint| {
                Arc::new(Cached {
                    inner: endpoint,
                    thread: thread.clone(),
                }) as Arc<dyn Endpoint>
            }),
            None => resolution,
        }
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        // The catalog is browse's, unchanged.
        self.inner.entries()
    }
}

impl CachedReads {
    /// The golden thread this request's answer depends on, when it is one of the reads this
    /// server caches. See [`cached_reads`] for the table.
    fn thread_for(&self, request: &Request) -> Option<String> {
        if request.verb != Verb::Source || !request.args.is_empty() {
            return None;
        }
        let (root, rest) = split_repo_iri(request.target.as_str())?;
        if !self.roots.iter().any(|watched| watched == root) {
            return None;
        }
        let filesystem = matches!(rest, "tree" | "hash" | "state")
            || rest.starts_with("tree:")
            || rest.starts_with("hash:")
            || rest.starts_with("file:");
        filesystem.then(|| root_thread(root))
    }
}

/// `urn:repo:{root}:{rest}` split into its two halves. `None` for anything else — including
/// `ikigai-repo`'s own `urn:repo:status`, which has no second colon after the prefix.
fn split_repo_iri(target: &str) -> Option<(&str, &str)> {
    target.strip_prefix("urn:repo:")?.split_once(':')
}

/// One browse read, cached under its root's thread.
struct Cached {
    inner: Arc<dyn Endpoint>,
    thread: String,
}

#[async_trait]
impl Endpoint for Cached {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        Ok(self
            .inner
            .invoke(inv)
            .await?
            .cacheable()
            .depends_on(self.thread.as_str()))
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        // Unchanged — including the summaries that say these reads are live. They still are:
        // what makes them so is now a watcher rather than a recomputation, and a contract
        // that started describing this server's caching would be describing the wrong layer.
        self.inner.describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikigai_core::{ArgRef, Iri};

    fn watched(names: &[&str]) -> Vec<Watched> {
        names
            .iter()
            .map(|name| Watched {
                name: (*name).to_string(),
                dir: PathBuf::from("/tmp").join(name),
            })
            .collect()
    }

    fn space() -> CachedReads {
        cached_reads(EndpointSpace::new(), &watched(&["core"]))
    }

    fn request(verb: Verb, iri: &str) -> Request {
        Request::new(verb, Iri::parse(iri).expect("a test IRI"))
    }

    #[test]
    fn the_filesystem_reads_of_a_watched_root_are_cached() {
        let space = space();
        for iri in [
            "urn:repo:core:tree",
            "urn:repo:core:tree:src",
            "urn:repo:core:file:src/lib.rs",
            "urn:repo:core:hash",
            "urn:repo:core:hash:src",
            "urn:repo:core:state",
        ] {
            assert_eq!(
                space.thread_for(&request(Verb::Source, iri)).as_deref(),
                Some("urn:iki:gonk:browse:root:core"),
                "{iri}"
            );
        }
    }

    #[test]
    fn nothing_else_is() {
        let space = space();
        for iri in [
            // Another root's name — unwatched, so uncached: the fail-closed half.
            "urn:repo:other:tree",
            // Store-derived, and rewritten during a read when the anchor drifts.
            "urn:repo:core:annotations",
            "urn:repo:core:annotations:src/lib.rs",
            "urn:iki:annotation:n",
            // `gh`, not the disk.
            "urn:repo:core:prs",
            "urn:repo:core:pr:12",
            // The archive is the cache for these — see the table.
            "urn:repo:core:explain",
            "urn:repo:core:explain:src/lib.rs",
            "urn:repo:core:explain-versions:src/lib.rs",
            "urn:repo:core:review:src/lib.rs",
            "urn:repo:core:pr:12:explain",
            // browse declares this one itself, under its own threads.
            "urn:repo:style",
            // ikigai-repo's facades.
            "urn:repo:status",
            "urn:repo:pr:list",
        ] {
            assert_eq!(space.thread_for(&request(Verb::Source, iri)), None, "{iri}");
        }
    }

    /// ★ An argument means a face whose inputs this thread does not track — the HTML and
    /// `annotations=include` faces read the annotation overlay, which a Source can rewrite.
    #[test]
    fn a_read_with_any_argument_is_not_cached() {
        let space = space();
        let html = request(Verb::Source, "urn:repo:core:file:src/lib.rs")
            .with_arg("as", ArgRef::Inline(b"text/html".to_vec()));
        assert_eq!(space.thread_for(&html), None);
    }

    #[test]
    fn a_meta_or_a_write_is_not_cached() {
        let space = space();
        for verb in [Verb::Meta, Verb::Sink, Verb::Delete, Verb::Exists] {
            assert_eq!(
                space.thread_for(&request(verb, "urn:repo:core:file:a.txt")),
                None,
                "{verb:?}"
            );
        }
    }

    #[test]
    fn a_root_name_that_would_read_as_another_family_is_refused() {
        assert!(check_root_name("core").is_ok());
        assert!(check_root_name("pr").is_err(), "ikigai-repo's own segment");
        assert!(check_root_name("style").is_err(), "browse's own row");
        assert!(check_root_name("a:b").is_err(), "would forge an IRI");
        assert!(check_root_name("").is_err());
    }
}
