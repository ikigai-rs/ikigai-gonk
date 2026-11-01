//! What an OFFER may carry out of this server (ledger
//! [#1745](http://localhost:1062/l/default/item/1745)): the content gate the mirror runs on the
//! title and the brief of every offer before it is dropped into a layer.
//!
//! An offer's brief is its item's BODY ([`crate::mirror`]), and a layer is read by another
//! party. `urn:jsonld:prune` and the layer host's closed shapes decide which TERMS cross; nothing
//! looked at what the body SAID, so a token pasted into an item's body crossed with it. This gate
//! refuses to offer an item whose title or body carries:
//!
//! - a credential shape ([`secret`]): a GitHub token (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`,
//!   `github_pat_`), an `sk-` API key, an AWS access key id (`AKIA`, `ASIA`), a PEM private key
//!   (`-----BEGIN … PRIVATE KEY-----`), or a bearer token (`Bearer <token>`);
//! - a line of the operator's own deny file ([`DenyList`], `gonk.layer.deny = "<path>"`), for
//!   the words that are this server's business to keep and no business of its public code.
//!
//! A refusal names the KIND of what matched (or the deny file's line number), never the matched
//! text: the reason is written on the item as a comment, and the reason must not repeat the
//! secret it is withholding.
//!
//! ★ This is a gate against an ACCIDENT on our own side (a token pasted into an item, a word that
//! is not for this collaboration), not against an adversary writing our ledger. It matches
//! shapes, so a credential spelled out of shape passes; the dispatcher still decides what a
//! shared ledger holds.

use std::path::Path;

/// The kind of credential `text` carries, when it carries one: `github-token`, `api-key`,
/// `aws-access-key`, `private-key` or `bearer-token`. Never the match itself.
///
/// A prefix counts only at the start of a word (`desk-…` is not an `sk-` key) and only with a
/// token's worth of characters after it, so prose that NAMES a format ("a `ghp_` token") passes.
///
/// ```
/// use ikigai_gonk::egress::secret;
/// assert_eq!(secret("token ghp_0123456789abcdefghijABCDEFGHIJ0123456789"), Some("github-token"));
/// assert_eq!(secret("github_pat_11ABCDEFG0123456789_abcdefghijklmnop"), Some("github-token"));
/// assert_eq!(secret("key=sk-ant-api03-abcdefghijklmnopqrstuvwxyz"), Some("api-key"));
/// assert_eq!(secret("AKIAIOSFODNN7EXAMPLE"), Some("aws-access-key"));
/// assert_eq!(secret("-----BEGIN OPENSSH PRIVATE KEY-----\nb3Bl"), Some("private-key"));
/// assert_eq!(secret("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.e30.abc"), Some("bearer-token"));
/// // Prose about the formats, and words that merely contain a prefix, pass.
/// assert_eq!(secret("rotate the ghp_ tokens; see the sk- keys"), None);
/// assert_eq!(secret("a risk-free desk-top task-list"), None);
/// assert_eq!(secret("-----BEGIN PUBLIC KEY-----"), None);
/// assert_eq!(secret("the bearer of bad news"), None);
/// ```
pub fn secret(text: &str) -> Option<&'static str> {
    let alnum = |b: u8| b.is_ascii_alphanumeric();
    let pat = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let key = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    let aws = |b: u8| b.is_ascii_uppercase() || b.is_ascii_digit();
    let checks: [Check; 9] = [
        ("ghp_", 20, alnum, "github-token"),
        ("gho_", 20, alnum, "github-token"),
        ("ghu_", 20, alnum, "github-token"),
        ("ghs_", 20, alnum, "github-token"),
        ("ghr_", 20, alnum, "github-token"),
        ("github_pat_", 20, pat, "github-token"),
        ("sk-", 20, key, "api-key"),
        ("AKIA", 16, aws, "aws-access-key"),
        ("ASIA", 16, aws, "aws-access-key"),
    ];
    for (prefix, min, allowed, kind) in checks {
        if prefixed_token(text, prefix, min, allowed) {
            return Some(kind);
        }
    }
    if private_key(text) {
        return Some("private-key");
    }
    if bearer(text) {
        return Some("bearer-token");
    }
    None
}

/// One credential shape: its prefix, the fewest token bytes after it, which bytes those are,
/// and the kind it names.
type Check = (&'static str, usize, fn(u8) -> bool, &'static str);

/// Whether `text` holds `prefix` at the start of a word followed by at least `min` bytes
/// `allowed` admits.
fn prefixed_token(text: &str, prefix: &str, min: usize, allowed: fn(u8) -> bool) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(prefix).any(|(at, _)| {
        let starts_word = at == 0 || !word_byte(bytes[at - 1]);
        let run = bytes[at + prefix.len()..]
            .iter()
            .take_while(|b| allowed(**b))
            .count();
        starts_word && run >= min
    })
}

/// A byte that continues a word (so a prefix after it is inside the word, not at its start).
fn word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// `-----BEGIN … PRIVATE KEY-----` on one line: RSA, EC, OPENSSH, ENCRYPTED, or none.
fn private_key(text: &str) -> bool {
    text.match_indices("-----BEGIN ").any(|(at, _)| {
        let line = text[at..].lines().next().unwrap_or("");
        line.contains("PRIVATE KEY-----")
    })
}

/// `Bearer` (any case) at the start of a word, whitespace, then at least 20 characters of a
/// token (RFC 6750's `b64token`: letters, digits, `-._~+/`, then `=` padding).
fn bearer(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    lower.match_indices("bearer").any(|(at, word)| {
        if at > 0 && word_byte(bytes[at - 1]) {
            return false;
        }
        let rest = &bytes[at + word.len()..];
        let space = rest
            .iter()
            .take_while(|b| **b == b' ' || **b == b'\t')
            .count();
        if space == 0 {
            return false;
        }
        let token = rest[space..]
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(b))
            .count();
        token >= 20
    })
}

/// The operator's deny file (`gonk.layer.deny = "<path>"`): one pattern per line, matched as a
/// case-insensitive substring of an offer's title and brief. Blank lines and lines starting with
/// `#` are skipped.
///
/// ★ The words are the operator's, kept in a file of theirs: a word that must not leave this
/// server is often a word that must not appear in its public source either. A match is reported
/// by its LINE NUMBER, never by the word.
///
/// ```
/// use ikigai_gonk::egress::DenyList;
/// let deny = DenyList::parse("# topics that stay home\n\nProject Lantern\nskunkworks\n");
/// assert_eq!(deny.len(), 2);
/// assert_eq!(deny.matches("Ship the PROJECT LANTERN demo"), Some(3));
/// assert_eq!(deny.matches("the skunkworks budget"), Some(4));
/// assert_eq!(deny.matches("Port the importer"), None);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DenyList {
    /// `(line number, lowercased pattern)`.
    patterns: Vec<(usize, String)>,
}

impl DenyList {
    /// The patterns in `text`, a deny file's content.
    pub fn parse(text: &str) -> DenyList {
        let patterns = text
            .lines()
            .enumerate()
            .map(|(i, line)| (i + 1, line.trim()))
            .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
            .map(|(n, line)| (n, line.to_lowercase()))
            .collect();
        DenyList { patterns }
    }

    /// Read the deny file at `path`.
    ///
    /// # Errors
    ///
    /// When it cannot be read or is not UTF-8: a deny file that is named and missing must stop
    /// the server, never be read as "deny nothing".
    pub fn load(path: &Path) -> Result<DenyList, String> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "gonk.layer.deny: {} cannot be read ({e}); a deny file that is named is required",
                path.display()
            )
        })?;
        Ok(DenyList::parse(&text))
    }

    /// How many patterns it holds.
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// The line number of the first pattern `text` contains, ignoring case.
    pub fn matches(&self, text: &str) -> Option<usize> {
        let text = text.to_lowercase();
        self.patterns
            .iter()
            .find(|(_, pattern)| text.contains(pattern.as_str()))
            .map(|(n, _)| *n)
    }
}

/// Why an offer is withheld: the comment-key part (stable for one reason, so the item is told
/// once) and the sentence for the item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Withheld {
    /// `secret:{kind}` or `deny:{line}`: part of the key the reason is written under.
    pub reason: String,
    /// What the item is told.
    pub why: String,
}

/// The gate: whether an offer whose title is `title` and brief `brief` must not leave, and why.
///
/// ```
/// use ikigai_gonk::egress::{withheld, DenyList};
/// let deny = DenyList::parse("lantern\n");
/// let w = withheld("Fix it", "use ghp_0123456789abcdefghijABCDEFGHIJ0123456789", None).unwrap();
/// assert_eq!(w.reason, "secret:github-token");
/// assert!(!w.why.contains("ghp_0123"), "the reason never repeats the secret");
/// let w = withheld("The Lantern demo", "body", Some(&deny)).unwrap();
/// assert_eq!(w.reason, "deny:1");
/// assert!(!w.why.to_lowercase().contains("lantern"));
/// assert_eq!(withheld("Port the importer", "It reads the old format.", Some(&deny)), None);
/// ```
pub fn withheld(title: &str, brief: &str, deny: Option<&DenyList>) -> Option<Withheld> {
    for (field, text) in [("title", title), ("body", brief)] {
        if let Some(kind) = secret(text) {
            return Some(Withheld {
                reason: format!("secret:{kind}"),
                why: format!(
                    "its {field} carries what looks like a credential ({kind}), and an offer \
                     publishes the title and the body to the layer"
                ),
            });
        }
        if let Some(line) = deny.and_then(|d| d.matches(text)) {
            return Some(Withheld {
                reason: format!("deny:{line}"),
                why: format!(
                    "its {field} matches line {line} of this server's deny file \
                     (gonk.layer.deny), and an offer publishes the title and the body to the layer"
                ),
            });
        }
    }
    None
}

/// `text` with every link to an item of a ledger on THIS machine rewritten as words naming the
/// ledger and number (ledger [#2810](http://localhost:1062/l/default/item/2810)).
///
/// Every gonk binds `127.0.0.1:1062` by default, so `http://localhost:1062/l/default/item/403`
/// in a brief opens the TAKER's default ledger, item 403, on the taker's machine: the wrong item,
/// looking authoritative. A link whose host is a loopback name (`localhost`, `127.*`, `[::1]`,
/// `0.0.0.0`) and whose path is `/l/{ledger}/item/{n}` becomes `the offerer's {ledger} ledger
/// #{n}`; a Markdown link `[label](…)` to one keeps its label, `label (the offerer's …)`, and a
/// relative `[label](/l/{ledger}/item/{n})` is read the same way. An autolink's `<…>` goes with
/// it. Every other link is left as it is.
///
/// ★ A bare `#403` is not touched: it is as likely a pull request as an item, and the brief
/// writer's to spell out ("ikigai ledger #403").
///
/// ```
/// use ikigai_gonk::egress::local_links;
/// assert_eq!(
///     local_links("See http://localhost:1062/l/default/item/403."),
///     "See the offerer's default ledger #403."
/// );
/// assert_eq!(
///     local_links("[#12](http://127.0.0.1:1062/l/crew/item/12) and <http://[::1]/l/crew/item/7>"),
///     "#12 (the offerer's crew ledger #12) and the offerer's crew ledger #7"
/// );
/// assert_eq!(
///     local_links("[the plan](/l/crew/item/9#c3)"),
///     "the plan (the offerer's crew ledger #9)"
/// );
/// // Other hosts, other paths, and bare numbers pass.
/// let kept = "https://github.com/o/r/pull/3, http://localhost:1062/l/crew, #403";
/// assert_eq!(local_links(kept), kept);
/// ```
pub fn local_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let markdown = out.ends_with("](");
        let Some((len, words)) = local_link(rest, markdown) else {
            let c = rest.chars().next().unwrap_or_default();
            out.push(c);
            at += c.len_utf8();
            continue;
        };
        let after = &rest[len..];
        if markdown && after.starts_with(')') {
            // `[label](link)` → `label (words)`; the label is everything since its `[`.
            out.truncate(out.len() - 2);
            let open = out.rfind('[').unwrap_or(out.len());
            let label = out[open..].trim_start_matches('[').to_string();
            out.truncate(open);
            out.push_str(&format!("{label} ({words})"));
            at += len + 1;
        } else if out.ends_with('<') && after.starts_with('>') {
            out.pop();
            out.push_str(&words);
            at += len + 1;
        } else {
            out.push_str(&words);
            at += len;
        }
    }
    out
}

/// A local ledger link at the start of `text`: its length, and the words that replace it. A
/// relative `/l/…` link counts only as a Markdown link's target (`relative`).
fn local_link(text: &str, relative: bool) -> Option<(usize, String)> {
    let path = if relative && text.starts_with("/l/") {
        0
    } else {
        let after_scheme = text
            .strip_prefix("http://")
            .or_else(|| text.strip_prefix("https://"))?;
        let scheme = text.len() - after_scheme.len();
        let host_len = after_scheme
            .find(|c: char| c == '/' || c.is_whitespace())
            .unwrap_or(after_scheme.len());
        if !loopback(&after_scheme[..host_len]) {
            return None;
        }
        scheme + host_len
    };
    let tail = text[path..].strip_prefix("/l/")?;
    let ledger_len = tail.find('/')?;
    let ledger = &tail[..ledger_len];
    if ledger.is_empty() || !ledger.bytes().all(ledger_byte) {
        return None;
    }
    let number_part = tail[ledger_len..].strip_prefix("/item/")?;
    let digits = number_part.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let number = &number_part[..digits];
    // A fragment, query or trailing slash belongs to the link too.
    let end = number_part[digits..]
        .find(|c: char| c.is_whitespace() || "()<>[]\"'`".contains(c))
        .unwrap_or(number_part.len() - digits);
    let mut extra = &number_part[digits..digits + end];
    // Sentence punctuation after the link is the sentence's.
    while extra.ends_with(['.', ',', ';', ':', '!']) {
        extra = &extra[..extra.len() - 1];
    }
    if !matches!(extra.chars().next(), None | Some('#' | '?' | '/')) {
        return None;
    }
    let len = text.len() - number_part.len() + digits + extra.len();
    Some((len, format!("the offerer's {ledger} ledger #{number}")))
}

/// A byte a ledger's path segment may hold (percent-encoded names included).
fn ledger_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_.%~".contains(&b)
}

/// Whether `authority` (`host[:port]`) names this machine.
fn loopback(authority: &str) -> bool {
    let host = if let Some(v6) = authority.strip_prefix('[') {
        match v6.split_once(']') {
            Some((host, port)) if port.is_empty() || port.starts_with(':') => host,
            _ => return false,
        }
    } else {
        authority.split(':').next().unwrap_or("")
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host == "0.0.0.0"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}
