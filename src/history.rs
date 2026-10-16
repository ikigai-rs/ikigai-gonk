//! A browse root's git history as WEEKLY SERIES: commits per week, and test functions added
//! and removed per week, per language (ledger [#1631](http://localhost:1062/l/default/item/1631),
//! the git-history source of the delivery dashboard, ledger
//! [#1630](http://localhost:1062/l/default/item/1630)).
//!
//! ```text
//! urn:iki:gonk:history:{root}:weekly             Source  the series at the root's HEAD now
//! urn:iki:gonk:history:{root}:weekly:at:{sha}    Source  the series as at one commit
//! ```
//!
//! # The resource map
//!
//! | set | name | kind | how |
//! |---|---|---|---|
//! | a root's HEAD | `urn:repo:{root}:state` | composed, not built | browse's own freshness probe (`{sha} clean`), cached by gonk under the root's wide thread ([`crate::browse::cached_reads`]) |
//! | the series as at a commit | `urn:iki:gonk:history:{root}:weekly:at:{sha}` | **the one computation** | two `git log` passes over the root, bounded by a time budget; a pure function of `(sha, weeks)`, so cacheable with no thread to cut |
//! | the series now | `urn:iki:gonk:history:{root}:weekly` | composition | source the root's `state`, take the HEAD, source `…:weekly:at:{HEAD}` with the same arguments |
//! | a measure | `urn:iki:gonk:measure:{name}` | name only | the property each Turtle observation carries a count on |
//!
//! **No atom.** Nothing here holds state: the state is the repository's object store, which
//! `git commit` writes and the kernel never does. That is why the computation can be keyed
//! on the commit: everything reachable from a commit id is immutable, so the series AS AT a
//! commit is a constant, and "a new commit invalidates it" is not a cut at all but a NEW
//! NAME. The composite carries the freshness: it reads `state`, which the root watch cuts
//! on every change under `.git/refs`, `HEAD` and `packed-refs` (and on every working-tree
//! edit, since the wide thread is coarse), and an edit that moves no HEAD recomputes only
//! `state` and hits the cached series.
//!
//! **The temporal space is in the name.** `…:weekly:at:{sha}` is the history as of an
//! instant, addressed by the instant's commit, which is why it can be cached forever and
//! shared by every reader of that commit.
//!
//! # Why this is gonk's family and not browse's
//!
//! `urn:repo:{root}:…` is `ikigai-browse`'s grammar, a LIBRARY's family that other hosts
//! mount too. A gonk-only name inside it would mean one thing here and nothing elsewhere,
//! and a later browse release that added a `history` of its own would collide silently,
//! with the [`ikigai_core::Fallback`]'s order deciding which answered. gonk's overlay also
//! keys behavior on that prefix ([`crate::browse::cached_reads`] parses it). So the names
//! are gonk's (`urn:iki:gonk:…`, like `urn:iki:gonk:backup`), with the root as their
//! second segment so they read as "the history of root X" and enumerate per root, as
//! browse's rows do. The AUTHORITY is browse's: reading a root's history is reading the
//! root, so the family declares `urn:cap:browse:read:*` and enforces
//! `urn:cap:browse:read:{root}`, exactly as browse does for its own rows.
//!
//! # The method (also in each description, because a chart must state it)
//!
//! See [`METHOD`].
//!
//! # The bound
//!
//! `weeks` is at most [`MAX_WEEKS`] (an `InvalidArgument` past it), and the two `git`
//! passes share one time budget ([`DEFAULT_BUDGET`]). Past the budget the child is killed
//! and the request REFUSED with [`Error::Timeout`]: never a series cut short, because a
//! partial count read as a complete one is the silent truncation a bound exists to prevent.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Request, Result, UriTemplate, Verb,
};

/// The family's prefix.
pub const PREFIX: &str = "urn:iki:gonk:history:";

/// Where the Turtle face's measure properties are named.
pub const MEASURE_PREFIX: &str = "urn:iki:gonk:measure:";

/// The window when `weeks` is not given: a year.
pub const DEFAULT_WEEKS: u32 = 52;

/// The widest window a caller may ask for: ten years.
pub const MAX_WEEKS: u32 = 520;

/// How long the two `git` passes of one series may take together before the request is
/// refused. ikigai-cli's year (520 commits) takes well under a second on the machine that
/// serves it (see the PR that introduced this), so this is a guard against a pathological
/// repository, not a working limit.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(30);

/// How the series is counted, in words. Every description and the text and Turtle faces
/// carry it, so a chart drawn from this resource can say what it shows.
pub const METHOD: &str = "Commits: the non-merge commits reachable from the commit, counted \
    in the week of their COMMITTER date (when they landed); when the root is a subdirectory of \
    its repository, only those touching it. \
    Weeks begin Monday 00:00 UTC; the window is `weeks` weeks ending with the week that \
    holds the commit's own committer date, so the series is a pure function of the commit. \
    Tests: for each non-merge commit in the window, its diff against its parent (`git log \
    -p -U0`) is scanned, and every added line counts one test added and every removed line \
    one test removed when, leading whitespace aside, it begins: in a `.rs` file, `#[test]` \
    or `#[tokio::test` (Rust); in a `.py` file, `def test_` or `async def test_` (Python); \
    in a `.ts`, `.tsx`, `.js`, `.jsx`, `.mjs`, `.mts`, `.cjs` or `.cts` file, `Deno.test(`, \
    `Deno.test.only(` or `Deno.test.ignore(` (Deno). A test moved between files counts once \
    added and once removed; renames git detects count nothing. Merge commits contribute \
    neither commits nor tests (their changes are counted on the commits they merge).";

/// The languages counted, in column order.
const LANGUAGES: [&str; 3] = ["rust", "python", "deno"];

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const FACES: [&str; 3] = ["text/plain", "text/csv", "text/turtle"];
const DAY: i64 = 86_400;

/// `urn:iki:gonk:history:{root}:weekly` — the series at the root's HEAD now.
pub fn weekly_iri(root: &str) -> String {
    format!("{PREFIX}{root}:weekly")
}

/// `urn:iki:gonk:history:{root}:weekly:at:{sha}` — the series as at one commit.
pub fn weekly_at_iri(root: &str, sha: &str) -> String {
    format!("{PREFIX}{root}:weekly:at:{sha}")
}

/// The family over `roots`, with the [`DEFAULT_BUDGET`].
pub fn space(roots: &[(String, PathBuf)]) -> EndpointSpace {
    space_with_budget(roots, DEFAULT_BUDGET)
}

/// The family over `roots`, with a stated budget for each series' `git` passes — a test's
/// way to make the refusal happen without a large repository.
pub fn space_with_budget(roots: &[(String, PathBuf)], budget: Duration) -> EndpointSpace {
    let mut space = EndpointSpace::new();
    for (root, dir) in roots {
        space = space
            .bind(Exact::new(weekly_iri(root)), Weekly { root: root.clone() })
            .bind(
                UriTemplate::parse(format!("{PREFIX}{root}:weekly:at:{{sha}}"))
                    .expect("a root name holds no `:`, `/`, `{` or `}`"),
                WeeklyAt {
                    root: root.clone(),
                    dir: dir.clone(),
                    budget,
                },
            );
    }
    space.named(crate::spaces::iri(crate::spaces::HISTORY))
}

// ---------------------------------------------------------------- the two endpoints

/// The series at the root's HEAD now: a composition over `state` and `…:weekly:at:{sha}`.
struct Weekly {
    root: String,
}

#[async_trait]
impl Endpoint for Weekly {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        granted(inv, &self.root)?;
        // The arguments are checked HERE as well as at the pinned name, so a bad one is
        // refused before `git status` runs.
        Args::read(inv)?;
        let state = inv
            .source(&iri(&format!("urn:repo:{}:state", self.root))?)
            .await?;
        let state = String::from_utf8_lossy(&state.bytes);
        let head = head_of(&self.root, &state)?;
        let pinned = inv.request.args.iter().fold(
            Request::new(Verb::Source, iri(&weekly_at_iri(&self.root, head))?),
            |request, (name, value)| request.with_arg(name.clone(), value.clone()),
        );
        let series = inv.issue(pinned).await?;
        // Cacheable: the kernel takes the most restrictive expiry of what this read, so the
        // answer lives exactly as long as the `state` it was keyed on.
        Ok(Representation::new(series.repr_type.clone(), series.bytes.clone()).cacheable())
    }

    fn name(&self) -> &str {
        "gonk-history"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-history")
            .title("A repository's git history as weekly series, at its HEAD")
            .summary(format!(
                "urn:iki:gonk:history:{{root}}:weekly. Commits per week and test functions \
                 added and removed per week, per language (Rust, Python, Deno), for a \
                 configured browse root at its current HEAD: reads `urn:repo:{{root}}:state` \
                 for the HEAD and answers `urn:iki:gonk:history:{{root}}:weekly:at:{{sha}}` \
                 with the same arguments, so a new commit is a new name and an edit that \
                 moves no HEAD recomputes nothing but the probe. Method: {METHOD}"
            ))
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .input(weeks_spec())
            .input(as_spec())
            .output("text/plain")
            .output("text/csv")
            .output("text/turtle")
    }
}

/// The series as at one commit: the one computation in the family.
struct WeeklyAt {
    root: String,
    dir: PathBuf,
    budget: Duration,
}

#[async_trait]
impl Endpoint for WeeklyAt {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        granted(inv, &self.root)?;
        let sha = inv
            .bindings
            .get("sha")
            .ok_or_else(|| Error::MissingArgument("sha".to_string()))?
            .to_string();
        if !is_full_sha(&sha) {
            return Err(Error::InvalidArgument {
                name: "sha".to_string(),
                detail: format!(
                    "`{sha}` is not a full commit id (40 or 64 lowercase hex digits): an \
                     abbreviation could name a different commit tomorrow, and this name is \
                     cached as a constant"
                ),
            });
        }
        let args = Args::read(inv)?;
        if !matches!(args.face, Face::Csv) {
            // ★ The other faces are VIEWS of the CSV one, composed over it by name: one count
            // per commit and window, whichever face asks first, rather than one per face.
            let canonical = inv
                .request
                .args
                .iter()
                .filter(|(name, _)| name.as_str() != "as")
                .fold(
                    Request::new(Verb::Source, iri(&weekly_at_iri(&self.root, &sha))?),
                    |request, (name, value)| request.with_arg(name.clone(), value.clone()),
                )
                .with_arg("as", ikigai_core::ArgRef::Inline(b"text/csv".to_vec()));
            let canonical = inv.issue(canonical).await?;
            let rows = parse_csv(&String::from_utf8_lossy(&canonical.bytes))?;
            let body = match args.face {
                Face::Turtle => turtle(&self.root, &sha, &rows),
                _ => text(&self.root, &sha, &rows),
            };
            return Ok(
                Representation::new(ReprType::new(args.face.media_type()), body).cacheable(),
            );
        }
        let (dir, budget, weeks) = (self.dir.clone(), self.budget, args.weeks);
        let job = {
            let sha = sha.clone();
            move || count(&dir, &sha, weeks, budget)
        };
        // Seconds of `git` on a door's worker would stall every other request it carries,
        // so the passes run on the blocking pool when there is a runtime to own one.
        let series = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime
                .spawn_blocking(job)
                .await
                .map_err(|e| Error::Endpoint(format!("history: the count did not finish: {e}")))?,
            Err(_) => job(),
        }?;
        Ok(Representation::new(ReprType::new("text/csv"), csv(&series)).cacheable())
    }

    fn name(&self) -> &str {
        "gonk-history-at"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-history-at")
            .title("A repository's git history as weekly series, as at one commit")
            .summary(format!(
                "urn:iki:gonk:history:{{root}}:weekly:at:{{sha}}. Commits per week and test \
                 functions added and removed per week, per language, for a configured browse \
                 root, counted over the history reachable from one commit — a constant, so \
                 cached for as long as the kernel keeps it. Bounded: `weeks` is at most \
                 {MAX_WEEKS}, and the count is refused (a timeout) rather than cut short when \
                 its `git` passes overrun this server's budget. Method: {METHOD}"
            ))
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .input(
                ArgSpec::new("sha")
                    .summary("the full commit id the series is counted as at")
                    .class(XSD_STRING)
                    .binding(),
            )
            .input(weeks_spec())
            .input(as_spec())
            .output("text/plain")
            .output("text/csv")
            .output("text/turtle")
    }
}

fn weeks_spec() -> ArgSpec {
    ArgSpec::new("weeks")
        .summary("how many weeks the window covers, ending with the commit's own week")
        .class(XSD_INTEGER)
        .default_value(DEFAULT_WEEKS.to_string())
        .optional()
}

fn as_spec() -> ArgSpec {
    ArgSpec::new("as")
        .summary("the representation to answer in")
        .class(XSD_STRING)
        .one_of(FACES)
        .default_value("text/plain")
        .optional()
}

/// The per-root check behind the declared wildcard — browse's own rule, for browse's root.
fn granted(inv: &Invocation<'_>, root: &str) -> Result<()> {
    let scope = format!("{}{root}", ikigai_browse::CAP_PREFIX);
    if inv.capability.allows(&scope) || inv.capability.allows(ikigai_browse::CAP_WILDCARD) {
        return Ok(());
    }
    Err(Error::Denied(format!(
        "history: capability does not grant `{scope}`"
    )))
}

fn iri(text: &str) -> Result<Iri> {
    Iri::parse(text).map_err(|e| Error::Endpoint(format!("history: `{text}` is not an IRI: {e}")))
}

/// The HEAD out of `state`'s one line (`{sha} clean`, `{sha} dirty:{n}`, `unborn clean`, or
/// `not a git repository`).
fn head_of<'a>(root: &str, state: &'a str) -> Result<&'a str> {
    let first = state.split_whitespace().next().unwrap_or("");
    if state.trim() == "not a git repository" {
        return Err(Error::NotFound(format!(
            "history: browse root `{root}` is not a git repository"
        )));
    }
    if first == "unborn" {
        return Err(Error::NotFound(format!(
            "history: browse root `{root}` has no commits yet"
        )));
    }
    if is_full_sha(first) {
        Ok(first)
    } else {
        Err(Error::Endpoint(format!(
            "history: `urn:repo:{root}:state` answered `{}`, which names no HEAD",
            state.trim()
        )))
    }
}

fn is_full_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The arguments, read once.
struct Args {
    weeks: u32,
    face: Face,
}

#[derive(Clone, Copy)]
enum Face {
    Text,
    Csv,
    Turtle,
}

impl Face {
    fn media_type(self) -> &'static str {
        match self {
            Face::Text => "text/plain",
            Face::Csv => "text/csv",
            Face::Turtle => "text/turtle",
        }
    }
}

impl Args {
    fn read(inv: &Invocation<'_>) -> Result<Args> {
        let weeks = match inv.request.args.get("weeks") {
            None => DEFAULT_WEEKS,
            Some(_) => {
                let raw = inv.inline_str("weeks")?.trim();
                match raw.parse::<u32>() {
                    Ok(n) if (1..=MAX_WEEKS).contains(&n) => n,
                    _ => {
                        return Err(Error::InvalidArgument {
                            name: "weeks".to_string(),
                            detail: format!(
                                "`{raw}` is not a whole number of weeks from 1 to {MAX_WEEKS}"
                            ),
                        })
                    }
                }
            }
        };
        let face = match inv.request.args.get("as") {
            None => Face::Text,
            Some(_) => match inv.inline_str("as")?.trim() {
                "text/plain" => Face::Text,
                "text/csv" => Face::Csv,
                "text/turtle" => Face::Turtle,
                other => {
                    return Err(Error::InvalidArgument {
                        name: "as".to_string(),
                        detail: format!("`{other}` is not one of {}", FACES.join(", ")),
                    })
                }
            },
        };
        Ok(Args { weeks, face })
    }
}

// ---------------------------------------------------------------- the count

/// One week of the series.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Week {
    /// The week's Monday, as days since 1970-01-01.
    pub start: i64,
    /// Non-merge commits landed in the week.
    pub commits: u32,
    /// Test functions added, per language, in the order Rust, Python, Deno.
    pub added: [u32; 3],
    /// Test functions removed, per language, in the order Rust, Python, Deno.
    pub removed: [u32; 3],
}

/// A whole window, oldest week first, every week present (an empty week is a zero row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    /// The weeks.
    pub weeks: Vec<Week>,
}

impl Series {
    fn week_mut(&mut self, seconds: i64) -> Option<&mut Week> {
        let day = week_start(seconds.div_euclid(DAY));
        let first = self.weeks.first()?.start;
        let index = usize::try_from((day - first) / 7).ok()?;
        (day >= first).then_some(())?;
        self.weeks.get_mut(index)
    }
}

/// Count the series for `sha` in the repository at `dir` over `weeks` weeks, in `budget`.
///
/// # Errors
///
/// `NotFound` when `sha` names no commit there, `Timeout` past the budget, `Endpoint` when
/// `git` cannot run or fails otherwise.
pub fn count(dir: &std::path::Path, sha: &str, weeks: u32, budget: Duration) -> Result<Series> {
    let deadline = Instant::now() + budget;
    // The commit's own time anchors the window. `^{commit}` refuses a tree or a blob.
    let mut head_time = None;
    git(
        dir,
        &[
            "log",
            "-1",
            "--no-show-signature",
            "--format=%ct",
            &format!("{sha}^{{commit}}"),
            "--",
        ],
        deadline,
        |line| {
            if head_time.is_none() {
                head_time = String::from_utf8_lossy(line).trim().parse::<i64>().ok();
            }
        },
    )?;
    let head_time = head_time
        .ok_or_else(|| Error::Endpoint(format!("history: git gave no commit time for {sha}")))?;
    let last = week_start(head_time.div_euclid(DAY));
    let first = last - 7 * (i64::from(weeks) - 1);
    let mut series = Series {
        weeks: (0..i64::from(weeks))
            .map(|i| Week {
                start: first + 7 * i,
                ..Week::default()
            })
            .collect(),
    };
    let since = format!("--since=@{}", first * DAY);

    // Pass 1: the commits. `-- .` scopes them to the root when it is a subdirectory of its
    // repository, and ONLY then: a pathspec also drops a commit that changes nothing (an
    // empty commit), which at a repository's top is still a commit that landed.
    let mut prefix = String::new();
    git(dir, &["rev-parse", "--show-prefix"], deadline, |line| {
        prefix = String::from_utf8_lossy(line).trim().to_string();
    })?;
    let mut commits = vec!["log", "--no-merges", "--no-show-signature", "--format=%ct"];
    commits.push(&since);
    commits.push(sha);
    if !prefix.is_empty() {
        commits.extend(["--", "."]);
    }
    git(dir, &commits, deadline, |line| {
        if let Ok(time) = String::from_utf8_lossy(line).trim().parse::<i64>() {
            if let Some(week) = series.week_mut(time) {
                week.commits += 1;
            }
        }
    })?;

    // Pass 2: the tests. `-G test` keeps only the file pairs whose added or removed lines
    // say `test` — a superset of every pattern counted — so the patch stream stays small;
    // the patterns themselves are matched here.
    let mut scan = Scan::default();
    let mut args: Vec<String> = [
        "-c",
        "core.quotePath=false",
        "log",
        "--no-merges",
        "--no-show-signature",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--format=%x01%ct",
        "-p",
        "-U0",
        "-G",
        "test",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    args.push(since);
    args.push(sha.to_string());
    args.push("--".to_string());
    for ext in Lang::EXTENSIONS.iter().map(|(ext, _)| ext) {
        args.push(format!("*.{ext}"));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    git(dir, &args, deadline, |line| scan.line(line, &mut series))?;
    Ok(series)
}

/// Which language a file's tests are counted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    Deno,
}

impl Lang {
    const EXTENSIONS: [(&'static str, Lang); 10] = [
        ("rs", Lang::Rust),
        ("py", Lang::Python),
        ("ts", Lang::Deno),
        ("tsx", Lang::Deno),
        ("js", Lang::Deno),
        ("jsx", Lang::Deno),
        ("mjs", Lang::Deno),
        ("mts", Lang::Deno),
        ("cjs", Lang::Deno),
        ("cts", Lang::Deno),
    ];

    /// The language of a `--- a/…` or `+++ b/…` path, `None` for `/dev/null` or another kind
    /// of file.
    fn of(path: &str) -> Option<Lang> {
        let path = path.trim_end_matches(['\t', '"', '\r']);
        let name = path.rsplit('/').next()?;
        let (_, ext) = name.rsplit_once('.')?;
        Lang::EXTENSIONS
            .iter()
            .find(|(e, _)| *e == ext)
            .map(|(_, lang)| *lang)
    }

    fn index(self) -> usize {
        match self {
            Lang::Rust => 0,
            Lang::Python => 1,
            Lang::Deno => 2,
        }
    }

    /// Whether one added or removed line declares a test.
    fn declares_test(self, content: &str) -> bool {
        let line = content.trim_start();
        match self {
            Lang::Rust => line.starts_with("#[test]") || line.starts_with("#[tokio::test"),
            Lang::Python => line.starts_with("def test_") || line.starts_with("async def test_"),
            Lang::Deno => ["Deno.test(", "Deno.test.only(", "Deno.test.ignore("]
                .iter()
                .any(|p| line.starts_with(p)),
        }
    }
}

/// The patch reader's state: which commit and file it is in, and whether it is past the
/// file's header.
#[derive(Default)]
struct Scan {
    time: Option<i64>,
    lang: Option<Lang>,
    in_hunk: bool,
}

impl Scan {
    fn line(&mut self, raw: &[u8], series: &mut Series) {
        let line = String::from_utf8_lossy(raw);
        if let Some(time) = line.strip_prefix('\u{1}') {
            self.time = time.trim().parse().ok();
            self.lang = None;
            self.in_hunk = false;
            return;
        }
        // Nothing inside a hunk can begin `diff --git`: every content line carries a
        // `+`, `-` or ` ` first.
        if line.starts_with("diff --git ") {
            self.lang = None;
            self.in_hunk = false;
            return;
        }
        if !self.in_hunk {
            if let Some(path) = line
                .strip_prefix("--- a/")
                .or_else(|| line.strip_prefix("+++ b/"))
            {
                // The new path wins over the old when both name a language, which only
                // matters for a rename between kinds.
                if let Some(lang) = Lang::of(path) {
                    self.lang = Some(lang);
                }
            } else if line.starts_with("@@") {
                self.in_hunk = true;
            }
            return;
        }
        let (Some(lang), Some(time)) = (self.lang, self.time) else {
            return;
        };
        let (added, content) = match line.as_bytes().first() {
            Some(b'+') => (true, &line[1..]),
            Some(b'-') => (false, &line[1..]),
            _ => return,
        };
        if !lang.declares_test(content) {
            return;
        }
        if let Some(week) = series.week_mut(time) {
            if added {
                week.added[lang.index()] += 1;
            } else {
                week.removed[lang.index()] += 1;
            }
        }
    }
}

/// Run `git -C dir args…` and hand `on_line` each line of its output, refusing past
/// `deadline`.
///
/// A watchdog thread owns the child: it kills it at the deadline, which ends the output, so
/// the reader never waits on a git that has stopped talking. A run that was killed is a
/// [`Error::Timeout`] even though some lines were read: those lines are a prefix of the
/// answer, never the answer.
fn git(
    dir: &std::path::Path,
    args: &[&str],
    deadline: Instant,
    on_line: impl FnMut(&[u8]),
) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    stream(command, deadline, on_line)
}

/// [`git`]'s body, over any command — so the watchdog can be shown killing a child that
/// never ends without a repository slow enough to need it.
fn stream(mut command: Command, deadline: Instant, mut on_line: impl FnMut(&[u8])) -> Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(timeout());
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Endpoint(format!("history: could not run git: {e}")))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    // Drained beside stdout, so a chatty stderr cannot fill its pipe and stall the child.
    let errors = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.by_ref().take(64 * 1024).read_to_end(&mut text);
        String::from_utf8_lossy(&text).trim().to_string()
    });
    let (done, finished) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        let late = matches!(
            finished.recv_timeout(remaining),
            Err(RecvTimeoutError::Timeout)
        );
        if late {
            let _ = child.kill();
        }
        // `wait`, never `try_wait`: the end of the output says the child is exiting, not
        // that it has been reaped.
        (late, child.wait())
    });
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                on_line(&line);
            }
            Err(_) => break,
        }
    }
    let _ = done.send(());
    let (late, status) = watchdog
        .join()
        .map_err(|_| Error::Endpoint("history: the git watchdog panicked".to_string()))?;
    let stderr = errors.join().unwrap_or_default();
    let status =
        status.map_err(|e| Error::Endpoint(format!("history: could not wait for git: {e}")))?;
    if status.success() {
        return Ok(());
    }
    if late {
        return Err(timeout());
    }
    if stderr.contains("unknown revision")
        || stderr.contains("bad object")
        || stderr.contains("bad revision")
        || stderr.contains("not a commit")
    {
        return Err(Error::NotFound(format!(
            "history: no such commit: {stderr}"
        )));
    }
    Err(Error::Endpoint(format!(
        "history: git exited {}: {stderr}",
        status.code().unwrap_or(-1)
    )))
}

fn timeout() -> Error {
    Error::Timeout(format!(
        "history: counting the series took longer than this server's budget; nothing was \
         answered rather than a partial count. Ask for fewer `weeks` (the default is \
         {DEFAULT_WEEKS})"
    ))
}

// ---------------------------------------------------------------- calendar arithmetic

/// The Monday on or before `day` (days since 1970-01-01, a Thursday).
fn week_start(day: i64) -> i64 {
    day - (day + 3).rem_euclid(7)
}

/// `day` (days since 1970-01-01) as `YYYY-MM-DD`, proleptic Gregorian.
fn date(day: i64) -> String {
    // Howard Hinnant's `civil_from_days`.
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

// ---------------------------------------------------------------- the faces

/// The column names, shared by the text and CSV faces.
fn columns() -> Vec<String> {
    let mut columns = vec![
        "commits".to_string(),
        "tests_added".to_string(),
        "tests_removed".to_string(),
    ];
    for lang in LANGUAGES {
        columns.push(format!("{lang}_added"));
        columns.push(format!("{lang}_removed"));
    }
    columns
}

fn values(week: &Week) -> Vec<u32> {
    let mut values = vec![
        week.commits,
        week.added.iter().sum(),
        week.removed.iter().sum(),
    ];
    for i in 0..LANGUAGES.len() {
        values.push(week.added[i]);
        values.push(week.removed[i]);
    }
    values
}

/// `week,commits,tests_added,tests_removed,rust_added,…`, one row per week, oldest first.
fn csv(series: &Series) -> String {
    let mut out = format!("week,{}\n", columns().join(","));
    for week in &series.weeks {
        let row: Vec<String> = values(week).iter().map(u32::to_string).collect();
        out.push_str(&format!("{},{}\n", date(week.start), row.join(",")));
    }
    out
}

/// One CSV row read back: the week's Monday as written, and its values in column order.
struct Row {
    week: String,
    values: Vec<u32>,
}

/// The CSV face read back into rows, for the views composed over it.
fn parse_csv(text: &str) -> Result<Vec<Row>> {
    let width = columns().len();
    let bad = |line: &str| Error::Endpoint(format!("history: unreadable series row `{line}`"));
    text.lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut cells = line.split(',');
            let week = cells.next().ok_or_else(|| bad(line))?.to_string();
            let values = cells
                .map(|cell| cell.parse::<u32>().map_err(|_| bad(line)))
                .collect::<Result<Vec<u32>>>()?;
            if values.len() == width {
                Ok(Row { week, values })
            } else {
                Err(bad(line))
            }
        })
        .collect()
}

fn text(root: &str, sha: &str, rows: &[Row]) -> String {
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        return String::new();
    };
    let mut out = format!(
        "history of `{root}` as at {sha}\n{} weeks, the week of {} through the week of {} \
         (weeks begin Monday 00:00 UTC)\n\n",
        rows.len(),
        first.week,
        last.week
    );
    let columns = columns();
    let mut header = format!("{:<10}", "week");
    for c in &columns {
        header.push_str(&format!("  {c:>14}"));
    }
    out.push_str(header.trim_end());
    out.push('\n');
    let mut totals = vec![0u32; columns.len()];
    for week in rows {
        let mut row = week.week.clone();
        for (i, &v) in week.values.iter().enumerate() {
            totals[i] += v;
            row.push_str(&format!("  {v:>14}"));
        }
        out.push_str(&row);
        out.push('\n');
    }
    let mut row = format!("{:<10}", "total");
    for v in totals {
        row.push_str(&format!("  {v:>14}"));
    }
    out.push_str(&row);
    out.push_str("\n\nmethod: ");
    out.push_str(METHOD);
    out.push('\n');
    out
}

/// The RDF Data Cube shape: the series is a `qb:DataSet`, each week a `qb:Observation`
/// keyed by `sdmx-dimension:refPeriod` (the week's Monday), each count on a measure
/// property named under [`MEASURE_PREFIX`]. Every IRI is a stable name: no blank nodes.
fn turtle(root: &str, sha: &str, rows: &[Row]) -> String {
    let dataset = weekly_at_iri(root, sha);
    let mut out = String::from(
        "@prefix qb: <http://purl.org/linked-data/cube#> .\n\
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
         @prefix dct: <http://purl.org/dc/terms/> .\n\
         @prefix sdmx-dimension: <http://purl.org/linked-data/sdmx/2009/dimension#> .\n\n",
    );
    out.push_str(&format!(
        "<{dataset}> a qb:DataSet ;\n    rdfs:label {} ;\n    dct:description {} .\n\n",
        literal(&format!(
            "weekly git history of browse root {root} as at {sha}"
        )),
        literal(METHOD)
    ));
    let measures: Vec<String> = columns().iter().map(|c| c.replace('_', "-")).collect();
    for m in &measures {
        out.push_str(&format!(
            "<{MEASURE_PREFIX}{m}> a qb:MeasureProperty ;\n    rdfs:label {} ;\n    \
             rdfs:range xsd:integer .\n",
            literal(&m.replace('-', " "))
        ));
    }
    for week in rows {
        let day = &week.week;
        out.push_str(&format!(
            "\n<{dataset}:week:{day}> a qb:Observation ;\n    qb:dataSet <{dataset}> ;\n    \
             sdmx-dimension:refPeriod \"{day}\"^^xsd:date"
        ));
        for (m, v) in measures.iter().zip(&week.values) {
            out.push_str(&format!(" ;\n    <{MEASURE_PREFIX}{m}> {v}"));
        }
        out.push_str(" .\n");
    }
    out
}

/// A Turtle string literal.
fn literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_are_dated_and_weeks_begin_on_monday() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(-1), "1969-12-31");
        assert_eq!(date(11_016), "2000-02-29");
        // 1970-01-01 was a Thursday; its week began Monday 1969-12-29.
        assert_eq!(date(week_start(0)), "1969-12-29");
        // 2026-10-15 is a Thursday; 2026-10-12 a Monday, which starts its own week.
        let thursday = 20_741;
        assert_eq!(date(thursday), "2026-10-15");
        assert_eq!(date(week_start(thursday)), "2026-10-12");
        assert_eq!(week_start(week_start(thursday)), week_start(thursday));
    }

    #[test]
    fn the_test_patterns_are_the_documented_ones() {
        let yes = [
            (Lang::Rust, "    #[test]"),
            (Lang::Rust, "#[tokio::test]"),
            (Lang::Rust, "#[tokio::test(flavor = \"multi_thread\")]"),
            (Lang::Python, "def test_x():"),
            (Lang::Python, "    async def test_y(self):"),
            (Lang::Deno, "Deno.test(\"a\", () => {});"),
            (Lang::Deno, "  Deno.test.ignore(\"b\", () => {});"),
        ];
        for (lang, line) in yes {
            assert!(lang.declares_test(line), "{lang:?} {line}");
        }
        let no = [
            (Lang::Rust, "/// #[test]"),
            (Lang::Rust, "#[cfg(test)]"),
            (Lang::Python, "def helper_test():"),
            (Lang::Deno, "await t.step(\"c\", () => {});"),
        ];
        for (lang, line) in no {
            assert!(!lang.declares_test(line), "{lang:?} {line}");
        }
        assert_eq!(Lang::of("src/lib.rs"), Some(Lang::Rust));
        assert_eq!(Lang::of("a b/t.py\t"), Some(Lang::Python));
        assert_eq!(Lang::of("x.test.mts"), Some(Lang::Deno));
        assert_eq!(Lang::of("README.md"), None);
        assert_eq!(Lang::of("Makefile"), None);
    }

    /// ★ The bound REFUSES: a child still running at the deadline is killed, and what it
    /// had said by then is not an answer.
    #[test]
    fn a_child_past_the_deadline_is_killed_and_refused() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo partial; exec sleep 30"]);
        let started = Instant::now();
        let mut seen = Vec::new();
        let answer = stream(command, started + Duration::from_millis(300), |line| {
            seen.push(String::from_utf8_lossy(line).into_owned())
        });
        assert!(matches!(answer, Err(Error::Timeout(_))), "{answer:?}");
        assert_eq!(seen, ["partial"], "it read what came before the deadline");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "and did not wait out the child: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_head_is_read_off_the_state_line() {
        let sha = "a".repeat(40);
        assert_eq!(head_of("r", &format!("{sha} clean")).unwrap(), sha);
        assert_eq!(head_of("r", &format!("{sha} dirty:3")).unwrap(), sha);
        assert!(matches!(
            head_of("r", "unborn clean"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            head_of("r", "not a git repository"),
            Err(Error::NotFound(_))
        ));
        assert!(!is_full_sha("abc123"));
        assert!(!is_full_sha(&"A".repeat(40)));
        assert!(is_full_sha(&"0".repeat(64)));
    }
}
