//! Holder aliases: one worker loop, one row on the Dashboard's Agents tab, whatever it has signed
//! with (ledger [#3034](http://localhost:1062/l/default/item/3034)).
//!
//! ```toml
//! gonk.holder.alias = "lawnboy -> lawnboy-claude before 2026-10-21"
//! gonk.holder.alias = "lawnboy -> lawnboy-codex"
//! ```
//!
//! ★ **An alias is data about a signature at a time, not a rename.** A name may have meant one loop
//! once and another later, so a rule reads the time of the event that carries the signature. Rules
//! are read in file order and the first that matches decides; a date is the UTC midnight it starts,
//! and `before` excludes it. A name no rule matches is its own row. One step only: `a -> b` and
//! `b -> c` put `a`'s events in `b`'s row and `b`'s in `c`'s, so name the final row in each rule.
//!
//! An item still held is dated by `ledger:claimedAt`, which is when its claim was taken or last
//! renewed: a claim from before a rule's date that is renewed past it counts as the later row, the
//! loop that is holding it now. Every other number reads its event's `dcterms:created`.
//!
//! ★ **Applied where the numbers are composed, and nowhere else.** [`crate::agents`] writes the
//! mapping into the SPARQL it runs, so a number and its link ask one text. The ledger's records keep
//! the names they were signed with: nothing here writes to a ledger.
//!
//! ⚠ A rule that could never match is refused, not skipped: it is a mistake in what the operator
//! believes the table says. So is a target with a space in it, which is how `befor 2026-10-21` would
//! otherwise read: as part of a holder's name, and silently no date at all.

use crate::rules::sparql_string;

const XSD_DATE_TIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

/// The configuration key, for the sentences that name it.
pub const KEY: &str = "gonk.holder.alias";

/// One `gonk.holder.alias` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The name as the ledger's events carry it.
    pub from: String,
    /// The row it counts in.
    pub to: String,
    /// `YYYY-MM-DD`: the rule covers events strictly before that UTC midnight.
    pub before: Option<String>,
}

/// The rules, in file order. Empty is the default, and every question is then its stored text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HolderAliases {
    rules: Vec<Rule>,
}

impl HolderAliases {
    /// The values of the `gonk.holder.alias` lines, in file order. A refusal names the line.
    pub fn parse(lines: &[String]) -> Result<HolderAliases, String> {
        let mut rules: Vec<Rule> = Vec::with_capacity(lines.len());
        for line in lines {
            let refuse = |why: String| format!("{KEY} = \"{line}\": {why}");
            let rule = rule_of(line).map_err(refuse)?;
            let earlier: Vec<&Rule> = rules.iter().filter(|r| r.from == rule.from).collect();
            if earlier.iter().any(|r| r.before.is_none()) {
                return Err(refuse(format!(
                    "an earlier rule with no date already covers all of `{}`, so this one could \
                     never match; first match wins",
                    rule.from
                )));
            }
            let latest = earlier.iter().filter_map(|r| r.before.as_deref()).max();
            if let (Some(latest), Some(before)) = (latest, rule.before.as_deref()) {
                if before <= latest {
                    return Err(refuse(format!(
                        "an earlier rule already covers `{}` before {latest}, so this one could \
                         never match; first match wins",
                        rule.from
                    )));
                }
            }
            rules.push(rule);
        }
        Ok(HolderAliases { rules })
    }

    /// Whether `holder`'s row is anything but the events signed with exactly that name.
    pub fn touches(&self, holder: &str) -> bool {
        self.rules
            .iter()
            .any(|r| r.to == holder || r.from == holder)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Every name an event in `holder`'s row may be signed with: `holder` and what maps to it.
    pub fn signers<'a>(&'a self, holder: &'a str) -> Vec<&'a str> {
        let mut names = vec![holder];
        for rule in self.rules.iter().filter(|r| r.to == holder) {
            if !names.contains(&rule.from.as_str()) {
                names.push(&rule.from);
            }
        }
        names
    }

    /// The SPARQL expression for the row of an event signed `signed` at `at`, both variables. The
    /// rules are nested in order, so the first that matches decides, as the configuration says.
    pub fn row_expression(&self, signed: &str, at: &str) -> String {
        expression(self.rules.iter(), signed, at)
    }

    /// What holds an event to `holder`'s row, in place after its triples: a `VALUES` of the names
    /// it may be signed with, which the store's planner starts from as it would a label in the text,
    /// and the rules' verdict on the one it found.
    pub fn row_clause(&self, holder: &str, signed: &str, at: &str) -> String {
        let signers = self.signers(holder);
        let names: Vec<String> = signers.iter().map(|name| quoted(name)).collect();
        let rules = self
            .rules
            .iter()
            .filter(|rule| signers.contains(&rule.from.as_str()));
        format!(
            "VALUES {signed} {{ {} }}\n  FILTER({} = {})",
            names.join(" "),
            expression(rules, signed, at),
            quoted(holder)
        )
    }

    /// The sentence for a table showing `holders`, or `None` when no rule fills one of its rows. A
    /// viewer hears only of the rows it may already see, not of every loop this server knows.
    pub fn sentence(&self, holders: &[String]) -> Option<String> {
        let said: Vec<String> = self
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| holders.contains(&rule.to))
            .map(|(at, rule)| self.said(at, rule))
            .collect();
        (!said.is_empty()).then(|| {
            format!(
                "Holder aliases apply here: {}. The ledger keeps the names as they were signed.",
                said.join("; ")
            )
        })
    }

    /// One rule in words, with the date it takes over from: the rule before it has the time before.
    fn said(&self, at: usize, rule: &Rule) -> String {
        let from = self.rules[..at]
            .iter()
            .filter(|r| r.from == rule.from)
            .filter_map(|r| r.before.as_deref())
            .max();
        let mut name = rule.from.clone();
        if let Some(from) = from {
            name.push_str(&format!(" from {from}"));
        }
        if let Some(before) = &rule.before {
            name.push_str(&format!(" before {before}"));
        }
        format!("{name} counts as {}", rule.to)
    }
}

fn quoted(label: &str) -> String {
    format!("\"{}\"", sparql_string(label))
}

fn expression<'a>(
    rules: impl DoubleEndedIterator<Item = &'a Rule>,
    signed: &str,
    at: &str,
) -> String {
    rules.rev().fold(signed.to_string(), |otherwise, rule| {
        let name = format!("{signed} = {}", quoted(&rule.from));
        let test = match &rule.before {
            Some(date) => {
                format!("{name} && {at} < \"{date}T00:00:00Z\"^^<{XSD_DATE_TIME}>")
            }
            None => name,
        };
        format!("IF({test}, {}, {otherwise})", quoted(&rule.to))
    })
}

/// `<from> -> <to>` or `<from> -> <to> before <YYYY-MM-DD>`.
fn rule_of(line: &str) -> Result<Rule, String> {
    let shape = "expected `<holder> -> <row>` or `<holder> -> <row> before <YYYY-MM-DD>`";
    let Some((from, rest)) = line.split_once("->") else {
        return Err(format!("no `->`; {shape}"));
    };
    if rest.contains("->") {
        return Err(format!("more than one `->`; {shape}"));
    }
    let (from, rest) = (from.trim(), rest.trim());
    let (to, before) = match rest.rsplit_once(" before ") {
        Some((to, date)) => {
            let date = date.trim();
            if !is_date(date) {
                return Err(format!("`{date}` is not a date in the form YYYY-MM-DD"));
            }
            (to.trim(), Some(date.to_string()))
        }
        None => (rest, None),
    };
    if from.is_empty() || to.is_empty() {
        return Err(format!("a side of the `->` is empty; {shape}"));
    }
    if to.contains(char::is_whitespace) {
        return Err(format!("the row `{to}` has a space in it; {shape}"));
    }
    Ok(Rule {
        from: from.to_string(),
        to: to.to_string(),
        before,
    })
}

/// A real calendar date, spelled `YYYY-MM-DD`.
fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !digits(0..4)
        || !digits(5..7)
        || !digits(8..10)
    {
        return false;
    }
    let number = |range: std::ops::Range<usize>| text[range].parse::<u32>().unwrap_or(0);
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(lines: &[&str]) -> Result<HolderAliases, String> {
        let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        HolderAliases::parse(&lines)
    }

    #[test]
    fn a_rule_reads_its_two_shapes() {
        let read = aliases(&["a -> b before 2026-10-21", "a->c", "two words -> d"]).unwrap();
        assert_eq!(
            read.rules,
            vec![
                Rule {
                    from: "a".into(),
                    to: "b".into(),
                    before: Some("2026-10-21".into())
                },
                Rule {
                    from: "a".into(),
                    to: "c".into(),
                    before: None
                },
                Rule {
                    from: "two words".into(),
                    to: "d".into(),
                    before: None
                },
            ]
        );
    }

    /// Each refusal names the whole line and what is wrong with it.
    #[test]
    fn a_malformed_rule_is_refused_naming_its_line() {
        for (line, says) in [
            ("just-a-name", "no `->`"),
            ("a - b", "no `->`"),
            ("a -> b -> c", "more than one `->`"),
            (" -> b", "empty"),
            ("a -> ", "empty"),
            ("a -> b before tomorrow", "not a date"),
            ("a -> b before 2026-13-01", "not a date"),
            ("a -> b before 2026-02-29", "not a date"),
            ("a -> b before 2026-1-1", "not a date"),
            ("a -> b before 2026-10-21T00:00:00Z", "not a date"),
            ("a -> b befor 2026-10-21", "has a space"),
            ("a -> b after 2026-10-21", "has a space"),
        ] {
            let refused = aliases(&[line]).unwrap_err();
            assert!(refused.contains(says), "{line}: {refused}");
            assert!(
                refused.contains(&format!("gonk.holder.alias = \"{line}\"")),
                "{refused}"
            );
        }
        assert!(aliases(&["a -> b before 2028-02-29"]).is_ok());
    }

    #[test]
    fn a_rule_that_could_never_match_is_refused() {
        let refused = aliases(&["a -> b", "a -> c before 2026-10-21"]).unwrap_err();
        assert!(refused.contains("could never match"), "{refused}");
        assert!(refused.contains("a -> c before 2026-10-21"), "{refused}");
        let refused =
            aliases(&["a -> b before 2026-10-21", "a -> c before 2026-10-21"]).unwrap_err();
        assert!(refused.contains("could never match"), "{refused}");
        assert!(aliases(&["a -> b before 2026-10-21", "a -> c before 2026-11-01"]).is_ok());
        assert!(aliases(&["a -> b", "x -> b"]).is_ok());
    }

    #[test]
    fn the_signers_of_a_row_are_its_name_and_what_maps_to_it() {
        let read = aliases(&["a -> b before 2026-10-21", "a -> c", "d -> c"]).unwrap();
        assert_eq!(read.signers("c"), ["c", "a", "d"]);
        assert_eq!(read.signers("b"), ["b", "a"]);
        assert_eq!(read.signers("elsewhere"), ["elsewhere"]);
        assert!(read.touches("a") && read.touches("c"));
        assert!(!read.touches("elsewhere"));
    }

    /// The first match wins because the rules are nested in file order.
    #[test]
    fn the_expression_tries_the_rules_in_order() {
        let read = aliases(&["a -> b before 2026-10-21", "a -> c"]).unwrap();
        assert_eq!(
            read.row_expression("?s", "?t"),
            "IF(?s = \"a\" && ?t < \"2026-10-21T00:00:00Z\"^^\
             <http://www.w3.org/2001/XMLSchema#dateTime>, \"b\", \
             IF(?s = \"a\", \"c\", ?s))"
        );
    }

    #[test]
    fn a_sentence_names_only_the_rows_it_is_given() {
        let read = aliases(&["a -> b before 2026-10-21", "a -> c", "secret -> hidden"]).unwrap();
        let said = read
            .sentence(&["b".to_string(), "c".to_string(), "other".to_string()])
            .unwrap();
        assert!(
            said.contains("a before 2026-10-21 counts as b; a from 2026-10-21 counts as c."),
            "{said}"
        );
        assert!(
            !said.contains("secret") && !said.contains("hidden"),
            "{said}"
        );
        assert_eq!(read.sentence(&["other".to_string()]), None);
        assert_eq!(HolderAliases::default().sentence(&["b".to_string()]), None);
    }
}
