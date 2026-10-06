//! `ikigai-gonk roborev file` — turn one roborev `review.completed` hook into ledger items.
//!
//! [roborev](https://github.com/kenn-io/roborev) reviews each commit with a coding agent and
//! fires `[[hooks]]` on its events. This is the command such a hook runs: it reads the
//! review roborev hands it, and files **one ledger item per finding** into a named ledger on
//! a running gonk, over the HTTP door, so every finding can be triaged, labeled, linked,
//! claimed and closed here, and shows up on browse's page for the file it is about.
//!
//! # What roborev hands a hook (read from its source, `kenn-io/roborev` 2026-10-05)
//!
//! A command hook is a shell line with `{var}` placeholders, each replaced by a
//! single-quoted value (`internal/daemon/hooks.go`, `interpolate`): `{job_id}`, `{repo}`,
//! `{repo_name}`, `{sha}`, `{agent}`, `{verdict}`, `{findings}`, `{error}`. There is no
//! stdin and no JSON. **`{findings}` is Markdown**: the agent returns a JSON document
//! (`pkg/structuredreview`, severities `critical|high|medium|low`, each finding a
//! `problem`, a `fix` and an optional `file:line` `location`), and the daemon broadcasts
//! that document's `Document.Markdown` rendering, not the document. So this module parses
//! that rendering — and accepts a finding only when rendering it again reproduces the bytes
//! roborev sent, the same check roborev's own `ParseMarkdown` makes. A review that drifts
//! from the format is refused, loudly, rather than filed as a guess.
//!
//! ⚠ `review.completed` also fires for roborev's `fix` and `task` jobs, whose output is
//! free-form prose. That is not a review, so it files nothing and exits 0 ([`Review::NotStructured`]).
//!
//! # Idempotence
//!
//! Every finding gets a key, `urn:roborev:finding:{hash}`, over the browse root, the file
//! path, and the problem text with its whitespace collapsed ([`finding_key`]). The key is
//! filed as one of the item's `about` IRIs, and before filing, the ledger is asked for any
//! item, open or closed, about that key. So the same hook payload run twice files once, and
//! roborev re-reporting a finding verbatim (a rerun, or a later commit's review carrying it
//! forward) files nothing new.
//!
//! ⚠ Why not the job id and the finding's index: roborev **reuses the job id on a rerun**
//! (`ReenqueueJob` resets the terminal job in place, `internal/storage/jobs.go`), and a
//! rerun is a fresh model call with different findings. Keyed on (job, index), a rerun's
//! NEW first finding would be skipped as already filed. Keyed on content, the worst case is
//! the opposite one: the same defect described in different words is filed twice. A
//! duplicate is closed in one click; a dropped finding is never seen.
//!
//! The check and the append are two requests, not one transaction, so two hooks filing the
//! same finding at the same instant can both file it. One invocation files its findings one
//! at a time, which is also what `ikigai-ledger` 0.2.x needs: concurrent appends in the same
//! millisecond can collide on an id there (fixed in 0.3.0, ledger #768).
//!
//! # Authority
//!
//! The HTTP door grants an anonymous loopback caller the read and write tokens of the
//! ledgers listed in `gonk.http.ledger`, and nothing else (`crate::doors`). Filing needs
//! both: write to append, read to check the key first. `ikigai-gonk grants <ledger> write`
//! prints them (`urn:cap:ledger:write:<ledger>` and its siblings). A ledger the door does not
//! grant answers 403, and this command says so and exits 1.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use ikigai_ledger::Ledger;
use sha2::{Digest, Sha256};

/// The severities roborev's schema allows, most severe first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// `low` — filed at priority 3, and only under `--min-severity low`.
    Low,
    /// `medium` — priority 2.
    Medium,
    /// `high` — priority 1.
    High,
    /// `critical` — priority 0.
    Critical,
}

impl Severity {
    /// Parse roborev's lowercase word.
    pub fn parse(word: &str) -> Option<Severity> {
        match word {
            "critical" => Some(Severity::Critical),
            "high" => Some(Severity::High),
            "medium" => Some(Severity::Medium),
            "low" => Some(Severity::Low),
            _ => None,
        }
    }

    /// The word, as roborev writes it and as the ledger label carries it.
    pub fn word(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        }
    }

    /// The ledger priority: 0 is the highest.
    pub fn priority(self) -> u8 {
        match self {
            Severity::Critical => 0,
            Severity::High => 1,
            Severity::Medium => 2,
            Severity::Low => 3,
        }
    }

    /// The heading roborev renders (`titleSeverity`: the first letter capitalized).
    fn heading(self) -> &'static str {
        match self {
            Severity::Critical => "Critical",
            Severity::High => "High",
            Severity::Medium => "Medium",
            Severity::Low => "Low",
        }
    }
}

/// One finding, as roborev's structured document holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// How serious.
    pub severity: Severity,
    /// `file:line` when the agent knew it; free text in practice.
    pub location: Option<String>,
    /// What is wrong. roborev's prompt asks a medium or high finding to name its concrete
    /// trigger here; there is no separate trigger field.
    pub problem: String,
    /// What to do about it.
    pub fix: String,
    /// The panel reviewers that reported it, on a synthesized review.
    pub reported_by: Option<String>,
}

/// What a `{findings}` value turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Review {
    /// Not a structured review: a `fix` or `task` job's prose, or a legacy review with no
    /// extracted findings. Nothing to file.
    NotStructured,
    /// A structured review and its findings, possibly none.
    Findings(Vec<Finding>),
}

const SUMMARY: &str = "## Summary\n\n";
const FINDINGS: &str = "\n\n## Findings\n";

/// Parse the Markdown roborev's `Document.Markdown` renders.
///
/// Errors only on a review that LOOKS structured (it opens with `## Summary` and carries a
/// `## Findings` heading) and does not parse back exactly: that is format drift, and filing
/// a guess would be worse than filing nothing.
///
/// ```
/// use ikigai_gonk::roborev::{parse, Review, Severity};
///
/// // (No continuation line may start with `#`: rustdoc reads `##` there as an escape.)
/// let markdown = "## Summary\n\nOne issue.\n\n**Agent assessment:** Fail\n\n## Findings\
///                 \n\n### 1. High\n\n**Location:** src/a.rs:12\n\n\
///                 **Problem:** It panics.\n\n**Fix:** Return the error.\n";
/// let Review::Findings(found) = parse(markdown).unwrap() else { panic!() };
/// assert_eq!(found[0].severity, Severity::High);
/// assert_eq!(found[0].location.as_deref(), Some("src/a.rs:12"));
/// assert_eq!(found[0].problem, "It panics.");
/// assert_eq!(found[0].fix, "Return the error.");
/// ```
pub fn parse(markdown: &str) -> Result<Review, String> {
    let text = markdown.replace("\r\n", "\n");
    // The rendering ends with a newline (after the last Fix, or the Findings heading) that
    // a shell or a trim may have taken; put exactly one back.
    let text = format!("{}\n", text.trim());
    if !text.starts_with(SUMMARY) {
        return Ok(Review::NotStructured);
    }
    let Some(at) = text.find(FINDINGS) else {
        // "No issues found.", or an agent that could not review: no findings section.
        return Ok(Review::Findings(Vec::new()));
    };
    let section = &text[at + FINDINGS.len()..];
    let mut findings = Vec::new();
    let mut rest = section;
    let mut n = 1;
    while !rest.is_empty() {
        let next = format!("\n\n### {}. ", n + 1);
        let (block, after) = match rest.find(&next) {
            // Keep the first newline in this block: it ends the Fix (or Reported by) line.
            Some(end) => rest.split_at(end + 1),
            None => (rest, ""),
        };
        let finding = parse_block(block, n)?;
        if render_block(&finding, n) != block {
            return Err(format!(
                "finding {n} does not render back to the text roborev sent, so its fields \
                 cannot be told apart; nothing was filed. The block was:\n{block}"
            ));
        }
        findings.push(finding);
        rest = after;
        n += 1;
    }
    if findings.is_empty() {
        return Err("the review has a `## Findings` heading and no finding under it".into());
    }
    Ok(Review::Findings(findings))
}

fn parse_block(block: &str, n: usize) -> Result<Finding, String> {
    let drift = |what: &str| format!("finding {n}: {what}; nothing was filed");
    let heading = format!("\n### {n}. ");
    let body = block
        .strip_prefix(&heading)
        .ok_or_else(|| drift(&format!("expected the heading `### {n}. <Severity>`")))?;
    let (title, body) = body
        .split_once("\n\n")
        .ok_or_else(|| drift("no blank line after the heading"))?;
    let severity = Severity::parse(&title.to_ascii_lowercase())
        .ok_or_else(|| drift(&format!("`{title}` is not critical, high, medium or low")))?;
    let (location, body) = match body.strip_prefix("**Location:** ") {
        Some(located) => {
            let (location, body) = located
                .split_once("\n\n")
                .ok_or_else(|| drift("no blank line after the location"))?;
            (Some(location.to_string()), body)
        }
        None => (None, body),
    };
    let body = body
        .strip_prefix("**Problem:** ")
        .ok_or_else(|| drift("no `**Problem:**` line"))?;
    let (problem, fix) = body
        .split_once("\n\n**Fix:** ")
        .ok_or_else(|| drift("no `**Fix:**` line"))?;
    let (fix, reported_by) = match fix.rsplit_once("\n\n**Reported by:** ") {
        Some((fix, by)) => (fix, Some(by.trim_end_matches('\n').to_string())),
        None => (fix.trim_end_matches('\n'), None),
    };
    Ok(Finding {
        severity,
        location,
        problem: problem.to_string(),
        fix: fix.to_string(),
        reported_by,
    })
}

/// One finding as `Document.Markdown` writes it, leading newline included.
fn render_block(finding: &Finding, n: usize) -> String {
    let mut out = format!("\n### {n}. {}\n\n", finding.severity.heading());
    if let Some(location) = &finding.location {
        out.push_str(&format!("**Location:** {location}\n\n"));
    }
    out.push_str(&format!("**Problem:** {}\n\n", finding.problem));
    out.push_str(&format!("**Fix:** {}\n", finding.fix));
    if let Some(by) = &finding.reported_by {
        out.push_str(&format!("\n**Reported by:** {by}\n"));
    }
    out
}

/// The repository-relative path a roborev `location` names, if it names one.
///
/// roborev asks for `file:line`; agents also write ranges, `file:line:col`, a GitHub `#L12`,
/// backticks, absolute paths and lists. This takes the first token, drops the line, and
/// strips `repo_path` from an absolute path. It answers `None` rather than invent a path: an
/// absolute path outside the repository, or one that climbs with `..`.
///
/// ```
/// use ikigai_gonk::roborev::location_path;
///
/// assert_eq!(location_path("src/a.rs:12-30", None).as_deref(), Some("src/a.rs"));
/// assert_eq!(location_path("`/r/x/src/b.rs:4`", Some("/r/x")).as_deref(), Some("src/b.rs"));
/// assert_eq!(location_path("/elsewhere/c.rs:1", Some("/r/x")), None);
/// ```
pub fn location_path(location: &str, repo_path: Option<&str>) -> Option<String> {
    let token = location
        .split(|c: char| c.is_whitespace() || c == ',')
        .find(|token| !token.is_empty())?;
    let mut path = token.trim_matches(|c| matches!(c, '`' | '"' | '\'' | '(' | ')' | '[' | ']'));
    if let Some(at) = path.find("#L") {
        path = &path[..at];
    }
    let line = path
        .char_indices()
        .find(|&(i, c)| c == ':' && path[i + 1..].starts_with(|d: char| d.is_ascii_digit()));
    if let Some((at, _)) = line {
        path = &path[..at];
    }
    if let Some(repo) = repo_path.map(|r| r.trim_end_matches('/')) {
        if let Some(inside) = path.strip_prefix(repo).and_then(|p| p.strip_prefix('/')) {
            path = inside;
        }
    }
    let path = path.trim_start_matches("./").trim_end_matches(':');
    if path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return None;
    }
    Some(path.to_string())
}

/// `urn:repo:{root}:file:{path}` — the IRI browse serves the file at, and joins items on.
///
/// The path is percent-encoded the way `ikigai-browse` encodes it (its `iri_encode`, which is
/// crate-private there, so this is a copy): `/` and the URN-safe punctuation stay literal.
pub fn file_iri(root: &str, path: &str) -> String {
    const SAFE: &[u8] = b"-._~/!$&'()*+,;=:@";
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || SAFE.contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("urn:repo:{root}:file:{encoded}")
}

/// The idempotence key: `urn:roborev:finding:{32 hex}`, over the root, the path and the
/// problem with its whitespace collapsed. Not the severity, the fix or the line: those move
/// while the defect stays the same one.
///
/// ```
/// use ikigai_gonk::roborev::finding_key;
///
/// let key = finding_key("gonk", Some("src/a.rs"), "It  panics\non empty input.");
/// assert_eq!(key, finding_key("gonk", Some("src/a.rs"), "It panics on empty input."));
/// assert_ne!(key, finding_key("gonk", Some("src/b.rs"), "It panics on empty input."));
/// assert!(key.starts_with("urn:roborev:finding:") && key.len() == 20 + 32);
/// ```
pub fn finding_key(root: &str, path: Option<&str>, problem: &str) -> String {
    let problem = problem.split_whitespace().collect::<Vec<_>>().join(" ");
    let digest = Sha256::digest(format!(
        "roborev-finding/1\n{root}\n{}\n{problem}",
        path.unwrap_or("")
    ));
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("urn:roborev:finding:{hex}")
}

/// The ledger item's title: the problem's first line, cut at a word near 100 characters.
pub fn title(problem: &str) -> String {
    const MAX: usize = 100;
    let first = problem
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if first.chars().count() <= MAX {
        return first.to_string();
    }
    let cut: String = first.chars().take(MAX).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(at) if at > MAX / 2 => &cut[..at],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end())
}

/// Everything `ikigai-gonk roborev file` was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileArgs {
    /// `--gonk`: the HTTP door, `http://<loopback host>:<port>`.
    pub gonk: String,
    /// `--ledger`: the ledger to file into.
    pub ledger: String,
    /// `--root`: the browse root name the file IRIs are under (roborev's `{repo_name}`
    /// when gonk browses the repository under that name).
    pub root: String,
    /// `--findings`: the Markdown, or `-` for stdin.
    pub findings: String,
    /// `--job`: roborev's job id, for the body.
    pub job: Option<String>,
    /// `--sha`: the commit reviewed; filed as the item's `revision`.
    pub sha: Option<String>,
    /// `--agent`: the reviewing agent, for the body.
    pub agent: Option<String>,
    /// `--repo-path`: roborev's `{repo}`, stripped from absolute locations.
    pub repo_path: Option<String>,
    /// `--min-severity`: the least severe finding filed. Default `medium`.
    pub min_severity: Severity,
    /// `--dry-run`: print what would be filed and touch nothing.
    pub dry_run: bool,
}

/// Parse the arguments after `roborev`.
pub fn parse_args(mut args: impl Iterator<Item = String>) -> Result<FileArgs, String> {
    match args.next().as_deref() {
        Some("file") => {}
        Some(other) => return Err(format!("roborev: `{other}` is not a subcommand (file)")),
        None => return Err("roborev: expected `file`".to_string()),
    }
    let (mut gonk, mut ledger, mut root, mut findings) = (None, None, None, None);
    let mut out = FileArgs {
        gonk: String::new(),
        ledger: String::new(),
        root: String::new(),
        findings: String::new(),
        job: None,
        sha: None,
        agent: None,
        repo_path: None,
        min_severity: Severity::Medium,
        dry_run: false,
    };
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("roborev file: {flag} needs a value"))
        };
        match flag.as_str() {
            "--gonk" => gonk = Some(value()?),
            "--ledger" => ledger = Some(value()?),
            "--root" => root = Some(value()?),
            "--findings" => findings = Some(value()?),
            "--job" => out.job = Some(value()?).filter(|v| !v.is_empty()),
            "--sha" => out.sha = Some(value()?).filter(|v| !v.is_empty()),
            "--agent" => out.agent = Some(value()?).filter(|v| !v.is_empty()),
            "--repo-path" => out.repo_path = Some(value()?).filter(|v| !v.is_empty()),
            "--min-severity" => {
                let word = value()?;
                out.min_severity = Severity::parse(&word).ok_or_else(|| {
                    format!("roborev file: --min-severity `{word}` is not critical, high, medium or low")
                })?;
            }
            "--dry-run" => out.dry_run = true,
            other => return Err(format!("roborev file: unknown argument `{other}`")),
        }
    }
    let required = |value: Option<String>, flag: &str| {
        value.ok_or_else(|| format!("roborev file: {flag} is required"))
    };
    out.gonk = required(gonk, "--gonk")?;
    out.ledger = required(ledger, "--ledger")?;
    out.root = required(root, "--root")?;
    out.findings = required(findings, "--findings")?;
    Ledger::parse(&out.ledger).map_err(|e| format!("roborev file: --ledger: {e}"))?;
    if out.root.is_empty() || out.root.contains(|c: char| c == ':' || c.is_whitespace()) {
        return Err(format!(
            "roborev file: --root `{}` is not a browse root name (no `:`, no whitespace)",
            out.root
        ));
    }
    Door::parse(&out.gonk)?;
    Ok(out)
}

/// One finding, decided: what would be filed, or why it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// The finding.
    pub finding: Finding,
    /// Its idempotence key.
    pub key: String,
    /// The file IRI it is about, when its location names one.
    pub file: Option<String>,
    /// The append's `content`: title, a blank line, the body.
    pub content: String,
    /// The append's other arguments, in order.
    pub args: Vec<(&'static str, String)>,
}

/// Decide every finding: no network, so the mapping is testable on its own.
pub fn plan(args: &FileArgs, findings: &[Finding]) -> Vec<Planned> {
    findings
        .iter()
        .filter(|f| f.severity >= args.min_severity)
        .map(|finding| {
            let path = finding
                .location
                .as_deref()
                .and_then(|l| location_path(l, args.repo_path.as_deref()));
            let key = finding_key(&args.root, path.as_deref(), &finding.problem);
            let file = path.as_deref().map(|p| file_iri(&args.root, p));
            let mut body = format!("{}\n\nFix: {}\n\n", finding.problem, finding.fix);
            body.push_str(&format!("- severity: {}\n", finding.severity.word()));
            if let Some(location) = &finding.location {
                body.push_str(&format!("- location: {location}\n"));
            }
            if let Some(sha) = &args.sha {
                body.push_str(&format!("- commit: {sha}\n"));
            }
            if let Some(job) = &args.job {
                body.push_str(&format!("- roborev job: {job} (`roborev show {job}`)\n"));
            }
            if let Some(agent) = &args.agent {
                body.push_str(&format!("- agent: {agent}\n"));
            }
            if let Some(by) = &finding.reported_by {
                body.push_str(&format!("- reported by: {by}\n"));
            }
            body.push_str(&format!("- key: {key}\n"));
            let about = match &file {
                Some(file) => format!("{file} {key}"),
                None => key.clone(),
            };
            let mut append = vec![
                ("labels", format!("roborev,{}", finding.severity.word())),
                ("priority", finding.severity.priority().to_string()),
                ("about", about),
                ("author", "roborev".to_string()),
            ];
            if let Some(sha) = &args.sha {
                append.push(("revision", sha.clone()));
            }
            Planned {
                finding: finding.clone(),
                key,
                file,
                content: format!("{}\n\n{body}", title(&finding.problem)),
                args: append,
            }
        })
        .collect()
}

/// What happened to one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Filed now, as this item (`#12`, or `acme#12`).
    Filed(String),
    /// Already in the ledger as this item; not filed again.
    Already(String),
    /// Below `--min-severity`.
    BelowThreshold(Severity),
}

/// Run `roborev file`: parse, plan, then check and file each finding in turn. Prints one
/// line per finding to `out`. A refusal or an unreachable door stops it with an error; the
/// findings filed before that stay filed, and running it again skips them.
pub fn run(args: &FileArgs, out: &mut impl Write) -> Result<Vec<Outcome>, String> {
    let markdown = if args.findings == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| format!("reading the findings from stdin: {e}"))?;
        text
    } else {
        args.findings.clone()
    };
    let findings = match parse(&markdown)? {
        Review::NotStructured => {
            let _ = writeln!(
                out,
                "not a structured review (a fix or task job?): nothing to file"
            );
            return Ok(Vec::new());
        }
        Review::Findings(findings) => findings,
    };
    let door = Door::parse(&args.gonk)?;
    let ledger = Ledger::parse(&args.ledger).map_err(|e| e.to_string())?;
    let mut outcomes = Vec::new();
    for skipped in findings.iter().filter(|f| f.severity < args.min_severity) {
        let _ = writeln!(
            out,
            "skipped  {:<8} below --min-severity {}: {}",
            skipped.severity.word(),
            args.min_severity.word(),
            title(&skipped.problem)
        );
        outcomes.push(Outcome::BelowThreshold(skipped.severity));
    }
    for planned in plan(args, &findings) {
        let sev = planned.finding.severity.word();
        if args.dry_run {
            let _ = writeln!(
                out,
                "would file {sev}: {}",
                planned.content.lines().next().unwrap_or("")
            );
            for (name, value) in &planned.args {
                let _ = writeln!(out, "    {name}={value}");
            }
            continue;
        }
        let query = format!("?about={}&status=all&limit=1", url_encode(&planned.key));
        let listing = door.call("GET", &path_of(&ledger, "items"), &query, "")?;
        if let Some(item) = first_item(&listing) {
            let _ = writeln!(out, "already  {sev:<8} {item} {}", planned.key);
            outcomes.push(Outcome::Already(item));
            continue;
        }
        let query = planned
            .args
            .iter()
            .map(|(name, value)| format!("{name}={}", url_encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        let filed = door.call(
            "POST",
            &path_of(&ledger, "append"),
            &format!("?{query}"),
            &planned.content,
        )?;
        let item = filed.split_whitespace().next().unwrap_or("?").to_string();
        let _ = writeln!(
            out,
            "filed    {sev:<8} {item} {}",
            planned.content.lines().next().unwrap_or("")
        );
        outcomes.push(Outcome::Filed(item));
    }
    Ok(outcomes)
}

/// The door's URL path for one of a ledger's resources: the IRI with `urn:` dropped and
/// every `:` a `/`, which is the mapping `ikigai-web` reverses.
fn path_of(ledger: &Ledger, action: &str) -> String {
    let iri = ledger.resource(action);
    format!("/{}", iri.trim_start_matches("urn:").replace(':', "/"))
}

/// The first item in a plain-text listing (`  #12  open  p1  title …`), if any.
fn first_item(listing: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let token = line.split_whitespace().next()?;
        let (_, number) = token.split_once('#')?;
        (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
            .then(|| token.to_string())
    })
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// gonk's HTTP door, as `--gonk` names it.
///
/// Loopback names only, because the door grants nothing to any other `Host` (DNS
/// rebinding, `crate::doors`): an address this command could reach and gonk would refuse
/// is a misconfiguration to name now, not a 403 to explain later. Plain HTTP only, which
/// is all the door speaks.
#[derive(Debug)]
struct Door {
    host: String,
    port: u16,
}

impl Door {
    fn parse(url: &str) -> Result<Door, String> {
        let bad = |why: &str| format!("--gonk `{url}`: {why} (expected http://127.0.0.1:1060)");
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| bad("only http:// — the door is plain HTTP on loopback"))?;
        let rest = rest.trim_end_matches('/');
        let (host, port) = rest.rsplit_once(':').ok_or_else(|| bad("no port"))?;
        let port: u16 = port.parse().map_err(|_| bad("the port is not a number"))?;
        if !["localhost", "127.0.0.1", "[::1]"].contains(&host) {
            return Err(bad(
                "gonk's HTTP door grants nothing to a non-loopback Host, so only localhost, \
                 127.0.0.1 or [::1]",
            ));
        }
        Ok(Door {
            host: host.to_string(),
            port,
        })
    }

    /// One request, `Connection: close`; the body of a 200, or an error naming the refusal.
    fn call(&self, method: &str, path: &str, query: &str, body: &str) -> Result<String, String> {
        let target = format!(
            "{}:{}",
            self.host.trim_matches(|c| c == '[' || c == ']'),
            self.port
        );
        let unreachable = |e: std::io::Error| {
            format!(
                "cannot reach gonk at http://{}:{}: {e}",
                self.host, self.port
            )
        };
        let addrs = target.to_socket_addrs().map_err(unreachable)?;
        let mut last = None;
        let mut stream = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let mut stream = stream.ok_or_else(|| {
            unreachable(last.unwrap_or_else(|| std::io::Error::other("no address")))
        })?;
        let timeout = Some(Duration::from_secs(30));
        stream.set_read_timeout(timeout).map_err(unreachable)?;
        stream.set_write_timeout(timeout).map_err(unreachable)?;
        let request = format!(
            "{method} {path}{query} HTTP/1.1\r\nHost: {}:{}\r\nAccept: text/plain\r\n\
             Content-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            self.host,
            self.port,
            body.len()
        );
        stream.write_all(request.as_bytes()).map_err(unreachable)?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).map_err(unreachable)?;
        let raw = String::from_utf8_lossy(&raw);
        let (head, mut text) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| format!("gonk answered {method} {path} with no status line"))?;
        let length = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        });
        if let Some(length) = length.filter(|&l| l <= text.len()) {
            text = &text[..length];
        }
        match status {
            200..=299 => Ok(text.to_string()),
            403 => Err(format!(
                "gonk refused {method} {path} (403): {}. The HTTP door grants an anonymous \
                 loopback caller the read and write tokens of the ledgers in `gonk.http.ledger` \
                 and nothing else; filing needs both (`ikigai-gonk grants <ledger> write` \
                 prints them)",
                text.trim()
            )),
            _ => Err(format!(
                "gonk answered {method} {path} with {status}: {}",
                text.trim()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(severity: Severity, location: Option<&str>, problem: &str) -> Finding {
        Finding {
            severity,
            location: location.map(str::to_string),
            problem: problem.to_string(),
            fix: "Do the other thing.".to_string(),
            reported_by: None,
        }
    }

    fn args() -> FileArgs {
        parse_args(
            [
                "file",
                "--gonk",
                "http://127.0.0.1:1060",
                "--ledger",
                "reviews",
                "--root",
                "gonk",
                "--findings",
                "-",
                "--sha",
                "abc123",
                "--job",
                "42",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap()
    }

    #[test]
    fn prose_from_a_fix_or_task_job_is_not_a_review() {
        assert_eq!(
            parse("Applied the patch.\n").unwrap(),
            Review::NotStructured
        );
        assert_eq!(parse("").unwrap(), Review::NotStructured);
    }

    #[test]
    fn a_passing_review_has_no_findings() {
        let pass = "## Summary\n\nLooks fine.\n\n**Agent assessment:** Pass\n\nNo issues found.\n";
        assert_eq!(parse(pass).unwrap(), Review::Findings(Vec::new()));
    }

    #[test]
    fn a_multi_paragraph_problem_and_a_panel_attribution_parse_back() {
        let md = "## Summary\n\nTwo.\n\n## Findings\n\n### 1. Medium\n\n**Problem:** First \
                  paragraph.\n\nSecond paragraph.\n\n**Fix:** Mend it.\n\n**Reported by:** codex, \
                  gemini\n\n### 2. Low\n\n**Location:** a.rs:1\n\n**Problem:** Nit.\n\n**Fix:** \
                  Tidy.\n";
        let Review::Findings(found) = parse(md).unwrap() else {
            panic!("structured")
        };
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].problem, "First paragraph.\n\nSecond paragraph.");
        assert_eq!(found[0].reported_by.as_deref(), Some("codex, gemini"));
        assert_eq!(found[0].location, None);
        assert_eq!(found[1].severity, Severity::Low);
        assert_eq!(found[1].fix, "Tidy.");
    }

    #[test]
    fn a_review_that_drifts_from_the_rendering_is_refused_not_guessed() {
        // Severity spelled outside roborev's schema.
        let md =
            "## Summary\n\nx\n\n## Findings\n\n### 1. Severe\n\n**Problem:** p\n\n**Fix:** f\n";
        assert!(parse(md).unwrap_err().contains("not critical, high"));
        // A field roborev never renders.
        let md = "## Summary\n\nx\n\n## Findings\n\n### 1. High\n\n**Problem:** p\n\n**Fix:** f\n\n**Trigger:** t\n";
        // Parses (the extra line folds into the fix) only if it renders back — it does, so
        // this is the honest limit of the check: it proves the fields are separable, not that
        // the producer is roborev.
        assert!(parse(md).is_ok());
        // No Fix line at all.
        let md = "## Summary\n\nx\n\n## Findings\n\n### 1. High\n\n**Problem:** p\n";
        assert!(parse(md).unwrap_err().contains("Fix"));
        // A heading with no finding.
        let md = "## Summary\n\nx\n\n## Findings\n";
        assert!(parse(md).is_err());
    }

    #[test]
    fn locations_name_a_path_or_nothing() {
        let cases = [
            ("src/a.rs:12", Some("src/a.rs")),
            ("src/a.rs:12:5", Some("src/a.rs")),
            ("src/a.rs#L12-L20", Some("src/a.rs")),
            ("`src/a.rs` lines 3-9", Some("src/a.rs")),
            ("src/a.rs:12, src/b.rs:3", Some("src/a.rs")),
            ("./README.md", Some("README.md")),
            ("../outside.rs:1", None),
            ("/abs/elsewhere.rs:1", None),
            ("", None),
        ];
        for (location, want) in cases {
            assert_eq!(location_path(location, None).as_deref(), want, "{location}");
        }
        assert_eq!(
            location_path("/repo/src/a.rs:3", Some("/repo/")).as_deref(),
            Some("src/a.rs")
        );
    }

    #[test]
    fn file_iris_encode_like_browse() {
        assert_eq!(
            file_iri("gonk", "src/a b.rs"),
            "urn:repo:gonk:file:src/a%20b.rs"
        );
        assert_eq!(file_iri("gonk", "src/k.rs"), "urn:repo:gonk:file:src/k.rs");
    }

    #[test]
    fn titles_cut_at_a_word() {
        assert_eq!(title("Short.\nMore."), "Short.");
        let long = "word ".repeat(40);
        let cut = title(&long);
        assert!(cut.ends_with('…') && cut.chars().count() <= 101, "{cut}");
        assert!(!cut.contains("  "));
    }

    #[test]
    fn the_mapping() {
        let found = [
            finding(Severity::Critical, Some("src/a.rs:1"), "Boom."),
            finding(Severity::High, None, "Bang."),
            finding(Severity::Medium, Some("src/b.rs:2"), "Hmm."),
            finding(Severity::Low, Some("src/c.rs:3"), "Nit."),
        ];
        let planned = plan(&args(), &found);
        assert_eq!(planned.len(), 3, "low is below the default threshold");
        let get = |p: &Planned, name: &str| {
            p.args
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get(&planned[0], "priority").as_deref(), Some("0"));
        assert_eq!(get(&planned[1], "priority").as_deref(), Some("1"));
        assert_eq!(get(&planned[2], "priority").as_deref(), Some("2"));
        assert_eq!(
            get(&planned[0], "labels").as_deref(),
            Some("roborev,critical")
        );
        assert_eq!(get(&planned[0], "revision").as_deref(), Some("abc123"));
        assert_eq!(
            get(&planned[0], "about").unwrap(),
            format!("urn:repo:gonk:file:src/a.rs {}", planned[0].key)
        );
        assert_eq!(
            get(&planned[1], "about").unwrap(),
            planned[1].key,
            "no location, no file"
        );
        assert!(planned[0]
            .content
            .starts_with("Boom.\n\nBoom.\n\nFix: Do the other thing.\n"));
        assert!(planned[0]
            .content
            .contains("- roborev job: 42 (`roborev show 42`)\n"));

        let mut low = args();
        low.min_severity = Severity::Low;
        assert_eq!(plan(&low, &found).len(), 4);
    }

    #[test]
    fn listings_and_paths() {
        assert_eq!(first_item("no items match\n"), None);
        assert_eq!(
            first_item("  #12  open    p1  Boom.  [critical roborev]\n\n1 item(s)\n").as_deref(),
            Some("#12")
        );
        assert_eq!(
            first_item("acme#3  closed  p2  x\n").as_deref(),
            Some("acme#3")
        );
        assert_eq!(
            path_of(&Ledger::parse("default").unwrap(), "items"),
            "/iki/ledger/items"
        );
        assert_eq!(
            path_of(&Ledger::parse("acme").unwrap(), "append"),
            "/iki/ledger/acme/append"
        );
    }

    #[test]
    fn the_door_is_loopback_http() {
        assert!(Door::parse("http://127.0.0.1:1060").is_ok());
        assert!(Door::parse("http://localhost:1060/").is_ok());
        assert!(Door::parse("http://[::1]:1060").is_ok());
        assert!(Door::parse("https://127.0.0.1:1060").is_err());
        assert!(Door::parse("http://gonk.example:1060")
            .unwrap_err()
            .contains("non-loopback"));
        assert!(Door::parse("http://127.0.0.1").is_err());
    }

    #[test]
    fn the_arguments() {
        let parsed = args();
        assert_eq!(parsed.min_severity, Severity::Medium);
        assert_eq!(parsed.sha.as_deref(), Some("abc123"));
        let base = [
            "file",
            "--gonk",
            "http://127.0.0.1:1",
            "--ledger",
            "x",
            "--root",
            "r",
        ];
        let missing = parse_args(base.into_iter().map(String::from)).unwrap_err();
        assert!(missing.contains("--findings is required"), "{missing}");
        let bad_root = parse_args(
            [
                "file",
                "--gonk",
                "http://127.0.0.1:1",
                "--ledger",
                "x",
                "--root",
                "a:b",
                "--findings",
                "-",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap_err();
        assert!(bad_root.contains("not a browse root"), "{bad_root}");
        // roborev interpolates an empty {sha} as '' — that is "not given", not a revision.
        let empty = parse_args(
            [
                "file",
                "--gonk",
                "http://127.0.0.1:1",
                "--ledger",
                "x",
                "--root",
                "r",
                "--findings",
                "-",
                "--sha",
                "",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();
        assert_eq!(empty.sha, None);
    }
}
