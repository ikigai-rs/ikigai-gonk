//! `ikigai-gonk findings file <root>` — file gonk's OWN published review findings as ledger
//! items (ledger [#1453](http://localhost:1062/l/default/item/1453)).
//!
//! A review pass mints **pending findings**; a person publishes or declines each one at the
//! Queue (or over the socket); and until this command, a published finding reached the ledger
//! only by that person typing a keyed `append` by hand. `roborev file` ([`crate::roborev`]) has
//! done the same job for roborev's findings since ledger #779. This is its twin for gonk's own:
//! it reads a root's findings through the browse family, and files each one as ONE keyed
//! append, so a re-run files nothing new.
//!
//! # ★ Which findings: PUBLISHED ones, and by default only the serious set
//!
//! **Never a pending finding.** A pending finding is a model's claim no person has looked at,
//! and the review-value experiment (ledger #723) measured how much of that is worth a person's
//! time: a per-commit reviewer found less than half the serious bugs per token that an unled
//! audit did, and about half the serious claims it did make were coin flips. Filing every
//! pending guess would turn the ledger into a second review queue, one with no decline
//! button, no reason words and no judge. The Queue is where a claim is decided; the ledger is
//! where DECIDED work goes. A published finding is a person's statement that the claim is
//! real, so that, and only that, is filed. Brian's rule (2026-09-19) holds one step further
//! down: *nothing gets published to Gonk except by the human*, and nothing gets FILED from a
//! review either, except what a human published.
//!
//! **By default only the serious set** — `gonk.queue.serious`, the same words the Queue asks a
//! person about ([`crate::config::QueuePolicy`]), judged on the row's `effective_severity` (the
//! person's own word when they re-rated it at publish). A published finding below that set is
//! often positive signal or a remark worth keeping on the file's page, and is not work.
//! `--severity all` files every published finding. A published finding always carries a rating
//! in practice: the finding Sink refuses a decision on a finding the model left unrated unless
//! the decision states one. An unrated row would still be filed, as the Queue asks about one
//! ([`QueuePolicy::queues`]): nobody classed it as minor. The severity words are never written down here:
//! the policy spells them once, in config, and the contract checks that spelling at startup.
//!
//! `--reproduced` narrows further, to the findings published with the contract's "reproduced"
//! mark (ledger #696), for a ledger that should hold only defects someone showed happening.
//!
//! # Idempotence
//!
//! Every finding is filed `key=urn:iki:finding:{id}` — the finding's own IRI. A finding id is a
//! hash of the pass and the quote, so it is stable, and the keyed append (`ikigai-ledger`
//! 0.4.0) checks the key and files in one store update: a re-run, or two runs at once, answer
//! `existing` and file nothing. The key is ALSO one of the item's `about` IRIs, beside the file
//! (or pull request) the finding annotates and the annotation its publication minted, so browse
//! joins the item to the file exactly as it joins a roborev item.
//!
//! ⚠ The gonk Book's "Review every commit" chapter, written before this command, filed by hand
//! with `key=finding:{id}`. An item filed that way is a different key, and this command files
//! the finding again beside it. `ikigai-gonk ledger backfill-keys` ([`crate::keys`]) gives a
//! hand-filed item `about` `urn:iki:finding:{id}` this command's key, but only when it carries
//! NO key: the ledger sets a key once, so a `key=finding:{id}` item is reported as skipped.
//!
//! # Where it runs: the owner-only socket, never the HTTP door
//!
//! Reading findings needs `urn:cap:browse:read:*`, and an anonymous loopback caller of the HTTP
//! door holds only the ledgers' tokens (`tests/queue.rs::an_anonymous_loopback_caller_can_neither
//! _read_nor_decide_a_finding`). So unlike `roborev file`, which only writes, this command speaks
//! to a RUNNING gonk over its socket, whose capability is root — the door `ledger backfill-keys`
//! and `layer take` use.
//!
//! # Why a command and not a step inside the Queue's publish
//!
//! Filing at the moment of publication was considered and not built. The publish runs under the
//! DECIDING person's capability, which holds `urn:cap:annotate` and need not hold any ledger's
//! write token; which ledger a root's work belongs in is a setting nobody has written yet; and a
//! publish that also files would give one click two effects in two graphs, with no way to
//! publish without filing. A command over the socket keeps the decision and the filing two acts,
//! and its key makes running it after every triage session (or from a timer) free.

use std::io::Write;

use ikigai_core::{ArgRef, Iri, Request, Verb};
use ikigai_ledger::json::{Answer, MEDIA_TYPE as JSON, SCHEMA};
use ikigai_ledger::Ledger;
use serde_json::Value;

use crate::config::{Flags, QueuePolicy};

/// The label every item this command files carries, beside `repo:<root>` and the severity.
pub const LABEL: &str = "finding";

/// The label an item carries when its finding was published with the "reproduced" mark.
pub const REPRODUCED_LABEL: &str = "reproduced";

/// The findings listing's state that holds a person's publications — a word of the findings
/// Source's `state` set (`ikigai-browse`), and the decision outcome a published row carries.
const PUBLISHED: &str = "published";

/// Which published findings are filed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// `--severity serious` (the default): the `gonk.queue.serious` set, and unrated ones.
    #[default]
    Serious,
    /// `--severity all`: every published finding.
    All,
}

/// Everything `ikigai-gonk findings file` was told.
#[derive(Debug, Default)]
pub struct FileArgs {
    /// The browse root whose findings are filed (`gonk.browse.root`'s NAME).
    pub root: String,
    /// `--ledger`: the ledger to file into. Default `default`.
    pub ledger: String,
    /// `--severity`.
    pub scope: Scope,
    /// `--reproduced`: only findings published with the reproduced mark.
    pub reproduced_only: bool,
    /// `--dry-run`: read and plan, file nothing.
    pub dry_run: bool,
    /// `--socket`, `--config`, `--config-home`, `--data-home`: where the running gonk is, and
    /// which `config.toml` says so (and says `gonk.queue.serious`).
    pub flags: Flags,
}

/// Parse the arguments after `findings`.
///
/// ```
/// use ikigai_gonk::findings::{parse_args, Scope};
///
/// let args = ["file", "ikigai-gonk", "--severity", "all", "--socket", "/tmp/g.sock"];
/// let parsed = parse_args(args.iter().map(|s| s.to_string())).unwrap();
/// assert_eq!(parsed.root, "ikigai-gonk");
/// assert_eq!(parsed.ledger, "default");
/// assert_eq!(parsed.scope, Scope::All);
/// assert_eq!(parsed.flags.socket.as_deref(), Some("/tmp/g.sock"));
/// assert!(parse_args(["file"].iter().map(|s| s.to_string())).is_err());
/// assert!(parse_args(["file", "a:b"].iter().map(|s| s.to_string())).is_err());
/// ```
pub fn parse_args(mut args: impl Iterator<Item = String>) -> Result<FileArgs, String> {
    match args.next().as_deref() {
        Some("file") => {}
        Some(other) => return Err(format!("findings: `{other}` is not a subcommand (file)")),
        None => return Err("findings: expected `file <root>`".to_string()),
    }
    let root = args
        .next()
        .filter(|root| !root.starts_with('-'))
        .ok_or("findings file: expected <root>, the `gonk.browse.root` NAME")?;
    if root.is_empty() || root.contains(|c: char| c == ':' || c.is_whitespace()) {
        return Err(format!(
            "findings file: `{root}` is not a browse root name (no `:`, no whitespace)"
        ));
    }
    let mut out = FileArgs {
        root,
        ledger: "default".to_string(),
        ..FileArgs::default()
    };
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("findings file: {flag} needs a value"))
        };
        match flag.as_str() {
            "--ledger" => out.ledger = value()?,
            "--severity" => {
                out.scope = match value()?.as_str() {
                    "serious" => Scope::Serious,
                    "all" => Scope::All,
                    other => {
                        return Err(format!(
                            "findings file: --severity `{other}` is `serious` (the \
                             gonk.queue.serious set, the default) or `all`"
                        ))
                    }
                }
            }
            "--reproduced" => out.reproduced_only = true,
            "--dry-run" => out.dry_run = true,
            "--socket" => out.flags.socket = Some(value()?),
            "--config" => out.flags.config = Some(value()?.into()),
            "--config-home" => out.flags.config_home = Some(value()?.into()),
            "--data-home" => out.flags.data_home = Some(value()?.into()),
            other => return Err(format!("findings file: unknown argument `{other}`")),
        }
    }
    Ledger::parse(&out.ledger).map_err(|e| format!("findings file: --ledger: {e}"))?;
    Ok(out)
}

/// What decides which published rows are filed.
#[derive(Debug, Clone, Copy)]
pub struct Filter<'a> {
    /// The serious set.
    pub policy: &'a QueuePolicy,
    /// `--severity`.
    pub scope: Scope,
    /// `--reproduced`.
    pub reproduced_only: bool,
}

/// One finding, decided: what would be filed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// The finding's id.
    pub id: String,
    /// Its idempotence key: the finding's IRI.
    pub key: String,
    /// The append's `content`: the title, a blank line, the body.
    pub content: String,
    /// The append's other arguments, in order.
    pub args: Vec<(&'static str, String)>,
}

/// One finding not filed, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// The finding's id, or `?` for a row with none.
    pub id: String,
    /// Why, in words.
    pub why: String,
}

/// Decide every row: no socket, so the mapping is testable on its own.
pub fn plan(root: &str, rows: &[Value], filter: Filter<'_>) -> (Vec<Planned>, Vec<Skipped>) {
    let mut planned = Vec::new();
    let mut skipped = Vec::new();
    for row in rows {
        match decide(root, row, filter) {
            Ok(p) => planned.push(p),
            Err(why) => skipped.push(Skipped {
                id: text(row, "id").unwrap_or("?").to_string(),
                why,
            }),
        }
    }
    (planned, skipped)
}

fn decide(root: &str, row: &Value, filter: Filter<'_>) -> Result<Planned, String> {
    let id = text(row, "id").ok_or("a row with no `id`")?;
    let decision = row.get("decision").filter(|d| !d.is_null());
    let outcome = decision.and_then(|d| text(d, "outcome"));
    if outcome != Some(PUBLISHED) {
        // The listing was asked for published rows; a row that says otherwise is not one.
        return Err(format!(
            "not published (its decision is {})",
            outcome.unwrap_or("none")
        ));
    }
    let severity = crate::queue::rated(row);
    if filter.scope == Scope::Serious && !filter.policy.queues(severity) {
        return Err(format!(
            "{} is not in gonk.queue.serious ({}); --severity all files it",
            severity.unwrap_or("unrated"),
            filter.policy.serious.join(",")
        ));
    }
    let reproduced = decision
        .and_then(|d| d.get("reproduced"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if filter.reproduced_only && !reproduced {
        return Err("published without the reproduced mark; --reproduced files only those".into());
    }
    let key = crate::queue::finding_iri(id);
    ikigai_ledger::model::parse_key(&key)
        .map_err(|e| format!("`{key}` cannot be a ledger key: {e}"))?;

    let body = text(row, "body").unwrap_or("").trim();
    let title = match crate::roborev::title(body) {
        t if t.is_empty() => format!("Finding {id}"),
        t => t,
    };
    let mut text_body = if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n\n")
    };
    let mut fact = |name: &str, value: String| text_body.push_str(&format!("- {name}: {value}\n"));
    fact("finding", key.clone());
    match (row.get("pr").and_then(Value::as_u64), text(row, "path")) {
        (Some(pr), _) => fact("pull request", format!("{pr} in {root}")),
        (None, Some(path)) => fact(
            "file",
            match row.get("line").and_then(Value::as_u64) {
                Some(line) => format!("{path} line {line}"),
                None => path.to_string(),
            },
        ),
        (None, None) => {}
    }
    if let Some(exact) = text(row, "exact").filter(|e| !e.trim().is_empty()) {
        fact("quote", format!("\"{}\"", one_line(exact, 200)));
    }
    match (severity, text(row, "severity")) {
        (Some(final_), Some(proposed)) if final_ != proposed => fact(
            "severity",
            format!("{final_} (the model proposed {proposed})"),
        ),
        (Some(word), _) => fact("severity", word.to_string()),
        (None, _) => fact("severity", "unrated".to_string()),
    }
    if let Some(creator) = text(row, "creator") {
        fact("reviewer", creator.to_string());
    }
    let decided = decision.and_then(|d| text(d, "decided_at")).unwrap_or("?");
    fact(
        "published",
        format!(
            "{decided}, {}",
            if reproduced {
                "marked reproduced"
            } else {
                "not marked reproduced"
            }
        ),
    );
    if let Some(note) = decision
        .and_then(|d| text(d, "note"))
        .filter(|n| !n.trim().is_empty())
    {
        fact("note", one_line(note, 500));
    }
    let minted = decision.and_then(|d| text(d, "minted"));
    if let Some(annotation) = minted {
        fact("annotation", annotation.to_string());
    }
    if let Some(verdict) = row
        .get("judge")
        .and_then(|j| text(j, "verdict"))
        .filter(|v| !v.is_empty())
    {
        fact("judge", verdict.to_string());
    }
    fact("key", key.clone());

    // `about`: the file (or PR) the finding annotates, the finding, and the annotation its
    // publication minted — each only when it is an IRI the space-separated list can carry.
    let mut about: Vec<String> = Vec::new();
    for iri in [text(row, "annotates"), Some(key.as_str()), minted]
        .into_iter()
        .flatten()
    {
        let carries = !iri.contains(char::is_whitespace) && Iri::parse(iri).is_ok();
        if carries && !about.iter().any(|a| a == iri) {
            about.push(iri.to_string());
        }
    }
    let mut labels = vec![LABEL.to_string(), format!("repo:{root}")];
    if let Some(word) = severity.filter(|w| is_label(w)) {
        labels.push(word.to_string());
    }
    if reproduced {
        labels.push(REPRODUCED_LABEL.to_string());
    }
    Ok(Planned {
        id: id.to_string(),
        key: key.clone(),
        content: format!("{title}\n\n{text_body}"),
        args: vec![
            ("key", key),
            ("labels", labels.join(",")),
            ("about", about.join(" ")),
        ],
    })
}

/// A word the label list can carry as one label (no comma, no whitespace).
fn is_label(word: &str) -> bool {
    !word.is_empty() && !word.contains(|c: char| c == ',' || c.is_whitespace())
}

fn text<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}

/// `text` on one line, whitespace collapsed, cut at `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// What happened to one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Filed now, as this item.
    Filed(String),
    /// Already in the ledger as this item (open, closed or deleted); not filed again.
    Already(String),
    /// Not filed, and why.
    Skipped(Skipped),
    /// `--dry-run`: would be filed.
    Would(String),
}

/// Run `findings file` over `issue` — one request in, the representation's bytes out (the
/// socket door in production, [`crate::keys::over_socket`]; it must carry a capability that
/// reads the root's findings and appends to the ledger, which the socket's root does).
///
/// A refusal stops it with an error; the findings filed before that stay filed, and running
/// it again answers them `existing`.
pub fn run(
    issue: &dyn Fn(Request) -> Result<Vec<u8>, String>,
    args: &FileArgs,
    policy: &QueuePolicy,
    out: &mut impl Write,
) -> Result<Vec<Outcome>, String> {
    let ledger = Ledger::parse(&args.ledger).map_err(|e| e.to_string())?;
    let listing = crate::queue::findings_iri(&args.root);
    let bytes = issue(
        Request::new(
            Verb::Source,
            Iri::parse(&listing).map_err(|e| format!("`{listing}`: {e}"))?,
        )
        .with_arg("state", inline(PUBLISHED))
        .with_arg("as", inline("application/json")),
    )?;
    let rows: Vec<Value> = match serde_json::from_slice(&bytes) {
        Ok(Value::Array(rows)) => rows,
        _ => {
            return Err(format!(
                "`{listing}` answered something that is not a JSON array of findings"
            ))
        }
    };
    let filter = Filter {
        policy,
        scope: args.scope,
        reproduced_only: args.reproduced_only,
    };
    let (planned, skipped) = plan(&args.root, &rows, filter);
    let mut outcomes = Vec::new();
    let mut counts = (0usize, 0usize);
    for planned in planned {
        let title = planned.content.lines().next().unwrap_or("").to_string();
        if args.dry_run {
            let _ = writeln!(out, "would file  {}  {title}", planned.id);
            for (name, value) in &planned.args {
                let _ = writeln!(out, "    {name}={value}");
            }
            outcomes.push(Outcome::Would(planned.key));
            continue;
        }
        let request = planned.args.iter().fold(
            Request::new(
                Verb::Sink,
                Iri::parse(ledger.resource("append")).map_err(|e| e.to_string())?,
            )
            .with_arg("content", inline(&planned.content))
            .with_arg("as", inline(JSON)),
            |request, (name, value)| request.with_arg(*name, inline(value)),
        );
        let answer = answer(&issue(request)?)?;
        let item = answer
            .item
            .display
            .clone()
            .unwrap_or_else(|| answer.item.iri.clone());
        match answer.outcome.as_str() {
            "filed" => {
                let _ = writeln!(out, "filed    {item}  {title}");
                counts.0 += 1;
                outcomes.push(Outcome::Filed(item));
            }
            "existing" => {
                let status = answer.status.as_deref().unwrap_or("?");
                let _ = writeln!(out, "already  {item} ({status})  {}", planned.key);
                counts.1 += 1;
                outcomes.push(Outcome::Already(item));
            }
            other => {
                return Err(format!(
                    "gonk answered the append of {} with the outcome `{other}`, which is \
                     neither `filed` nor `existing`",
                    planned.key
                ))
            }
        }
    }
    for skip in &skipped {
        let _ = writeln!(out, "skipped  {}  {}", skip.id, skip.why);
    }
    let _ = writeln!(
        out,
        "\n{}{} published finding(s) on {}: {} {}, {} already filed, {} skipped",
        if args.dry_run {
            "dry run, nothing filed: "
        } else {
            ""
        },
        rows.len(),
        args.root,
        if args.dry_run {
            outcomes.len()
        } else {
            counts.0
        },
        if args.dry_run {
            "would be filed"
        } else {
            "filed"
        },
        counts.1,
        skipped.len(),
    );
    outcomes.extend(skipped.into_iter().map(Outcome::Skipped));
    Ok(outcomes)
}

/// The ledger's JSON answer, read into its type — the machine contract, checked for its schema.
fn answer(bytes: &[u8]) -> Result<Answer, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| {
        format!(
            "the ledger's append answer is not JSON ({e}): {}",
            String::from_utf8_lossy(bytes)
        )
    })?;
    let schema = value.get("schema").and_then(Value::as_u64);
    if schema != Some(u64::from(SCHEMA)) {
        return Err(format!(
            "the ledger answered in its JSON face schema {}, and this build reads schema \
             {SCHEMA}: rebuild ikigai-gonk against the ledger that server runs",
            schema.map_or_else(|| "(none)".to_string(), |s| s.to_string())
        ));
    }
    serde_json::from_value(value).map_err(|e| {
        format!("the ledger's append answer is not the shape schema {SCHEMA} promises ({e})")
    })
}

fn inline(value: &str) -> ArgRef {
    ArgRef::Inline(value.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The serious set, as a test spells it: these words are a fixture here, not a copy of
    /// the contract's set (the integration test reads the contract's).
    fn policy() -> QueuePolicy {
        QueuePolicy {
            serious: vec!["s1".to_string(), "s2".to_string()],
            configured: true,
        }
    }

    fn row(id: &str, severity: Option<&str>, outcome: Option<&str>, reproduced: bool) -> Value {
        json!({
            "id": id,
            "iri": format!("urn:iki:finding:{id}"),
            "annotates": "urn:repo:demo:file:src/lib.rs",
            "repo": "demo",
            "path": "src/lib.rs",
            "pr": null,
            "body": "It panics on an empty word.\nSecond line.",
            "exact": "word[..1]",
            "line": 3,
            "creator": "a-test-reviewer",
            "severity": severity,
            "effective_severity": severity,
            "decision": outcome.map(|o| json!({
                "outcome": o,
                "decided_at": "2026-10-13T00:00:00Z",
                "note": "initials(\"a  b\") panics",
                "minted": format!("urn:iki:annotation:{id}"),
                "reproduced": reproduced,
            })),
            "judge": {"verdict": "confirmed"},
        })
    }

    fn filter(policy: &QueuePolicy, scope: Scope, reproduced_only: bool) -> Filter<'_> {
        Filter {
            policy,
            scope,
            reproduced_only,
        }
    }

    fn arg<'a>(p: &'a Planned, name: &str) -> &'a str {
        p.args
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("no `{name}` in {:?}", p.args))
    }

    #[test]
    fn a_published_serious_finding_is_keyed_by_its_own_iri() {
        let policy = policy();
        let (planned, skipped) = plan(
            "demo",
            &[row("aa01", Some("s1"), Some("published"), true)],
            filter(&policy, Scope::Serious, false),
        );
        assert!(skipped.is_empty(), "{skipped:?}");
        let p = &planned[0];
        assert_eq!(p.key, "urn:iki:finding:aa01");
        assert_eq!(arg(p, "key"), "urn:iki:finding:aa01");
        assert_eq!(
            arg(p, "about"),
            "urn:repo:demo:file:src/lib.rs urn:iki:finding:aa01 urn:iki:annotation:aa01"
        );
        assert_eq!(arg(p, "labels"), "finding,repo:demo,s1,reproduced");
        assert!(
            p.content
                .starts_with("It panics on an empty word.\n\nIt panics on an empty word."),
            "{}",
            p.content
        );
        for line in [
            "- finding: urn:iki:finding:aa01\n",
            "- file: src/lib.rs line 3\n",
            "- quote: \"word[..1]\"\n",
            "- severity: s1\n",
            "- published: 2026-10-13T00:00:00Z, marked reproduced\n",
            "- annotation: urn:iki:annotation:aa01\n",
            "- judge: confirmed\n",
        ] {
            assert!(p.content.contains(line), "no {line:?} in\n{}", p.content);
        }
    }

    #[test]
    fn only_published_rows_in_the_serious_set_are_filed_by_default() {
        let policy = policy();
        let rows = [
            row("b1", Some("s1"), Some("published"), false),
            row("b2", Some("other"), Some("published"), false),
            row("b3", None, Some("published"), false),
            row("b4", Some("s1"), Some("declined"), false),
            row("b5", Some("s2"), None, false),
        ];
        let (planned, skipped) = plan("demo", &rows, filter(&policy, Scope::Serious, false));
        let filed: Vec<&str> = planned.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(filed, ["b1", "b3"], "serious, and unrated (never hidden)");
        let why: Vec<(&str, &str)> = skipped
            .iter()
            .map(|s| (s.id.as_str(), s.why.as_str()))
            .collect();
        assert!(
            why[0].0 == "b2" && why[0].1.contains("--severity all"),
            "{why:?}"
        );
        assert!(why[1].0 == "b4" && why[1].1.contains("declined"), "{why:?}");
        assert!(why[2].0 == "b5" && why[2].1.contains("none"), "{why:?}");

        let (all, _) = plan("demo", &rows, filter(&policy, Scope::All, false));
        assert_eq!(
            all.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["b1", "b2", "b3"],
            "--severity all files every PUBLISHED one, and still no pending or declined one"
        );
    }

    #[test]
    fn reproduced_narrows_to_the_marked_ones() {
        let policy = policy();
        let rows = [
            row("c1", Some("s1"), Some("published"), true),
            row("c2", Some("s1"), Some("published"), false),
        ];
        let (planned, skipped) = plan("demo", &rows, filter(&policy, Scope::Serious, true));
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].id, "c1");
        assert!(skipped[0].why.contains("reproduced"), "{skipped:?}");
        assert_eq!(
            arg(&planned[0], "labels"),
            "finding,repo:demo,s1,reproduced"
        );
    }

    #[test]
    fn a_human_rerating_is_the_word_used_and_the_proposal_is_said() {
        let policy = policy();
        let mut r = row("d1", Some("other"), Some("published"), false);
        r["effective_severity"] = json!("s2");
        let (planned, _) = plan("demo", &[r], filter(&policy, Scope::Serious, false));
        assert_eq!(arg(&planned[0], "labels"), "finding,repo:demo,s2");
        assert!(planned[0]
            .content
            .contains("- severity: s2 (the model proposed other)\n"));
    }

    #[test]
    fn a_pull_request_finding_is_about_its_pr_and_an_empty_body_still_has_a_title() {
        let policy = policy();
        let mut r = row("e1", Some("s1"), Some("published"), false);
        r["pr"] = json!(42);
        r["annotates"] = json!("urn:repo:demo:pr:42");
        r["body"] = json!("");
        let (planned, _) = plan("demo", &[r], filter(&policy, Scope::Serious, false));
        let p = &planned[0];
        assert!(
            p.content.starts_with("Finding e1\n\n- finding:"),
            "{}",
            p.content
        );
        assert!(p.content.contains("- pull request: 42 in demo\n"));
        assert!(arg(p, "about").starts_with("urn:repo:demo:pr:42 "));
    }

    #[test]
    fn an_about_that_is_not_an_iri_is_left_out_rather_than_split() {
        let policy = policy();
        let mut r = row("f1", Some("s1"), Some("published"), false);
        r["annotates"] = json!("not an iri");
        let (planned, _) = plan("demo", &[r], filter(&policy, Scope::Serious, false));
        assert_eq!(
            arg(&planned[0], "about"),
            "urn:iki:finding:f1 urn:iki:annotation:f1"
        );
    }

    #[test]
    fn the_arguments() {
        let parse = |args: &[&str]| parse_args(args.iter().map(|s| s.to_string()));
        let parsed = parse(&[
            "file",
            "demo",
            "--ledger",
            "reviews",
            "--reproduced",
            "--dry-run",
        ])
        .unwrap();
        assert_eq!(parsed.ledger, "reviews");
        assert_eq!(parsed.scope, Scope::Serious);
        assert!(parsed.reproduced_only && parsed.dry_run);
        assert!(parse(&["file", "demo", "--severity", "some"])
            .unwrap_err()
            .contains("`serious`"));
        assert!(parse(&["file", "demo", "--ledger", "Not A Ledger"]).is_err());
        assert!(
            parse(&["file", "--ledger", "x"]).is_err(),
            "the root comes first"
        );
        assert!(parse(&["list", "demo"]).is_err());
        assert!(parse(&["file", "demo", "--bogus"]).is_err());
    }
}
