//! `ikigai-gonk checkout <git-url>…` — clone or update the repositories gonk browses.
//!
//! A browse root has to exist on disk before gonk starts, or gonk refuses to start (a missing
//! directory is the shape of a typo, see `config::browse_roots`). On a new machine that means
//! cloning every repository by hand and writing one `gonk.browse.root` line per clone. This
//! command does both halves, and nothing else:
//!
//! - each URL is cloned into a MANAGED directory, `~/.ikigai/checkouts/<name>` unless `--dir`
//!   says otherwise, where `<name>` is the URL's last path segment without `.git`, or what
//!   `name=url` says;
//! - a URL already cloned there is fetched and its default branch FAST-FORWARDED — and nothing
//!   more: a checkout with local changes, on another branch, or with commits of its own is
//!   REFUSED with the reason, never reset, stashed, rebased or discarded;
//! - each root's `gonk.browse.root = "<name>=<path>"` line is printed, and with
//!   `--write-config` the missing ones are appended to the config home's `config.toml`.
//!
//! `checkout --all` is the same update without the URL list: every checkout already under the
//! managed directory is fetched and fast-forwarded from its own `origin`, with the same
//! refusals, and the run ends with how the checkouts and the config's roots cover each other
//! (a checkout no root uses, a root there with no checkout). It clones nothing and writes no
//! config, which is what makes it safe to put on a timer.
//!
//! It is a COMMAND, like `review request` and `roborev file`: it opens no store, binds no door,
//! and the server gains no network code. gonk reads its roots at startup, so a new root needs
//! a restart, and the command says so. A root that is already configured needs nothing: gonk
//! watches it, and the watcher sees the fast-forward like any other change on disk.
//!
//! # git, without prompts
//!
//! Every git call runs the `git` binary with stdin closed and `GIT_TERMINAL_PROMPT=0`, so an
//! HTTPS remote that wants a password fails instead of waiting for one. For SSH, `BatchMode`
//! does the same, set through `GIT_SSH_COMMAND` only when neither that variable nor
//! `core.sshCommand` already says how to run ssh (overriding either would throw away the
//! operator's own key or wrapper). The failure is reported with git's own message.
//!
//! ⚠ A managed clone shows its DEFAULT BRANCH. Work happening in other worktrees or branches
//! (a kata-flight loop's, say) is not what gonk browses there.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Homes;

/// The managed directory's name under the data home.
pub const CHECKOUTS_DIR: &str = "checkouts";

/// Everything `ikigai-gonk checkout` was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The repositories, each with the root name it will be served under.
    pub repos: Vec<Repo>,
    /// `--dir`: the managed directory, instead of `~/.ikigai/checkouts`.
    pub dir: Option<PathBuf>,
    /// `--write-config`: append the missing root lines to the config file.
    pub write_config: bool,
    /// `--config`: the config file to append to, instead of `<config home>/config.toml`.
    /// Under `--all`, the config file whose roots the coverage report reads.
    pub config: Option<PathBuf>,
    /// `--all`: update every checkout already under the managed directory instead of the
    /// repositories named here (which must then be none).
    pub all: bool,
}

/// One repository to check out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The browse root name, and the directory's name under the managed directory.
    pub name: String,
    /// The git URL, as given.
    pub url: String,
}

/// Parse the arguments after `checkout`.
pub fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args {
        repos: Vec::new(),
        dir: None,
        write_config: false,
        config: None,
        all: false,
    };
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| format!("checkout: {flag} needs a value"))
        };
        match arg.as_str() {
            "--dir" => out.dir = Some(PathBuf::from(value("--dir")?)),
            "--config" => out.config = Some(PathBuf::from(value("--config")?)),
            "--write-config" => out.write_config = true,
            "--all" => out.all = true,
            flag if flag.starts_with('-') => {
                return Err(format!("checkout: unknown argument `{flag}`"))
            }
            spec => {
                let repo = repo(spec)?;
                if out.repos.iter().any(|seen| seen.name == repo.name) {
                    return Err(format!(
                        "checkout: two repositories would be named `{}` — one name, one \
                         directory. Name one of them with `<name>=<url>`",
                        repo.name
                    ));
                }
                out.repos.push(repo);
            }
        }
    }
    if out.all {
        if !out.repos.is_empty() {
            return Err(
                "checkout: --all takes its URLs from each checkout's own `origin`; give \
                 either --all or URLs, not both"
                    .into(),
            );
        }
        if out.write_config {
            return Err(
                "checkout: --all never writes config (it adds no root); run `checkout <url>… \
                 --write-config` for the repositories whose lines are missing"
                    .into(),
            );
        }
    } else if out.repos.is_empty() {
        return Err(
            "checkout: expected at least one <git-url> (or <name>=<git-url>), or --all to \
             update every checkout already under the managed directory"
                .into(),
        );
    }
    Ok(out)
}

/// One positional argument: `<url>` or `<name>=<url>`.
///
/// A `=` is a name only when what precedes it could not be part of a URL, so a query string
/// in an HTTPS URL is never read as a name.
fn repo(spec: &str) -> Result<Repo, String> {
    let (name, url) = match spec.split_once('=') {
        Some((name, url)) if !name.contains(['/', ':', '@', '?']) => {
            (name.to_string(), url.to_string())
        }
        _ => {
            let name = name_from_url(spec).ok_or_else(|| {
                format!("checkout: no repository name in `{spec}` — name it with `<name>={spec}`")
            })?;
            (name, spec.to_string())
        }
    };
    if url.is_empty() {
        return Err(format!("checkout: `{spec}` names no URL"));
    }
    crate::browse::check_root_name(&name).map_err(|e| format!("checkout: {e}"))?;
    if name == "." || name == ".." || name.starts_with('-') || name.contains(char::is_whitespace) {
        return Err(format!(
            "checkout: `{name}` cannot be a directory name here — name the repository with \
             `<name>=<url>`"
        ));
    }
    Ok(Repo { name, url })
}

/// The repository name a git URL implies: its last path segment, without `.git`.
///
/// ```
/// use ikigai_gonk::checkout::name_from_url;
///
/// assert_eq!(name_from_url("https://github.com/kenn-io/kata.git").as_deref(), Some("kata"));
/// assert_eq!(name_from_url("git@github.com:ikigai-rs/ikigai-gonk").as_deref(), Some("ikigai-gonk"));
/// assert_eq!(name_from_url("file:///srv/git/demo.git/").as_deref(), Some("demo"));
/// assert_eq!(name_from_url("https://example.com/"), None);
/// ```
pub fn name_from_url(url: &str) -> Option<String> {
    let path = match url.split_once("://") {
        // The path after the host; a URL that is only a host names no repository.
        Some((_, rest)) => rest.split_once('/').map(|(_, path)| path)?,
        // scp-like `user@host:path`, or a local path.
        None => match url.split_once(':') {
            Some((host, path)) if !host.contains('/') => path,
            _ => url,
        },
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let last = path.trim_end_matches('/').rsplit('/').next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty()).then(|| name.to_string())
}

/// What happened to one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Cloned now, at this commit.
    Cloned {
        /// The short commit checked out.
        head: String,
    },
    /// Fast-forwarded from one commit to another.
    Updated {
        /// Where it was.
        from: String,
        /// Where it is.
        to: String,
        /// How many commits that is.
        commits: u64,
    },
    /// Already at the default branch's tip.
    UpToDate {
        /// The short commit.
        head: String,
    },
    /// Left exactly as it was, for this reason.
    Refused(String),
    /// git could not do it (a clone or a fetch failed: auth, network, a wrong URL).
    Failed(String),
}

impl Outcome {
    /// Whether the checkout is usable as a browse root after this run.
    pub fn is_root(&self) -> bool {
        !matches!(self, Outcome::Refused(_) | Outcome::Failed(_))
    }
}

/// What appending one root line did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLine {
    /// Appended.
    Added,
    /// The same name already points at the same directory.
    Present,
    /// The same name points somewhere else; that line was left alone and nothing added.
    Conflict(String),
}

/// Run `checkout`: clone or update every repository, print the root lines, and with
/// `--write-config` append the missing ones. Answers whether every repository came out
/// usable — a refusal is reported and the rest still run.
pub fn run(args: &Args, homes: &Homes, out: &mut impl Write) -> Result<bool, String> {
    let dir = args
        .dir
        .clone()
        .unwrap_or_else(|| homes.data.join(CHECKOUTS_DIR));
    if args.all {
        return run_all(&dir, args.config.as_deref(), homes, out);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let git = Git::new();
    let mut roots = Vec::new();
    let mut all_ok = true;
    for repo in &args.repos {
        let path = dir.join(&repo.name);
        let outcome = checkout(&git, repo, &path);
        let shown = display(&path, &homes.home);
        report(out, &repo.name, &shown, &outcome);
        if outcome.is_root() {
            roots.push((repo.name.clone(), shown));
        } else {
            all_ok = false;
        }
    }
    if roots.is_empty() {
        return Ok(all_ok);
    }
    let _ = writeln!(out, "\nbrowse roots:");
    for (name, path) in &roots {
        let _ = writeln!(out, "  {}", root_line(name, path));
    }
    let config = args
        .config
        .clone()
        .unwrap_or_else(|| homes.config.join("config.toml"));
    if !args.write_config {
        let _ = writeln!(
            out,
            "\nnot written: add the lines above to {}, or run again with --write-config",
            config.display()
        );
        return Ok(all_ok);
    }
    let (results, backup) = append_roots(&config, &roots, &homes.home)?;
    let _ = writeln!(out, "\n{}:", config.display());
    let mut added = false;
    for ((name, path), result) in roots.iter().zip(&results) {
        let _ = match result {
            ConfigLine::Added => {
                added = true;
                writeln!(out, "  added      {}", root_line(name, path))
            }
            ConfigLine::Present => writeln!(out, "  present    {}", root_line(name, path)),
            ConfigLine::Conflict(existing) => {
                all_ok = false;
                writeln!(
                    out,
                    "  CONFLICT   `{name}` is already a root at {existing}; that line was left \
                     alone and none was added. Remove it, or check this one out under \
                     another name (`<name>=<url>`)"
                )
            }
        };
    }
    if let Some(backup) = backup {
        let _ = writeln!(out, "  backup     {}", backup.display());
    }
    if added {
        // `kickstart -k`, not a re-registration: only config.toml changed, which gonk reads at
        // startup, and gonk holds no TCC grant a fresh registration would re-attribute. A
        // replaced BINARY is what needs bootout + bootstrap (README, "Why `kickstart`").
        let _ = writeln!(
            out,
            "\ngonk reads gonk.browse.root at startup only: restart it to serve the new \
             root(s). Under launchd:\n  launchctl kickstart -k gui/$(id -u)/dev.ikigai-rs.gonk"
        );
    }
    Ok(all_ok)
}

/// One repository's line (and, for a refusal or a failure, its reason on the next).
fn report(out: &mut impl Write, name: &str, shown: &str, outcome: &Outcome) {
    let _ = match outcome {
        Outcome::Cloned { head } => writeln!(out, "cloned     {name} at {head}  {shown}"),
        Outcome::Updated { from, to, commits } => writeln!(
            out,
            "updated    {name} {from}..{to} ({commits} commit(s), fast-forward)  {shown}"
        ),
        Outcome::UpToDate { head } => writeln!(out, "current    {name} at {head}  {shown}"),
        Outcome::Refused(why) => writeln!(out, "REFUSED    {name}  {shown}\n  {why}"),
        Outcome::Failed(why) => writeln!(out, "FAILED     {name}  {shown}\n  {why}"),
    };
}

/// `checkout --all`: fetch and fast-forward every checkout already under `dir`, then report
/// how those checkouts and the config's roots cover each other.
///
/// Each subdirectory is one checkout: its name is the directory's, its URL is its own
/// `origin`, and it goes through exactly the update a named URL does, so the refusals are the
/// same. Nothing is cloned (a missing directory is reported, not created) and no config is
/// written. Entries whose names begin with `.` and entries that are not directories are
/// skipped; a directory that is not a git checkout is REFUSED, like any other surprise in a
/// directory this command manages. The coverage lines are information: the exit status
/// answers only whether every checkout came out usable.
fn run_all(
    dir: &Path,
    config: Option<&Path>,
    homes: &Homes,
    out: &mut impl Write,
) -> Result<bool, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        format!(
            "checkout --all: reading {}: {e} — nothing has been checked out there yet \
             (`ikigai-gonk checkout <url>…` clones; --all only updates)",
            dir.display()
        )
    })?;
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("checkout --all: reading {}: {e}", dir.display()))?;
        // Followed, so a symlink to a checkout counts as one; `update` then insists the
        // directory IS a checkout's top level.
        if !entry.path().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            let _ = writeln!(
                out,
                "skipped    {}  (not a UTF-8 name, so not a root name)",
                entry.path().display()
            );
            continue;
        };
        if !name.starts_with('.') {
            names.push(name);
        }
    }
    names.sort();
    let git = Git::new();
    let mut all_ok = true;
    let mut checkouts = Vec::new();
    for name in &names {
        let path = dir.join(name);
        let shown = display(&path, &homes.home);
        // The URL is the checkout's own origin, so `update`'s same-repository check holds by
        // construction; when there is none, `update` itself refuses (after first asking
        // whether this is a checkout at all, which a directory inside another one is not).
        let repo = Repo {
            name: name.clone(),
            url: git
                .run(Some(&path), &["remote", "get-url", "origin"])
                .unwrap_or_default(),
        };
        // `update`, never `checkout`: the directory exists, and --all never clones.
        let outcome = match update(&git, &repo, &path) {
            Ok(outcome) => outcome,
            Err(Stop::Refuse(why)) => Outcome::Refused(why),
            Err(Stop::Fail(why)) => Outcome::Failed(why),
        };
        report(out, name, &shown, &outcome);
        all_ok &= outcome.is_root();
        checkouts.push((name.clone(), path));
    }
    if names.is_empty() {
        let _ = writeln!(out, "no checkouts under {}", display(dir, &homes.home));
    }
    let config = config
        .map(Path::to_path_buf)
        .unwrap_or_else(|| homes.config.join("config.toml"));
    let roots = match std::fs::read_to_string(&config) {
        Ok(text) => configured_roots(&text, &homes.home),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("reading {}: {e}", config.display())),
    };
    let coverage = coverage(dir, &checkouts, &roots);
    let _ = writeln!(
        out,
        "\ncoverage of {} by the roots in {}:",
        display(dir, &homes.home),
        config.display()
    );
    if coverage.is_empty() {
        let _ = writeln!(
            out,
            "  complete   every checkout here is a browse root, and every root here has a checkout"
        );
    }
    for gap in &coverage {
        let _ = match gap {
            Gap::Unused { name, path } => writeln!(
                out,
                "  unused     {name}  {}  no gonk.browse.root points into it: gonk does not \
                 browse it (`checkout <url> --write-config` adds its line)",
                display(path, &homes.home)
            ),
            Gap::Missing { name, path } => writeln!(
                out,
                "  missing    {name}  {}  root `{name}` points here and nothing is checked \
                 out: gonk refuses to start until it exists (`checkout <url>` clones it)",
                display(path, &homes.home)
            ),
        };
    }
    Ok(all_ok)
}

/// The `gonk.browse.root` lines of a config text, as (name, directory). Lines that do not
/// parse are skipped: the coverage report is information, and gonk's own startup names a
/// bad line.
fn configured_roots(text: &str, home: &Path) -> Vec<(String, PathBuf)> {
    crate::config::values_for(text, "gonk.browse.root")
        .into_iter()
        .filter_map(|line| {
            let (name, path) = line.split_once('=')?;
            Some((
                name.trim().to_string(),
                crate::config::expand_home(path.trim(), home),
            ))
        })
        .collect()
}

/// One way the checkouts under the managed directory and the configured roots fail to cover
/// each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gap {
    /// A checkout that no root points into.
    Unused {
        /// The checkout's directory name.
        name: String,
        /// Where it is.
        path: PathBuf,
    },
    /// A root that points into the managed directory at something that does not exist.
    Missing {
        /// The root's name.
        name: String,
        /// Where it points.
        path: PathBuf,
    },
}

/// Which checkouts no root uses, and which roots inside `dir` have nothing to point at.
///
/// A root "uses" a checkout when it points at the checkout or anywhere inside it. Paths are
/// compared after resolving symlinks — for a path that does not exist, through its nearest
/// existing ancestor — so `/tmp/x` and `/private/tmp/x` are one directory. Roots outside
/// `dir` are none of this report's business.
pub fn coverage(
    dir: &Path,
    checkouts: &[(String, PathBuf)],
    roots: &[(String, PathBuf)],
) -> Vec<Gap> {
    let dir = resolve(dir);
    let roots: Vec<(&String, &PathBuf, PathBuf)> =
        roots.iter().map(|(n, p)| (n, p, resolve(p))).collect();
    let mut gaps = Vec::new();
    for (name, path) in checkouts {
        let here = resolve(path);
        if !roots.iter().any(|(_, _, root)| root.starts_with(&here)) {
            gaps.push(Gap::Unused {
                name: name.clone(),
                path: path.clone(),
            });
        }
    }
    for (name, written, root) in roots {
        if root.starts_with(&dir) && root != dir && !root.exists() {
            gaps.push(Gap::Missing {
                name: name.clone(),
                path: written.clone(),
            });
        }
    }
    gaps
}

/// `path` with symlinks resolved as far as it exists, and the rest appended as written.
fn resolve(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// `gonk.browse.root = "<name>=<path>"`.
pub fn root_line(name: &str, path: &str) -> String {
    format!("gonk.browse.root = \"{name}={path}\"")
}

/// A path as a config line spells it: `~/…` under the home directory, absolute otherwise.
fn display(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

/// Clone `repo` into `path`, or bring the clone already there forward.
fn checkout(git: &Git, repo: &Repo, path: &Path) -> Outcome {
    if !path.exists() {
        let path_text = path.to_string_lossy();
        if let Err(e) = git.run(None, &["clone", "--quiet", "--", &repo.url, &path_text]) {
            return Outcome::Failed(format!("git clone failed: {e}"));
        }
        return match git.run(Some(path), &["rev-parse", "--short", "HEAD"]) {
            Ok(head) => Outcome::Cloned { head },
            // An empty repository clones with no HEAD commit; it is still a checkout.
            Err(_) => Outcome::Cloned {
                head: "(empty)".to_string(),
            },
        };
    }
    match update(git, repo, path) {
        Ok(outcome) => outcome,
        Err(Stop::Refuse(why)) => Outcome::Refused(why),
        Err(Stop::Fail(why)) => Outcome::Failed(why),
    }
}

/// Fetch and fast-forward an existing clone — or say why not, having changed nothing.
/// Why an update did not happen.
enum Stop {
    /// The checkout's state says no.
    Refuse(String),
    /// git failed.
    Fail(String),
}

impl From<String> for Stop {
    fn from(why: String) -> Stop {
        Stop::Refuse(why)
    }
}

fn update(git: &Git, repo: &Repo, path: &Path) -> Result<Outcome, Stop> {
    let top = git
        .run(Some(path), &["rev-parse", "--show-toplevel"])
        .map_err(|_| format!("{} exists and is not a git checkout", path.display()))?;
    let (top, here) = (
        std::fs::canonicalize(&top).unwrap_or_else(|_| PathBuf::from(&top)),
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
    );
    if top != here {
        return Err(Stop::Refuse(format!(
            "{} is inside the checkout at {}, not a checkout of its own",
            path.display(),
            top.display()
        )));
    }
    let origin = git
        .run(Some(path), &["remote", "get-url", "origin"])
        .map_err(|_| "it has no `origin` remote to fetch from".to_string())?;
    if same_remote(&origin) != same_remote(&repo.url) {
        return Err(Stop::Refuse(format!(
            "its origin is {origin}, not {} — a different repository under the same name. \
             Check this one out under another name (`<name>=<url>`)",
            repo.url
        )));
    }
    let changes = git.run(Some(path), &["status", "--porcelain"])?;
    if !changes.is_empty() {
        return Err(Stop::Refuse(format!(
            "it has local changes ({} path(s); `git -C {} status`). A managed checkout is \
             fast-forwarded only; nothing was fetched or changed",
            changes.lines().count(),
            path.display()
        )));
    }
    let default = git
        .run(
            Some(path),
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
        )
        .map_err(|_| {
            format!(
                "origin's default branch is not recorded (`git -C {} remote set-head origin \
                 --auto` records it)",
                path.display()
            )
        })?;
    let branch = default.strip_prefix("origin/").unwrap_or(&default);
    let current = git
        .run(Some(path), &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_else(|_| "(detached HEAD)".to_string());
    if current != branch {
        return Err(Stop::Refuse(format!(
            "it is on {current}, not the default branch {branch}; left as it is"
        )));
    }
    git.run(Some(path), &["fetch", "--quiet", "--prune", "origin"])
        .map_err(|e| Stop::Fail(format!("git fetch failed: {e}")))?;
    let counts = git.run(
        Some(path),
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...{default}"),
        ],
    )?;
    let (ahead, behind) = counts
        .split_once(char::is_whitespace)
        .and_then(|(a, b)| Some((a.trim().parse::<u64>().ok()?, b.trim().parse::<u64>().ok()?)))
        .ok_or_else(|| format!("could not read `git rev-list` output `{counts}`"))?;
    let head = git.run(Some(path), &["rev-parse", "--short", "HEAD"])?;
    match (ahead, behind) {
        (0, 0) => Ok(Outcome::UpToDate { head }),
        (0, behind) => {
            git.run(Some(path), &["merge", "--ff-only", "--quiet", &default])
                .map_err(|e| Stop::Fail(format!("git merge --ff-only failed: {e}")))?;
            let to = git.run(Some(path), &["rev-parse", "--short", "HEAD"])?;
            Ok(Outcome::Updated {
                from: head,
                to,
                commits: behind,
            })
        }
        (ahead, 0) => Err(Stop::Refuse(format!(
            "{branch} has {ahead} commit(s) {default} does not; a managed checkout mirrors \
             upstream, so it was left as it is (fetched, not changed)"
        ))),
        (ahead, behind) => Err(Stop::Refuse(format!(
            "{branch} has DIVERGED from {default} ({ahead} commit(s) of its own, {behind} \
             upstream); nothing was reset or merged (fetched, not changed)"
        ))),
    }
}

/// A remote URL compared loosely: `…/repo`, `…/repo/` and `…/repo.git` are one repository.
fn same_remote(url: &str) -> &str {
    let url = url.trim().trim_end_matches('/');
    url.strip_suffix(".git").unwrap_or(url)
}

/// Append the missing `gonk.browse.root` lines to `config`, leaving every other line alone.
///
/// Idempotent: a name already configured for the same directory is [`ConfigLine::Present`],
/// and a name configured for a DIFFERENT directory is a [`ConfigLine::Conflict`] that writes
/// nothing for it. When anything is written, the previous file is copied beside it first
/// (`config.toml.<unix seconds>.bak`), and the new text replaces it by an atomic rename that
/// keeps its permissions.
pub fn append_roots(
    config: &Path,
    roots: &[(String, String)],
    home: &Path,
) -> Result<(Vec<ConfigLine>, Option<PathBuf>), String> {
    let text = match std::fs::read_to_string(config) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("reading {}: {e}", config.display())),
    };
    let existing: Vec<(String, PathBuf)> =
        crate::config::values_for(text.as_deref().unwrap_or(""), "gonk.browse.root")
            .into_iter()
            .filter_map(|line| {
                let (name, path) = line.split_once('=')?;
                Some((
                    name.trim().to_string(),
                    crate::config::expand_home(path.trim(), home),
                ))
            })
            .collect();
    let mut results = Vec::new();
    let mut lines = Vec::new();
    for (name, path) in roots {
        let wanted = crate::config::expand_home(path, home);
        let result = match existing.iter().find(|(seen, _)| seen == name) {
            Some((_, at)) if *at == wanted => ConfigLine::Present,
            Some((_, at)) => ConfigLine::Conflict(at.display().to_string()),
            None => {
                lines.push(root_line(name, path));
                ConfigLine::Added
            }
        };
        results.push(result);
    }
    if lines.is_empty() {
        return Ok((results, None));
    }
    let mut new = text.clone().unwrap_or_default();
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    for line in &lines {
        new.push_str(line);
        new.push('\n');
    }
    if let Some(parent) = config.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let backup = match &text {
        Some(_) => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = sibling(config, &format!("{stamp}.bak"));
            std::fs::copy(config, &backup)
                .map_err(|e| format!("backing up {}: {e}", config.display()))?;
            Some(backup)
        }
        None => None,
    };
    let staging = sibling(config, "checkout-tmp");
    std::fs::write(&staging, &new).map_err(|e| format!("writing {}: {e}", staging.display()))?;
    if let Ok(meta) = std::fs::metadata(config) {
        let _ = std::fs::set_permissions(&staging, meta.permissions());
    }
    std::fs::rename(&staging, config).map_err(|e| {
        let _ = std::fs::remove_file(&staging);
        format!("replacing {}: {e}", config.display())
    })?;
    Ok((results, backup))
}

/// `<file>.<suffix>`, beside `<file>`.
fn sibling(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    file.with_file_name(name)
}

/// The `git` binary, run so that nothing it does can wait for a person.
struct Git {
    /// `GIT_SSH_COMMAND`, when this process supplies one.
    ssh: Option<&'static str>,
}

impl Git {
    fn new() -> Git {
        let configured = std::env::var_os("GIT_SSH_COMMAND").is_some()
            || Git { ssh: None }
                .run(None, &["config", "--get", "core.sshCommand"])
                .is_ok_and(|v| !v.is_empty());
        Git {
            ssh: (!configured).then_some("ssh -o BatchMode=yes"),
        }
    }

    /// Run git, answering its trimmed stdout, or its stderr as the error.
    fn run(&self, dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new("git");
        if let Some(dir) = dir {
            command.arg("-C").arg(dir);
        }
        command
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        if let Some(ssh) = self.ssh {
            command.env("GIT_SSH_COMMAND", ssh);
        }
        let output = command
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "`git {}` exited {}: {}",
                args.join(" "),
                output
                    .status
                    .code()
                    .map_or("by signal".into(), |c| c.to_string()),
                stderr.trim()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn names_come_from_the_url_or_from_name_equals() {
        let args = parse(&[
            "https://github.com/kenn-io/kata.git",
            "mine=git@github.com:cwensel/kata",
            "https://example.com/r/x?ref=main",
        ])
        .unwrap();
        let names: Vec<&str> = args.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["kata", "mine", "x"]);
        assert_eq!(args.repos[1].url, "git@github.com:cwensel/kata");
        assert_eq!(args.repos[2].url, "https://example.com/r/x?ref=main");
    }

    #[test]
    fn bad_names_and_duplicates_are_refused() {
        assert!(parse(&[]).unwrap_err().contains("at least one"));
        assert!(parse(&["https://a/x.git", "https://b/x"])
            .unwrap_err()
            .contains("two repositories"));
        assert!(parse(&["pr=https://a/x.git"])
            .unwrap_err()
            .contains("reserved"));
        assert!(parse(&["--bogus"]).unwrap_err().contains("unknown"));
        assert!(parse(&["https://example.com/"]).is_err());
    }

    #[test]
    fn all_takes_no_urls_and_writes_no_config() {
        let args = parse(&["--all", "--dir", "/srv/co"]).unwrap();
        assert!(args.all && args.repos.is_empty());
        assert!(parse(&["--all", "https://a/x.git"])
            .unwrap_err()
            .contains("not both"));
        assert!(parse(&["--all", "--write-config"])
            .unwrap_err()
            .contains("never writes config"));
        assert!(
            parse(&[]).unwrap_err().contains("--all"),
            "named as the way"
        );
    }

    #[cfg(unix)]
    #[test]
    fn coverage_compares_through_symlinks_and_ignores_roots_elsewhere() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("a/src")).unwrap();
        std::fs::create_dir_all(real.join("b")).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let checkouts = [
            ("a".to_string(), real.join("a")),
            ("b".to_string(), real.join("b")),
        ];
        let roots = [
            // Through the symlink, and inside the checkout: still uses `a`.
            ("a".to_string(), link.join("a/src")),
            // Through the symlink, and not there.
            ("gone".to_string(), link.join("gone")),
            ("far".to_string(), PathBuf::from("/nowhere/far")),
        ];
        assert_eq!(
            coverage(&real, &checkouts, &roots),
            [
                Gap::Unused {
                    name: "b".into(),
                    path: real.join("b")
                },
                Gap::Missing {
                    name: "gone".into(),
                    path: link.join("gone")
                },
            ]
        );
    }

    #[test]
    fn paths_under_home_are_spelled_with_a_tilde() {
        let home = Path::new("/home/u");
        assert_eq!(
            display(Path::new("/home/u/.ikigai/checkouts/x"), home),
            "~/.ikigai/checkouts/x"
        );
        assert_eq!(display(Path::new("/srv/x"), home), "/srv/x");
    }

    #[test]
    fn remotes_compare_without_git_or_a_slash() {
        assert_eq!(
            same_remote("https://h/o/r.git"),
            same_remote("https://h/o/r/")
        );
        assert_ne!(same_remote("https://h/o/r"), same_remote("https://h/o/s"));
    }
}
