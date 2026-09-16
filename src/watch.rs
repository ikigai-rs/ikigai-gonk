//! The watch over the browse roots: what makes a cached filesystem read safe to serve.
//!
//! # Why a server needs this and a library cannot have it
//!
//! `ikigai-browse` declares its browsing reads **live and uncacheable** — `tree`, `file`,
//! `hash`, `state` return a bare `Representation`, so the kernel recomputes each one on every
//! resolution. That is the only honest declaration a LIBRARY can make: cacheability is a
//! promise that something will notice when the answer changes, and a library linked into an
//! unknown host cannot know whether anything is watching the disk.
//!
//! A server can know, because a server is where the watcher lives. So gonk runs one
//! ([`RootWatch`]), and [`crate::browse::cached_reads`] declares the reads of a **watched**
//! root cacheable under the thread this module cuts. The two halves are written to fail
//! closed together: a root whose platform watcher does not start is not in the watched set,
//! and its reads keep browse's own uncacheable declaration. There is no path on which a read
//! is cached and unwatched.
//!
//! This is the live bug ledger #246 named, taken at the other end. There, `urn:file:*` was
//! `.cacheable().depends_on(target)` on a kernel with no watcher, so a replaced stylesheet
//! was served stale until the unit restarted — a declared thread nothing cut. Here nothing is
//! declared until something cuts it.
//!
//! # The shape
//!
//! Reimplemented from the design in closed `ikigai-cli` PR #321 (`WorkspaceWatch`:
//! `start` sets up the platform watch, `apply` cuts what a change names, `run` drives it for
//! the process's lifetime), which is in a workspace this crate cannot depend on. Two
//! differences, both from the setting rather than from taste:
//!
//! - **Many roots, one channel.** gonk serves a set of named roots, so `start` takes them
//!   all and reports which ones it got, and the path→thread map is "which root contains this
//!   path" rather than one root-relative rewrite.
//! - **One thread per ROOT, not per file.** A change anywhere under `core` cuts
//!   `urn:iki:gonk:browse:root:core`, and every cached read of that root recomputes. Coarse
//!   on purpose: the finer thread would have to be the file's own IRI, and building that from
//!   a filesystem path means `ikigai-browse`'s percent-encoding, which is private to that
//!   crate (`iri_encode`). A thread computed one way at the read and another way at the cut
//!   is a cache that looks fresh and is not — strictly worse than invalidating too much.
//!
//! ⚠ **`.git` is watched like everything else.** `urn:repo:{root}:state` is `git` output, so
//! its input IS the index and the refs; ignoring `.git` would make exactly that resource go
//! stale after a commit. The cost is that any git command in a root invalidates that root's
//! cached reads, which is correct and merely unhelpful.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

use ikigai_core::Kernel;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// The golden thread every cached read of `root` depends on, and the only thing
/// [`RootWatch`] ever cuts.
pub fn root_thread(root: &str) -> String {
    format!("urn:iki:gonk:browse:root:{root}")
}

/// A root that is being watched: its name, and the CANONICAL directory (symlinks resolved —
/// macOS reports `/private/var` where the config said `/var`, and a prefix test against the
/// un-canonicalized path would match nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// The root's name, as it appears in `urn:repo:{name}:…`.
    pub name: String,
    /// The canonical directory.
    pub dir: PathBuf,
}

/// A root the platform watcher refused, and why — reported at startup, never fatal.
#[derive(Debug, Clone)]
pub struct Unwatched {
    /// The root's name.
    pub name: String,
    /// What the platform said.
    pub reason: String,
}

/// A live watch over a set of browse roots.
///
/// Split from the thread that drives it ([`Self::spawn`]) so a test can drive it instead:
/// [`Self::apply_next`] blocks on the platform's own notification and applies the cut, which
/// is the only way to assert "the read recomputes after the change" without a sleep that
/// passes by luck on a fast machine and fails by luck on a slow one.
pub struct RootWatch {
    watched: Vec<Watched>,
    events: Receiver<notify::Result<notify::Event>>,
    /// Held for the watch's lifetime: dropping these ends the platform watches and closes
    /// `events`. One per root — `notify` will watch several paths on one watcher, but a
    /// per-root watcher is what makes one root's failure leave the others watched.
    _watchers: Vec<RecommendedWatcher>,
}

impl RootWatch {
    /// Start watching `roots`, recursively. Returns the watch and the roots it did NOT get;
    /// [`Self::watched`] is the set whose reads may be cached.
    pub fn start(roots: &[(String, PathBuf)]) -> (RootWatch, Vec<Unwatched>) {
        let (tx, events) = std::sync::mpsc::channel();
        let (mut watched, mut refused, mut watchers) = (Vec::new(), Vec::new(), Vec::new());
        for (name, dir) in roots {
            let dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            let tx = tx.clone();
            let started = notify::recommended_watcher(move |res| {
                let _ = tx.send(res);
            })
            .and_then(|mut watcher| {
                watcher.watch(&dir, RecursiveMode::Recursive)?;
                Ok(watcher)
            });
            match started {
                Ok(watcher) => {
                    watchers.push(watcher);
                    watched.push(Watched {
                        name: name.clone(),
                        dir,
                    });
                }
                Err(e) => refused.push(Unwatched {
                    name: name.clone(),
                    reason: e.to_string(),
                }),
            }
        }
        (
            RootWatch {
                watched,
                events,
                _watchers: watchers,
            },
            refused,
        )
    }

    /// The roots this watch actually holds — the set whose reads may be declared cacheable.
    pub fn watched(&self) -> &[Watched] {
        &self.watched
    }

    /// Cut the thread of every root a change names. Returns the threads cut — empty for an
    /// access (a read changes no content), a path under no watched root, or a watcher error
    /// (which names no path).
    pub fn apply(&self, kernel: &Kernel, event: notify::Result<notify::Event>) -> Vec<String> {
        let Ok(event) = event else {
            return Vec::new();
        };
        if event.kind.is_access() {
            return Vec::new();
        }
        let mut cut: Vec<String> = Vec::new();
        for path in &event.paths {
            for root in self.roots_containing(path) {
                let thread = root_thread(&root);
                if !cut.contains(&thread) {
                    kernel.cut(thread.as_str());
                    cut.push(thread);
                }
            }
        }
        cut
    }

    /// Wait up to `timeout` for the next notification and apply it. `None` when nothing
    /// arrived in time or the watch has ended; otherwise what [`Self::apply`] cut.
    ///
    /// Public, not `#[cfg(test)]`: gonk's watch test is an integration test, which is a
    /// separate crate and cannot see a test-gated item. The seam is the same one
    /// `ikigai-browse`'s own `StyleWatch` publishes, for the same reason.
    pub fn apply_next(&self, kernel: &Kernel, timeout: Duration) -> Option<Vec<String>> {
        self.events
            .recv_timeout(timeout)
            .ok()
            .map(|event| self.apply(kernel, event))
    }

    /// Drive the watch until its channel closes — in practice, for the process's lifetime.
    pub fn run(self, kernel: Arc<Kernel>) {
        while let Ok(event) = self.events.recv() {
            self.apply(&kernel, event);
        }
    }

    /// [`Self::run`] on its own thread.
    pub fn spawn(self, kernel: Arc<Kernel>) {
        std::thread::spawn(move || self.run(kernel));
    }

    /// Which watched roots contain `path`. Not `find`: roots may nest (a repo inside a
    /// workspace root is a legal configuration), and a change in the inner one changes both.
    fn roots_containing(&self, path: &Path) -> Vec<String> {
        self.watched
            .iter()
            .filter(|root| path.starts_with(&root.dir))
            .map(|root| root.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_that_cannot_be_watched_is_reported_and_left_out() {
        let missing = std::env::temp_dir().join("ikigai-gonk-no-such-root-9f3a");
        let _ = std::fs::remove_dir_all(&missing);
        let (watch, refused) = RootWatch::start(&[("ghost".to_string(), missing)]);
        assert!(watch.watched().is_empty(), "nothing is watched");
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].name, "ghost");
    }

    #[test]
    fn a_path_under_two_nested_roots_names_both() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let outer = dir.path().to_path_buf();
        let inner = outer.join("inner");
        std::fs::create_dir(&inner).expect("the inner root");
        let (watch, refused) = RootWatch::start(&[
            ("outer".to_string(), outer),
            ("inner".to_string(), inner.clone()),
        ]);
        assert!(refused.is_empty(), "{refused:?}");
        // Canonical, because that is the shape `notify` reports and the shape `start`
        // matches against — on macOS a temp dir is `/var/…`, which is a symlink to
        // `/private/var/…`, and a raw path would match no root at all.
        let inner = inner.canonicalize().expect("the inner root exists");
        let both = watch.roots_containing(&inner.join("file.txt"));
        assert_eq!(both, ["outer", "inner"], "a nested change names both roots");
    }

    #[test]
    fn the_thread_names_the_root() {
        assert_eq!(root_thread("core"), "urn:iki:gonk:browse:root:core");
    }
}
