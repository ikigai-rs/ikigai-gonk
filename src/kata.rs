//! `ikigai-gonk kata import <export.jsonl>` — file a kata export's issues into a gonk ledger.
//!
//! [kata](https://github.com/kenn-io/kata) keeps issues in its own database and writes it out
//! with `kata export` as JSONL. This command reads that file and files each issue as a ledger
//! item over gonk's HTTP door — the same loopback client and the same grant as
//! `roborev file` ([`crate::roborev`]) — then its comments, its close, and finally the links
//! between issues, once every issue has an item to point at.
//!
//! # What `kata export` writes (read from its source, `kenn-io/kata` 2026-10-06)
//!
//! One envelope per line, `{"kind": "<kind>", "data": {…}}` (`internal/jsonl/types.go`), in a
//! fixed kind order: `meta` (first, `export_version`), `project`, …, `issue`, `comment`,
//! `issue_label`, `link`, …, `event`, `purge_log`, `sqlite_sequence`. The four this reads are
//! `internal/db/export_types.go`'s `IssueExport`, `CommentExport`, `IssueLabelExport` and
//! `LinkExport`, plus `ProjectExport` for the project's name:
//!
//! | kind | fields read | |
//! | --- | --- | --- |
//! | `issue` | `id`, `uid` (a 26-character ULID), `project_id`, `short_id`, `title`, `body`, `status` (`open`/`closed`), `closed_reason`, `owner`, `priority` (0 highest … 4 lowest; absent = unset), `author`, `created_at`, `closed_at`, `deleted_at` | ignored: `updated_at`, `metadata`, `revision`, `assignment_expires_on`, recurrence fields |
//! | `comment` | `uid`, `issue_id`, `author`, `teammate`, `body`, `created_at` | |
//! | `issue_label` | `issue_id`, `label` | ignored: who added it, and when |
//! | `link` | `from_issue_uid`, `to_issue_uid`, `type` (`parent`, `blocks`, `related`) | ignored: `author`, `created_at` |
//! | `project` | `id`, `name` | |
//!
//! Every other kind (events, sync bindings, federation state, claims, purge logs, sequences)
//! is counted and reported, not read.
//!
//! # The mapping
//!
//! `ikigai-ledger` took kata's model on purpose, so most of it is one to one: priority 0–4 is
//! the same scale with the same meaning, the five close reasons are the same five words, and
//! the three link types are the same three, in the same direction (`parent` from the child,
//! `blocks` from the prerequisite). kata has no item kind, so none is filed.
//!
//! ⚠ **It is lossy, and every item says so.** The ledger stamps its own filed time and mints
//! its own number, so kata's `created_at` and `short_id` survive only as text: each item's
//! body ends with a provenance line (`Imported from kata <project>#<short_id> (<uid>), filed
//! <created_at> by <author>.`), each comment ends with its own, and a close carries kata's
//! close time as its note. A faithful import, which would keep them as data, is ledger #774.
//!
//! # Idempotence
//!
//! Each item is filed `about urn:kata:issue:<uid>`, and before filing, the ledger is asked for
//! any item, open or closed, about that key — the check `roborev file` makes. An issue already
//! there is not filed again, and the run CONVERGES rather than skipping it: a comment whose
//! marker (`kata comment <uid>`) is not on the item yet is added, an issue closed in kata whose
//! item is still open is closed, and a link the item does not show yet is made. So a re-run of
//! the same export changes nothing, and a later export adds what is new.
//!
//! ⚠ Not converged: an item's title, body, labels and priority are written once, when it is
//! filed; a later edit in kata does not reach it. The check and the append are two requests,
//! so two imports of one export at the same instant can both file an issue (ledger #779 is
//! the atomic append that fixes both this and roborev's case).

use std::collections::{BTreeMap, HashMap};
use std::io::Write;

use ikigai_ledger::Ledger;
use serde_json::Value;

use crate::roborev::{first_item, path_of, url_encode, Door};

/// The idempotence key's prefix: `urn:kata:issue:<uid>`.
pub const KEY_PREFIX: &str = "urn:kata:issue:";

/// Everything `ikigai-gonk kata import` was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportArgs {
    /// The export file.
    pub file: String,
    /// `--gonk`: the HTTP door. Default `http://127.0.0.1:1060`.
    pub gonk: String,
    /// `--ledger`: the ledger to file into. Default `default`.
    pub ledger: String,
    /// `--project`: only this kata project's issues.
    pub project: Option<String>,
    /// `--dry-run`: print what would be filed and touch nothing.
    pub dry_run: bool,
}

/// Parse the arguments after `kata`.
pub fn parse_args(mut args: impl Iterator<Item = String>) -> Result<ImportArgs, String> {
    match args.next().as_deref() {
        Some("import") => {}
        Some(other) => return Err(format!("kata: `{other}` is not a subcommand (import)")),
        None => return Err("kata: expected `import <export.jsonl>`".to_string()),
    }
    let mut out = ImportArgs {
        file: String::new(),
        gonk: "http://127.0.0.1:1060".to_string(),
        ledger: "default".to_string(),
        project: None,
        dry_run: false,
    };
    let mut file = None;
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| format!("kata import: {flag} needs a value"))
        };
        match arg.as_str() {
            "--gonk" => out.gonk = value("--gonk")?,
            "--ledger" => out.ledger = value("--ledger")?,
            "--project" => out.project = Some(value("--project")?),
            "--dry-run" => out.dry_run = true,
            flag if flag.starts_with('-') && flag != "-" => {
                return Err(format!("kata import: unknown argument `{flag}`"))
            }
            path if file.is_none() => file = Some(path.to_string()),
            extra => return Err(format!("kata import: unexpected argument `{extra}`")),
        }
    }
    out.file = file.ok_or("kata import: expected <export.jsonl> (`kata export` writes one)")?;
    Ledger::parse(&out.ledger).map_err(|e| format!("kata import: --ledger: {e}"))?;
    Door::parse(&out.gonk)?;
    Ok(out)
}

/// One kata issue, as much of it as this import reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// kata's database id: what comments and labels point at.
    pub id: i64,
    /// The ULID: the identity, and what links point at.
    pub uid: String,
    /// The display id within its project.
    pub short_id: String,
    /// The project's name, when the export carried it.
    pub project: Option<String>,
    /// The title.
    pub title: String,
    /// The body.
    pub body: String,
    /// `closed`, with kata's reason; `None` while open.
    pub closed: Option<Closed>,
    /// Who owns it in kata.
    pub owner: Option<String>,
    /// 0 highest … 4 lowest.
    pub priority: Option<u8>,
    /// Who filed it.
    pub author: String,
    /// When it was filed in kata.
    pub created_at: String,
    /// Soft-deleted in kata: not imported.
    pub deleted: bool,
    /// Its labels, in export order.
    pub labels: Vec<String>,
    /// Its comments, in export order.
    pub comments: Vec<Comment>,
}

/// A closed issue's reason and time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed {
    /// kata's `closed_reason`, when set.
    pub reason: Option<String>,
    /// kata's `closed_at`.
    pub at: Option<String>,
}

/// One comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// The comment's ULID: the idempotence marker.
    pub uid: String,
    /// The accountable author.
    pub author: String,
    /// The teammate it was written for, when kata recorded one.
    pub teammate: Option<String>,
    /// The text.
    pub body: String,
    /// When.
    pub created_at: String,
}

/// One link between two issues, by ULID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The subject: the child of a `parent`, the prerequisite of a `blocks`.
    pub from: String,
    /// The object.
    pub to: String,
    /// `parent`, `blocks` or `related`.
    pub kind: String,
}

/// An export, read.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Export {
    /// `meta.export_version`: kata's schema version at export.
    pub version: Option<String>,
    /// Every issue, deleted ones included, in export order.
    pub issues: Vec<Issue>,
    /// Every link.
    pub links: Vec<Link>,
    /// Records of kinds this import does not read, counted by kind.
    pub unread: BTreeMap<String, usize>,
}

/// Read a `kata export` file.
///
/// Refuses, naming the line, a line that is not an envelope, an issue missing a field this
/// import needs, and a comment or label naming an issue the export does not carry: a
/// half-understood export filed as a guess would be worse than one not filed at all.
///
/// ```
/// use ikigai_gonk::kata::parse;
///
/// let export = parse(concat!(
///     r#"{"kind":"meta","data":{"key":"export_version","value":"27"}}"#, "\n",
///     r#"{"kind":"issue","data":{"id":1,"uid":"01JZ0000000000000000000ABC","project_id":1,"short_id":"0abc","title":"Ship it","body":"","status":"open","closed_reason":null,"owner":null,"priority":1,"author":"chris","created_at":"2026-10-01T00:00:00.000Z","updated_at":"2026-10-01T00:00:00.000Z","closed_at":null,"deleted_at":null,"metadata":{},"revision":1,"content_revision":0}}"#, "\n",
///     r#"{"kind":"issue_label","data":{"issue_id":1,"label":"bug","author":"chris","created_at":"2026-10-01T00:00:00.000Z"}}"#, "\n",
/// )).unwrap();
/// assert_eq!(export.version.as_deref(), Some("27"));
/// assert_eq!(export.issues[0].priority, Some(1));
/// assert_eq!(export.issues[0].labels, ["bug"]);
/// ```
pub fn parse(text: &str) -> Result<Export, String> {
    let mut export = Export::default();
    let mut projects: HashMap<i64, String> = HashMap::new();
    let mut by_id: HashMap<i64, usize> = HashMap::new();
    for (n, line) in text.lines().enumerate().map(|(n, l)| (n + 1, l.trim())) {
        if line.is_empty() {
            continue;
        }
        let at = |what: String| format!("line {n}: {what}; nothing was imported");
        let envelope: Value =
            serde_json::from_str(line).map_err(|e| at(format!("not JSON ({e})")))?;
        let kind = envelope
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| at("no `kind` — not a kata export envelope".into()))?;
        let data = envelope
            .get("data")
            .ok_or_else(|| at(format!("a `{kind}` record with no `data`")))?;
        let field = |name: &str| -> Result<&Value, String> {
            data.get(name)
                .ok_or_else(|| at(format!("a `{kind}` record with no `{name}`")))
        };
        let string = |name: &str| -> Result<String, String> {
            field(name)?
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| at(format!("`{kind}.{name}` is not a string")))
        };
        let integer = |name: &str| -> Result<i64, String> {
            field(name)?
                .as_i64()
                .ok_or_else(|| at(format!("`{kind}.{name}` is not an integer")))
        };
        let optional = |name: &str| -> Option<String> {
            data.get(name)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        match kind {
            "meta" => {
                if data.get("key").and_then(Value::as_str) == Some("export_version") {
                    export.version = optional("value");
                }
            }
            "project" => {
                projects.insert(integer("id")?, string("name")?);
            }
            "issue" => {
                let id = integer("id")?;
                let priority = match data.get("priority") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(priority(value).map_err(at)?),
                };
                let closed = match string("status")?.as_str() {
                    "open" => None,
                    "closed" => Some(Closed {
                        reason: optional("closed_reason"),
                        at: optional("closed_at"),
                    }),
                    other => {
                        return Err(at(format!("issue status `{other}` is not open or closed")))
                    }
                };
                let issue = Issue {
                    id,
                    uid: string("uid")?,
                    short_id: string("short_id")?,
                    project: data
                        .get("project_id")
                        .and_then(Value::as_i64)
                        .and_then(|p| projects.get(&p).cloned()),
                    title: string("title")?,
                    body: string("body").unwrap_or_default(),
                    closed,
                    owner: optional("owner"),
                    priority,
                    author: string("author")?,
                    created_at: string("created_at")?,
                    deleted: optional("deleted_at").is_some(),
                    labels: Vec::new(),
                    comments: Vec::new(),
                };
                by_id.insert(id, export.issues.len());
                export.issues.push(issue);
            }
            "comment" => {
                let issue = integer("issue_id")?;
                let comment = Comment {
                    uid: string("uid")?,
                    author: string("author")?,
                    teammate: optional("teammate"),
                    body: string("body")?,
                    created_at: string("created_at")?,
                };
                let index = by_id.get(&issue).ok_or_else(|| {
                    at(format!(
                        "a comment on issue id {issue}, which the export does not carry"
                    ))
                })?;
                export.issues[*index].comments.push(comment);
            }
            "issue_label" => {
                let issue = integer("issue_id")?;
                let label = string("label")?;
                let index = by_id.get(&issue).ok_or_else(|| {
                    at(format!(
                        "a label on issue id {issue}, which the export does not carry"
                    ))
                })?;
                export.issues[*index].labels.push(label);
            }
            "link" => export.links.push(Link {
                from: string("from_issue_uid")?,
                to: string("to_issue_uid")?,
                kind: string("type")?,
            }),
            other => *export.unread.entry(other.to_string()).or_default() += 1,
        }
    }
    if export.version.is_none() {
        return Err(
            "no `meta` record carries `export_version`: this is not a `kata export` file, \
             which always opens with one; nothing was imported"
                .to_string(),
        );
    }
    Ok(export)
}

/// kata's priority, mapped explicitly onto the ledger's: both are 0 (highest) to 4 (lowest)
/// with absent meaning unset, so the map is the identity — written out, so that a change on
/// either side is a refusal here rather than a silent shift.
fn priority(value: &Value) -> Result<u8, String> {
    match value.as_i64() {
        Some(0) => Ok(0),
        Some(1) => Ok(1),
        Some(2) => Ok(2),
        Some(3) => Ok(3),
        Some(4) => Ok(4),
        _ => Err(format!(
            "issue priority {value} is outside kata's 0..4, which the ledger shares"
        )),
    }
}

/// kata's close reasons, mapped onto the ledger's: the same five words.
fn close_reason(reason: &str) -> Option<&'static str> {
    match reason {
        "done" => Some("done"),
        "wontfix" => Some("wontfix"),
        "duplicate" => Some("duplicate"),
        "superseded" => Some("superseded"),
        "audit-no-change" => Some("audit-no-change"),
        _ => None,
    }
}

/// kata's link types, mapped onto the ledger's: the same three, in the same direction.
fn link_type(kind: &str) -> Option<&'static str> {
    match kind {
        "parent" => Some("parent"),
        "blocks" => Some("blocks"),
        "related" => Some("related"),
        _ => None,
    }
}

/// `urn:kata:issue:<uid>`.
pub fn key(uid: &str) -> String {
    format!("{KEY_PREFIX}{uid}")
}

/// The marker a comment carries, by which a re-run knows it is there.
pub fn comment_marker(uid: &str) -> String {
    format!("kata comment {uid}")
}

/// One issue's append: `content` and the other arguments.
pub fn append_for(issue: &Issue) -> (String, Vec<(&'static str, String)>) {
    let title = issue.title.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut content = format!("{title}\n\n");
    let body = issue.body.trim();
    if !body.is_empty() {
        content.push_str(body);
        content.push_str("\n\n");
    }
    content.push_str(&format!(
        "Imported from kata {}#{} ({}), filed {} by {}",
        issue.project.as_deref().unwrap_or("?"),
        issue.short_id,
        issue.uid,
        issue.created_at,
        issue.author
    ));
    if let Some(owner) = &issue.owner {
        content.push_str(&format!("; owner {owner}"));
    }
    content.push_str(".\n");
    let mut args = vec![("about", key(&issue.uid)), ("author", issue.author.clone())];
    if let Some(priority) = issue.priority {
        args.push(("priority", priority.to_string()));
    }
    if !issue.labels.is_empty() {
        args.push(("labels", issue.labels.join(",")));
    }
    (content, args)
}

/// One comment's text, with its marker.
fn comment_text(comment: &Comment) -> String {
    let mut text = format!(
        "{}\n\n({}, {}",
        comment.body.trim(),
        comment_marker(&comment.uid),
        comment.created_at
    );
    if let Some(teammate) = &comment.teammate {
        text.push_str(&format!(", for {teammate}"));
    }
    text.push(')');
    text
}

/// What one run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Items filed now.
    pub filed: usize,
    /// Issues already in the ledger.
    pub already: usize,
    /// Comments added.
    pub comments: usize,
    /// Items closed.
    pub closed: usize,
    /// Links made.
    pub links: usize,
    /// Deleted issues skipped.
    pub deleted: usize,
    /// Links skipped: an end not imported, or a type the ledger has no word for.
    pub links_skipped: usize,
}

/// An item, as the door shows it.
struct Item {
    /// `#12` or `acme#12`.
    short: String,
    /// Its IRI.
    iri: String,
    /// Its detail text: status, links, comments.
    text: String,
}

impl Item {
    fn is_open(&self) -> bool {
        self.text.split_whitespace().nth(1) == Some("open")
    }

    /// Whether the detail shows `<kind>: <to>`.
    fn has_link(&self, kind: &str, to: &str) -> bool {
        self.text.lines().any(|line| {
            let line = line.trim();
            line.strip_prefix(kind)
                .and_then(|rest| rest.strip_prefix(':'))
                .is_some_and(|rest| rest.trim() == to)
        })
    }
}

/// Run `kata import`. Prints one line per thing done to `out` and answers the tally. A
/// refusal or an unreachable door stops it with an error; what was filed before that stays
/// filed, and running it again carries on from there.
pub fn run(args: &ImportArgs, out: &mut impl Write) -> Result<Summary, String> {
    let text =
        std::fs::read_to_string(&args.file).map_err(|e| format!("reading {}: {e}", args.file))?;
    let export = parse(&text)?;
    let door = Door::parse(&args.gonk)?;
    let ledger = Ledger::parse(&args.ledger).map_err(|e| e.to_string())?;
    let mut summary = Summary::default();
    let _ = writeln!(
        out,
        "kata export version {}: {} issue(s), {} link(s)",
        export.version.as_deref().unwrap_or("?"),
        export.issues.len(),
        export.links.len()
    );
    let wanted = |issue: &Issue| match &args.project {
        Some(project) => issue.project.as_deref() == Some(project.as_str()),
        None => true,
    };
    // kata uid -> the item it became (or already was).
    let mut items: HashMap<String, Item> = HashMap::new();
    for issue in export.issues.iter().filter(|i| wanted(i)) {
        if issue.deleted {
            summary.deleted += 1;
            let _ = writeln!(out, "deleted   kata {}  not imported", issue.short_id);
            continue;
        }
        let (content, append) = append_for(issue);
        if args.dry_run {
            let _ = writeln!(
                out,
                "would file  kata {}  {}",
                issue.short_id,
                first_line(&content)
            );
            for (name, value) in &append {
                let _ = writeln!(out, "    {name}={value}");
            }
            let _ = writeln!(
                out,
                "    then {} comment(s){}",
                issue.comments.len(),
                if issue.closed.is_some() {
                    ", close"
                } else {
                    ""
                }
            );
            continue;
        }
        let query = format!("?about={}&status=all&limit=1", url_encode(&key(&issue.uid)));
        let listing = door.call("GET", &path_of(&ledger, "items"), &query, "")?;
        let item = match first_item(&listing) {
            Some(short) => {
                summary.already += 1;
                let item = read_item(&door, &ledger, &short)?;
                let _ = writeln!(out, "already   {:<8} kata {}", item.short, issue.short_id);
                item
            }
            None => {
                let query = encode(&append);
                let filed = door.call("POST", &path_of(&ledger, "append"), &query, &content)?;
                let short = filed.split_whitespace().next().unwrap_or("?").to_string();
                summary.filed += 1;
                let _ = writeln!(
                    out,
                    "filed     {short:<8} kata {}  {}",
                    issue.short_id,
                    first_line(&content)
                );
                read_item(&door, &ledger, &short)?
            }
        };
        for comment in &issue.comments {
            if item.text.contains(&comment_marker(&comment.uid)) {
                continue;
            }
            let query = encode(&[
                ("item", item.iri.clone()),
                ("author", comment.author.clone()),
            ]);
            door.call(
                "POST",
                &path_of(&ledger, "comment"),
                &query,
                &comment_text(comment),
            )?;
            summary.comments += 1;
            let _ = writeln!(
                out,
                "commented {:<8} kata comment {}",
                item.short, comment.uid
            );
        }
        if let (Some(closed), true) = (&issue.closed, item.is_open()) {
            let mut close = vec![("item", item.iri.clone())];
            match closed.reason.as_deref().map(|r| (r, close_reason(r))) {
                Some((_, Some(reason))) => close.push(("reason", reason.to_string())),
                // An unknown reason closes as the ledger's default (done) and says so.
                Some((other, None)) => {
                    let _ = writeln!(
                        out,
                        "note      {:<8} kata close reason `{other}` has no ledger word; closed as done",
                        item.short
                    );
                }
                None => {}
            }
            let note = format!(
                "Closed in kata{}{}.",
                closed
                    .at
                    .as_deref()
                    .map(|at| format!(" at {at}"))
                    .unwrap_or_default(),
                closed
                    .reason
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            );
            door.call("POST", &path_of(&ledger, "close"), &encode(&close), &note)?;
            summary.closed += 1;
            let _ = writeln!(out, "closed    {:<8} kata {}", item.short, issue.short_id);
        }
        // No re-read: comments and a close add no link, and the link pass reads only links.
        items.insert(issue.uid.clone(), item);
    }
    if args.dry_run {
        let imported = |uid: &str| {
            export
                .issues
                .iter()
                .any(|i| i.uid == uid && !i.deleted && wanted(i))
        };
        for link in &export.links {
            let verb = match link_type(&link.kind) {
                Some(_) if imported(&link.from) && imported(&link.to) => "would link",
                _ => "would skip",
            };
            let _ = writeln!(out, "{verb}  {} {} {}", link.from, link.kind, link.to);
        }
        return Ok(summary);
    }
    // The second pass: every end exists now, or was not imported.
    for link in &export.links {
        let (Some(from), Some(to), Some(kind)) = (
            items.get(&link.from),
            items.get(&link.to),
            link_type(&link.kind),
        ) else {
            summary.links_skipped += 1;
            let _ = writeln!(
                out,
                "skipped   link {} {} {}: an end was not imported, or the type is not one the ledger has",
                link.from, link.kind, link.to
            );
            continue;
        };
        if from.has_link(kind, &to.iri) {
            continue;
        }
        let query = encode(&[("item", from.iri.clone()), ("type", kind.to_string())]);
        door.call("POST", &path_of(&ledger, "link"), &query, &to.iri)?;
        summary.links += 1;
        let _ = writeln!(out, "linked    {} {kind} {}", from.short, to.short);
    }
    let unread = export
        .unread
        .iter()
        .map(|(kind, n)| format!("{kind} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        out,
        "\n{} filed, {} already there; {} comment(s), {} close(s), {} link(s) added; {} deleted \
         issue(s) and {} link(s) skipped{}",
        summary.filed,
        summary.already,
        summary.comments,
        summary.closed,
        summary.links,
        summary.deleted,
        summary.links_skipped,
        if unread.is_empty() {
            String::new()
        } else {
            format!("; not read: {unread}")
        }
    );
    Ok(summary)
}

/// `GET` an item's detail by its short form (`#12`, `acme#12`).
fn read_item(door: &Door, ledger: &Ledger, short: &str) -> Result<Item, String> {
    let number = short
        .rsplit_once('#')
        .map(|(_, n)| n)
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| format!("gonk answered `{short}`, which is not an item number"))?;
    let text = door.call("GET", &path_of(ledger, &format!("item:{number}")), "", "")?;
    let iri = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("iri:"))
        .map(|iri| iri.trim().to_string())
        .ok_or_else(|| format!("gonk's item {short} names no IRI"))?;
    Ok(Item {
        short: short.to_string(),
        iri,
        text,
    })
}

fn encode(args: &[(&str, String)]) -> String {
    let query = args
        .iter()
        .map(|(name, value)| format!("{name}={}", url_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("?{query}")
}

fn first_line(content: &str) -> &str {
    content.lines().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: &str = "01JZ0000000000000000000ABC";

    fn issue_line(extra: &str) -> String {
        format!(
            r#"{{"kind":"issue","data":{{"id":1,"uid":"{UID}","project_id":1,"short_id":"0abc","title":"Ship  it","body":"Body.","status":"open","closed_reason":null,"owner":null,"author":"chris","created_at":"2026-10-01T00:00:00.000Z","updated_at":"2026-10-01T00:00:00.000Z","closed_at":null,"deleted_at":null{extra}}}}}"#
        )
    }

    fn export(lines: &[&str]) -> Result<Export, String> {
        let mut text =
            String::from(r#"{"kind":"meta","data":{"key":"export_version","value":"27"}}"#);
        text.push('\n');
        text.push_str(r#"{"kind":"project","data":{"id":1,"uid":"01JZ00000000000000000PROJ1","name":"gonk","created_at":"x","metadata":{},"revision":1}}"#);
        text.push('\n');
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        parse(&text)
    }

    #[test]
    fn the_append_carries_the_provenance_and_the_key() {
        let parsed = export(&[&issue_line(r#","priority":0,"owner":"wensel""#)]).unwrap();
        let (content, args) = append_for(&parsed.issues[0]);
        assert_eq!(
            content,
            format!(
                "Ship it\n\nBody.\n\nImported from kata gonk#0abc ({UID}), filed \
                 2026-10-01T00:00:00.000Z by chris; owner wensel.\n"
            )
        );
        assert!(args.contains(&("about", format!("urn:kata:issue:{UID}"))));
        assert!(args.contains(&("priority", "0".to_string())));
        assert!(args.contains(&("author", "chris".to_string())));
        assert!(
            !args.iter().any(|(n, _)| *n == "labels"),
            "no labels, no arg"
        );
    }

    #[test]
    fn priorities_outside_the_shared_scale_are_refused() {
        assert_eq!(
            export(&[&issue_line(r#","priority":4"#)]).unwrap().issues[0].priority,
            Some(4)
        );
        assert_eq!(export(&[&issue_line("")]).unwrap().issues[0].priority, None);
        assert!(export(&[&issue_line(r#","priority":5"#)])
            .unwrap_err()
            .contains("outside kata's 0..4"));
    }

    #[test]
    fn a_file_that_is_not_an_export_is_refused() {
        assert!(parse("{\"kind\":\"issue\"}\n")
            .unwrap_err()
            .contains("no `data`"));
        assert!(parse("").unwrap_err().contains("export_version"));
        assert!(parse("not json\n").unwrap_err().contains("line 1"));
        assert!(export(&[r#"{"kind":"comment","data":{"uid":"c","issue_id":9,"author":"a","body":"b","created_at":"t"}}"#])
            .unwrap_err()
            .contains("issue id 9"));
    }

    #[test]
    fn the_maps_are_explicit() {
        for reason in [
            "done",
            "wontfix",
            "duplicate",
            "superseded",
            "audit-no-change",
        ] {
            assert_eq!(close_reason(reason), Some(reason));
        }
        assert_eq!(close_reason("abandoned"), None);
        for kind in ["parent", "blocks", "related"] {
            assert_eq!(link_type(kind), Some(kind));
        }
        assert_eq!(
            link_type("blocked_by"),
            None,
            "kata stores blocks, never blocked_by"
        );
    }

    #[test]
    fn an_item_shows_its_status_and_links() {
        let item = Item {
            short: "#3".into(),
            iri: "urn:iki:ledger:default:item:a".into(),
            text: "   #3  open    p1  T\n  iri:      urn:iki:ledger:default:item:a\n  \
                   blocks:   urn:iki:ledger:default:item:b\n"
                .into(),
        };
        assert!(item.is_open());
        assert!(item.has_link("blocks", "urn:iki:ledger:default:item:b"));
        assert!(!item.has_link("parent", "urn:iki:ledger:default:item:b"));
        assert!(!item.has_link("blocks", "urn:iki:ledger:default:item:c"));
    }

    #[test]
    fn the_arguments() {
        let parsed = parse_args(["import", "x.jsonl"].into_iter().map(String::from)).unwrap();
        assert_eq!(parsed.ledger, "default");
        assert_eq!(parsed.gonk, "http://127.0.0.1:1060");
        assert!(parse_args(["import"].into_iter().map(String::from))
            .unwrap_err()
            .contains("expected <export.jsonl>"));
        assert!(parse_args(["export"].into_iter().map(String::from)).is_err());
        assert!(parse_args(
            ["import", "x", "--gonk", "http://gonk.example:1060"]
                .into_iter()
                .map(String::from)
        )
        .is_err());
    }
}
