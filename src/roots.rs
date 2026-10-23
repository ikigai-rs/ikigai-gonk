//! **The browse roots as live data** (ledger [#2154](http://localhost:1062/l/default/item/2154)):
//! a root added to or removed from `config.toml` mounts or unmounts while gonk runs, with no
//! restart, so no worker's connection and no review in flight is dropped for it.
//!
//! ```text
//! urn:iki:gonk:browse:roots            Source        the roots in force, the refused, and where they come from
//! urn:iki:gonk:browse:root:{name}      Source        one root's line of the same
//!                                      Sink          add it: content = its directory; writes the config line
//!                                      Delete        remove it: comments its lines out of config.toml
//! ```
//!
//! # The resource map, as built
//!
//! | name | kind | holds |
//! |---|---|---|
//! | `config.toml`'s `gonk.browse.root` lines | ATOM (a file) | the set of roots, as the operator wrote it |
//! | the arrangement in force ([`crate::browse::CachedReads`]) | derived state | browse's space, the watched set, the history and GitHub series, all built FROM the set |
//! | [`crate::browse::ROOTS_THREAD`] | thread | every answer of the family hangs from it; a re-arrangement cuts it |
//! | `urn:iki:gonk:browse:roots` | view | the set in force, the refused lines and why, the source |
//! | `urn:iki:gonk:browse:root:{name}` | view + the write door onto the atom | Sink appends the line, Delete comments it out; both re-read the atom |
//!
//! The one thing written as code that holds state is [`LiveRoots`]'s record of what is in
//! force, which it needs to know what a new reading REMOVED. Everything else is the file
//! (the atom) and what is rebuilt from it.
//!
//! # ★ Why not core's `DynamicSpace`
//!
//! `DynamicSpace` (ledger #2006) rebuilds its doors from a DECLARATION against a [`Registry`]
//! fixed when the space is made, by endpoint NAME. A new root is new endpoints: `ikigai-browse`
//! builds them for the roots it is handed (`Mount::new(roots)`), and none of them exists when the
//! registry is filled. So a declaration cannot name them, and the dynamic space cannot carry
//! this. What it is built on can: [`ikigai_core::Threaded`], the combinator under it, threads
//! every answer of a space from one thread, and the swap here cuts that thread, which is the
//! dynamic space's own COARSE cut. The missing piece in core is a way to install a space
//! ALREADY BUILT (`install_space(Arc<dyn Space>)`) with its status and its cut; reported to the
//! hub, not changed here.
//!
//! [`Registry`]: ikigai_core::Registry
//!
//! # When it is live, and when it says it is not
//!
//! - Roots from `config.toml`: live. The file is watched ([`LiveRoots::watch_config`]), and the
//!   owner's `ikigai-gonk browse add|remove` writes it and asks for the re-reading at once.
//! - Roots from `--browse-root` flags: fixed for the run. The flags override the file
//!   wholesale, so a line in it means nothing to this process; the status says so and every
//!   write is refused.
//! - No root at start: the browse family is not composed at all (`ikigai-repo`'s facades
//!   with it, see [`crate::compose_with`]), so there is nothing to re-arrange. The first root
//!   is written to the file and takes a restart; the status says so.
//!
//! # ★ A root that is not there is REFUSED, and the others keep serving
//!
//! At start a root whose directory does not exist stops gonk (`config::settings`), the shape of
//! a typo. While it runs, the same line is refused with its reason in the status, and every
//! other root goes on serving: an edit to one line never takes the family down.
//!
//! # Authority
//!
//! Every name here declares [`CAP_ROOTS`], which gonk mints for nobody: the socket door's
//! ROOT reaches it and no other door does. The status names directories, and a page never
//! shows one ([`crate::web::Web::browse_roots`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation,
    Kernel, ReprType, Representation, Result, UriTemplate, Verb,
};

use crate::browse::CachedReads;
use crate::watch::{Epochs, Held, RootWatch, Unwatched};

/// The set of roots in force, its refusals and its source.
pub const ROOTS_IRI: &str = "urn:iki:gonk:browse:roots";

/// One root: `urn:iki:gonk:browse:root:{name}`.
pub const ROOT_PREFIX: &str = "urn:iki:gonk:browse:root:";

/// What every name here declares. Minted for nobody: the socket door's root holds it.
pub const CAP_ROOTS: &str = "urn:cap:gonk:browse:roots";

/// The config key the roots are read from.
const KEY: &str = "gonk.browse.root";

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The NAMES of the roots in force, shared by every page that offers a root (the header, the
/// Browse and Queue pages, the judge backfill) and replaced by a re-arrangement.
///
/// ⚠ Names only, as [`crate::web::Web::browse_roots`] says: the paths are the mount's.
#[derive(Clone, Debug, Default)]
pub struct RootNames(Arc<RwLock<Vec<String>>>);

impl From<Vec<String>> for RootNames {
    fn from(names: Vec<String>) -> Self {
        RootNames(Arc::new(RwLock::new(names)))
    }
}

impl FromIterator<String> for RootNames {
    fn from_iter<I: IntoIterator<Item = String>>(names: I) -> Self {
        RootNames::from(names.into_iter().collect::<Vec<_>>())
    }
}

impl RootNames {
    /// The names now, in configured order.
    pub fn names(&self) -> Vec<String> {
        self.0.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Whether no root is in force.
    pub fn is_empty(&self) -> bool {
        self.0.read().unwrap_or_else(|p| p.into_inner()).is_empty()
    }

    /// Whether `root` is in force.
    pub fn contains(&self, root: &str) -> bool {
        self.0
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|r| r == root)
    }

    fn set(&self, names: Vec<String>) {
        *self.0.write().unwrap_or_else(|p| p.into_inner()) = names;
    }
}

/// What builds the family over a set of roots: [`crate::browse::wire`] with this process's
/// writer, graph, tiers and config home, and "On GitHub" with the overrides that apply.
/// `main` writes it once, from the same values it wired the first arrangement with.
pub type Build =
    Box<dyn Fn(&[(String, PathBuf)], &RootWatch, &[(String, String)]) -> CachedReads + Send + Sync>;

/// Where this process's roots came from, and so whether they can change.
pub enum Source {
    /// `config.toml`'s lines: live.
    Config {
        /// The file `serve` read (`--config`, else the config home's).
        path: PathBuf,
        /// `$HOME`, for `~/` in a line.
        home: PathBuf,
        /// The family, and how to build it again.
        live: Box<Live>,
    },
    /// `--browse-root` flags: fixed for the run.
    Flags,
    /// No root at start, so no family to re-arrange.
    NotComposed {
        /// The file a first root would be written to.
        path: PathBuf,
        /// `$HOME`, for `~/` in a line.
        home: PathBuf,
    },
}

/// The family in the hub and what rebuilds it.
pub struct Live {
    /// The overlay the hub holds; [`CachedReads::replace`] swaps what it serves.
    pub browse: Arc<CachedReads>,
    /// The badge's table, kept across arrangements.
    pub epochs: Arc<Epochs>,
    /// How to build the family over a new set.
    pub build: Build,
}

/// A line that did not mount, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    /// The line's value, as written.
    pub line: String,
    /// Why.
    pub reason: String,
}

#[derive(Default)]
struct State {
    in_force: Vec<(String, PathBuf)>,
    github: Vec<(String, String)>,
    refused: Vec<Refused>,
    unwatched: Vec<Unwatched>,
    /// The watch over the roots in force; dropping it stops it.
    held: Option<Held>,
    /// How many re-arrangements since start.
    changes: u64,
    /// The last time the file could not be read, when the last reading failed.
    unreadable: Option<String>,
}

/// **The live set of browse roots** — see the module docs.
pub struct LiveRoots {
    source: Source,
    names: RootNames,
    state: Mutex<State>,
    hub: OnceLock<Weak<Kernel>>,
}

impl LiveRoots {
    /// The set `serve` started with: `roots` mounted and watched by `held` (when live), with
    /// the GitHub overrides `github` and the watch's refusals `unwatched`.
    pub fn new(
        source: Source,
        roots: Vec<(String, PathBuf)>,
        github: Vec<(String, String)>,
        unwatched: Vec<Unwatched>,
        held: Option<Held>,
    ) -> Arc<LiveRoots> {
        let names = RootNames::from(roots.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>());
        Arc::new(LiveRoots {
            source,
            names,
            state: Mutex::new(State {
                in_force: roots,
                github,
                unwatched,
                held,
                ..State::default()
            }),
            hub: OnceLock::new(),
        })
    }

    /// Whether the roots can change while this process runs ([`Source::Config`]).
    pub fn is_live(&self) -> bool {
        matches!(self.source, Source::Config { .. })
    }

    /// Hold the watch `serve` started over the first roots, so the first re-arrangement
    /// retires it.
    pub fn hold(&self, held: Held) {
        self.lock().held = Some(held);
    }

    /// The names in force, for the pages.
    pub fn names(&self) -> RootNames {
        self.names.clone()
    }

    /// Hand it the hub it is bound in. Called once, after the hub is built.
    pub fn attach(&self, hub: &Arc<Kernel>) {
        let _ = self.hub.set(Arc::downgrade(hub));
    }

    /// The resources, for the hub.
    pub fn space(self: &Arc<Self>) -> EndpointSpace {
        EndpointSpace::new()
            .bind(
                Exact::new(ROOTS_IRI),
                RootsEndpoint {
                    live: Arc::clone(self),
                },
            )
            .bind(
                UriTemplate::parse("urn:iki:gonk:browse:root:{name}").expect("a constant template"),
                RootEndpoint {
                    live: Arc::clone(self),
                },
            )
            .named(crate::spaces::iri(crate::spaces::ROOTS))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// **Read the file again and re-arrange the family to match.** Idempotent: a reading that
    /// changes nothing cuts nothing. Answers the status text.
    ///
    /// # Errors
    ///
    /// When the roots are not live ([`Source::Flags`], [`Source::NotComposed`]), or no hub is
    /// attached. A line that does not mount is not an error: it is refused in the status.
    pub fn reload(&self) -> std::result::Result<String, String> {
        let (path, home, live) = match &self.source {
            Source::Config { path, home, live } => (path, home, live),
            _ => return Err(self.not_live()),
        };
        let hub = self
            .hub
            .get()
            .and_then(Weak::upgrade)
            .ok_or("no hub is attached yet, so nothing can be mounted")?;
        let mut state = self.lock();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                // Keep what is in force: an unreadable file is not an empty one.
                state.unreadable = Some(format!("reading {}: {e}", path.display()));
                return Ok(self.status_of(&state));
            }
        };
        state.unreadable = None;
        let reading = read_roots(&text, home);
        let github = read_github(&text, &reading.roots);
        state.refused = reading.refused;
        if reading.roots == state.in_force && github == state.github {
            return Ok(self.status_of(&state));
        }
        // The new watch first, then the swap and its cut, then the old watch retired: no
        // moment exists at which a cached read of a root in force is unwatched.
        let (watch, unwatched) =
            RootWatch::start_counting(&reading.roots, Some(Arc::clone(&live.epochs)));
        let next = (live.build)(&reading.roots, &watch, &github);
        let removed: Vec<String> = state
            .in_force
            .iter()
            .filter(|(name, _)| !reading.roots.iter().any(|(n, _)| n == name))
            .map(|(name, _)| name.clone())
            .collect();
        live.browse.replace(&hub, &next, &removed);
        self.names
            .set(reading.roots.iter().map(|(n, _)| n.clone()).collect());
        state.held = Some(watch.spawn_held(Arc::clone(&hub)));
        state.in_force = reading.roots;
        state.github = github;
        state.unwatched = unwatched;
        state.changes += 1;
        Ok(self.status_of(&state))
    }

    /// **Add a root**: write its `gonk.browse.root` line (append-only, a backup beside the
    /// file, [`crate::checkout::append_roots`]) and re-read. Idempotent: the same name at the
    /// same directory writes nothing.
    ///
    /// # Errors
    ///
    /// A name a root cannot have, a directory that is not there, the name already configured
    /// for ANOTHER directory, a file that cannot be written, or roots that are not live.
    pub fn add(&self, name: &str, dir: &str) -> std::result::Result<String, String> {
        let (path, home) = match &self.source {
            Source::Config { path, home, .. } => (path, home),
            Source::NotComposed { path, home } => {
                write_root(path, home, name, dir)?;
                return Err(format!(
                    "wrote the line to {}, and it takes a restart: {}",
                    path.display(),
                    self.not_live()
                ));
            }
            Source::Flags => return Err(self.not_live()),
        };
        write_root(path, home, name, dir)?;
        self.reload()
    }

    /// **Remove a root**: comment its lines out of the file (its `gonk.browse.root` and any
    /// `gonk.browse.github` naming it, which would stop the next start otherwise), a backup
    /// beside it, and re-read. Idempotent: a name no line configures writes nothing.
    ///
    /// # Errors
    ///
    /// A file that cannot be written, or roots that are not live.
    pub fn remove(&self, name: &str) -> std::result::Result<String, String> {
        let path = match &self.source {
            Source::Config { path, .. } => path,
            _ => return Err(self.not_live()),
        };
        crate::checkout::comment_out_root(path, name)?;
        self.reload()
    }

    /// Watch the config file and re-read it on every change, on a thread of its own. The
    /// owner's command does not need this (it asks for the re-reading); an edit by hand does.
    ///
    /// The DIRECTORY is watched, not the file: `append_roots` and most editors replace the file
    /// by a rename, and a watch on the old inode would hear nothing after the first edit. A
    /// burst of events is settled for a moment before one reading, so a write in two steps is
    /// not read half-done.
    ///
    /// # Errors
    ///
    /// When the roots are not live, or the platform watcher refuses.
    pub fn watch_config(self: &Arc<Self>) -> std::result::Result<(), String> {
        use notify::{RecursiveMode, Watcher};
        let path = match &self.source {
            Source::Config { path, .. } => path.clone(),
            _ => return Err(self.not_live()),
        };
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("{} has no directory to watch", path.display()))?;
        let name = path.file_name().map(|n| n.to_os_string());
        let (tx, events) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if !event.kind.is_access()
                    && event.paths.iter().any(|p| p.file_name() == name.as_deref())
                {
                    let _ = tx.send(());
                }
            }
        })
        .map_err(|e| e.to_string())?;
        watcher
            .watch(&dir, RecursiveMode::NonRecursive)
            .map_err(|e| format!("watching {}: {e}", dir.display()))?;
        let live = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("gonk-browse-roots".to_string())
            .spawn(move || {
                let _watcher = watcher;
                while events.recv().is_ok() {
                    std::thread::sleep(Duration::from_millis(200));
                    while events.try_recv().is_ok() {}
                    let Some(live) = live.upgrade() else { return };
                    if let Err(e) = live.reload() {
                        eprintln!("ikigai-gonk: browse roots not re-read: {e}");
                    }
                }
            })
            .map(|_| ())
            .map_err(|e| format!("no thread for the config watch: {e}"))
    }

    fn not_live(&self) -> String {
        match &self.source {
            Source::Config { .. } => "the roots are live".to_string(),
            Source::Flags => "this gonk's roots came from --browse-root flags, which override \
                              config.toml for the whole run, so they change only with a restart"
                .to_string(),
            Source::NotComposed { .. } => "this gonk started with no gonk.browse.root, so the \
                                           browse family is not composed and has nothing to \
                                           re-arrange; restart once to compose it"
                .to_string(),
        }
    }

    /// The status text, now.
    pub fn status(&self) -> String {
        self.status_of(&self.lock())
    }

    fn status_of(&self, state: &State) -> String {
        let mut out = match &self.source {
            Source::Config { path, .. } => format!(
                "browse roots from {} (live: an edit mounts or unmounts with no restart)\n",
                path.display()
            ),
            Source::Flags => "browse roots from --browse-root flags (fixed for this run)\n".into(),
            Source::NotComposed { path, .. } => format!(
                "no browse root at start, so the family is not composed (a line in {} takes a \
                 restart)\n",
                path.display()
            ),
        };
        for (name, dir) in &state.in_force {
            let watch = match state.unwatched.iter().find(|u| &u.name == name) {
                Some(u) => format!("served live, not watched ({})", u.reason),
                None => "watched".to_string(),
            };
            out.push_str(&format!("  {name}\t{}\t{watch}\n", dir.display()));
        }
        if state.in_force.is_empty() {
            out.push_str("  (none in force)\n");
        }
        for refused in &state.refused {
            out.push_str(&format!(
                "  refused\t{}\t{}\n",
                refused.line, refused.reason
            ));
        }
        if let Some(why) = &state.unreadable {
            out.push_str(&format!(
                "  ⚠ the last reading failed, the roots above stay in force: {why}\n"
            ));
        }
        out.push_str(&format!(
            "{} re-arrangement(s) since start\n",
            state.changes
        ));
        out
    }

    /// One root's line: in force, refused, or not configured.
    fn root_status(&self, name: &str) -> String {
        let state = self.lock();
        if let Some((_, dir)) = state.in_force.iter().find(|(n, _)| n == name) {
            return format!("{name}\t{}\tin force\n", dir.display());
        }
        if let Some(r) = state
            .refused
            .iter()
            .find(|r| r.line.split('=').next().map(str::trim) == Some(name))
        {
            return format!("{name}\trefused\t{}\n", r.reason);
        }
        format!("{name}\tnot configured\n")
    }
}

/// One reading of the file.
struct Reading {
    roots: Vec<(String, PathBuf)>,
    refused: Vec<Refused>,
}

/// The `gonk.browse.root` lines of `text`, each mounted or refused — the same checks
/// `config::settings` makes at start, but per line: one bad line is refused and the rest
/// mount.
fn read_roots(text: &str, home: &Path) -> Reading {
    let mut reading = Reading {
        roots: Vec::new(),
        refused: Vec::new(),
    };
    for line in crate::config::values_for(text, KEY) {
        let refuse = |reason: String| Refused {
            line: line.clone(),
            reason,
        };
        let Some((name, path)) = line.split_once('=') else {
            reading
                .refused
                .push(refuse("expected <name>=<path>".to_string()));
            continue;
        };
        let (name, path) = (name.trim(), path.trim());
        if let Err(why) = crate::browse::check_root_name(name) {
            reading.refused.push(refuse(why));
            continue;
        }
        if reading.roots.iter().any(|(seen, _)| seen == name) {
            reading.refused.push(refuse(format!(
                "`{name}` is named twice; the first line is in force"
            )));
            continue;
        }
        let dir = crate::config::expand_home(path, home);
        if !dir.is_dir() {
            reading
                .refused
                .push(refuse(format!("{} is not a directory", dir.display())));
            continue;
        }
        reading.roots.push((name.to_string(), dir));
    }
    reading
}

/// The `gonk.browse.github` lines that apply to `roots`. A line that does not is left for the
/// next start to refuse; it changes no link while this process runs.
fn read_github(text: &str, roots: &[(String, PathBuf)]) -> Vec<(String, String)> {
    let mut overrides: Vec<(String, String)> = Vec::new();
    for line in crate::config::values_for(text, crate::github::OVERRIDE_KEY) {
        let Some((root, value)) = line.split_once('=') else {
            continue;
        };
        let root = root.trim();
        if !roots.iter().any(|(name, _)| name == root)
            || overrides.iter().any(|(seen, _)| seen == root)
        {
            continue;
        }
        if let Some(repository) = crate::github::override_repository(value.trim()) {
            overrides.push((root.to_string(), repository));
        }
    }
    overrides
}

/// A root name the owner's command may WRITE: stricter than browse's own rule
/// ([`crate::browse::check_root_name`]), because the name is spliced into a quoted config line
/// and split back out at its first `=`.
fn check_new_name(name: &str) -> std::result::Result<(), String> {
    crate::browse::check_root_name(name)?;
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "browse root `{name}`: a new root's name is letters, digits, `-`, `_` and `.`"
        ));
    }
    Ok(())
}

/// Write `name=dir` into `config` — the half of [`LiveRoots::add`] that needs no server, and
/// what `ikigai-gonk browse add` does itself when no gonk is running.
///
/// # Errors
///
/// As [`LiveRoots::add`].
pub fn write_root(
    config: &Path,
    home: &Path,
    name: &str,
    dir: &str,
) -> std::result::Result<crate::checkout::ConfigLine, String> {
    check_new_name(name)?;
    let dir = dir.trim();
    if dir.is_empty() || dir.contains(['"', '\\', '\n', '\r', '#']) {
        return Err(format!(
            "browse root `{name}`: the directory must be a plain path (no quote, backslash, \
             `#` or newline), got `{dir}`"
        ));
    }
    if !crate::config::expand_home(dir, home).is_dir() {
        return Err(format!(
            "browse root `{name}`: {} is not a directory",
            crate::config::expand_home(dir, home).display()
        ));
    }
    let (results, _backup) =
        crate::checkout::append_roots(config, &[(name.to_string(), dir.to_string())], home)?;
    match results.into_iter().next() {
        Some(crate::checkout::ConfigLine::Conflict(at)) => Err(format!(
            "browse root `{name}` is already configured for {at}; remove it first"
        )),
        Some(line) => Ok(line),
        None => Ok(crate::checkout::ConfigLine::Present),
    }
}

/// The text answer every name here gives.
fn text(body: String) -> Representation {
    Representation::new(ReprType::new("text/plain;charset=utf-8"), body.into_bytes())
}

/// The floor every name here declares, checked again at the door (it is declared, so the
/// kernel has refused already; this is the belt to that).
fn owner(inv: &Invocation<'_>) -> Result<()> {
    if inv.capability.allows(CAP_ROOTS) {
        Ok(())
    } else {
        Err(Error::Denied(format!(
            "the browse roots are the owner's: {CAP_ROOTS}"
        )))
    }
}

/// `urn:iki:gonk:browse:roots`.
struct RootsEndpoint {
    live: Arc<LiveRoots>,
}

#[async_trait]
impl Endpoint for RootsEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        owner(inv)?;
        Ok(text(self.live.status()))
    }

    fn name(&self) -> &str {
        "gonk-browse-roots-live"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-browse-roots-live")
            .title("The browse roots in force")
            .summary(
                "The roots the browse family serves now, one per line with its directory and \
                 whether it is watched; each config line that did not mount, with its reason; \
                 where the roots come from, and whether they can change while gonk runs. Live \
                 state: never cached.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_ROOTS)
            .output("text/plain;charset=utf-8")
    }
}

/// `urn:iki:gonk:browse:root:{name}`.
struct RootEndpoint {
    live: Arc<LiveRoots>,
}

#[async_trait]
impl Endpoint for RootEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        owner(inv)?;
        let name = inv
            .request
            .target
            .as_str()
            .strip_prefix(ROOT_PREFIX)
            .unwrap_or_default()
            .to_string();
        if name.is_empty() {
            return Err(Error::NotFound(inv.request.target.to_string()));
        }
        let refused = Error::Endpoint;
        match inv.request.verb {
            Verb::Sink => {
                let dir = match inv.request.args.get("content") {
                    Some(ArgRef::Inline(bytes)) => String::from_utf8(bytes.clone())
                        .map_err(|_| refused("the directory is not UTF-8 text".to_string()))?,
                    _ => {
                        return Err(refused(
                            "add a root with content=<its directory>".to_string(),
                        ))
                    }
                };
                // A file, a platform watch and a mount: quick and bounded, and every caller is
                // the owner, so it runs here rather than on a thread of its own.
                self.live.add(&name, &dir).map(text).map_err(refused)
            }
            Verb::Delete => self.live.remove(&name).map(text).map_err(refused),
            _ => Ok(text(self.live.root_status(&name))),
        }
    }

    fn name(&self) -> &str {
        "gonk-browse-root"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-browse-root")
            .title("One browse root")
            .summary(
                "One root of the browse family, by name. Source: whether it is in force, \
                 refused (and why), or not configured. Sink: add it, content = its directory \
                 (`~/` allowed); writes its gonk.browse.root line to config.toml (a backup \
                 beside it) and mounts it with no restart; the same name at the same \
                 directory writes nothing. Delete: comment its lines out of config.toml and \
                 unmount it; every answer it served is cut.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("this root's status")
                    .requires(CAP_ROOTS)
                    .output("text/plain;charset=utf-8"),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("add this root and mount it")
                    .requires(CAP_ROOTS)
                    .input(
                        ArgSpec::new("content")
                            .class(XSD_STRING)
                            .summary("the root's directory; `~/` is the home directory"),
                    )
                    .output("text/plain;charset=utf-8"),
            )
            .action(
                ActionSpec::new(Verb::Delete)
                    .summary("remove this root and unmount it")
                    .requires(CAP_ROOTS)
                    .output("text/plain;charset=utf-8"),
            )
            .verb(Verb::Meta)
    }
}
