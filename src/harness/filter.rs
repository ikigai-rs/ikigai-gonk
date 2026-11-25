//! Which rules the Harness page shows (ledger #3787): the filters on `/harness`, the counts on
//! their choices, the repository picker, and the query that carries the filters into every link.
//!
//! A rule is a guide or a control (`harness:Guide`, `harness:Control`). Each filter is an
//! optional query parameter, named in the singular as the ledger listing's are, and may be
//! repeated:
//!
//! ```text
//! pack=        the pack a rule came from, by its name; `-` for the repository's own harness
//! category=    harness:category, by the term's local name (authoritySafety)
//! stage=       harness:stage (toolCall)
//! direction=   harness:direction: feedforward (a guide) or feedback (a sensor)
//! execution=   harness:execution: computational or inferential
//! enforced=    harness:enforcedAt, the literal (tools/claim_hook.py#main)
//! fired=       fired, never, refused, warned: over the whole firing log, the page's day window
//! gap=         unchecked-guide (a row of guides-without-sensor), unstated-control (a row of
//!              sensors-without-guide): the coverage questions' own answers
//! q=           words in the rule's label or description, every word of one value
//! ```
//!
//! Filters of different kinds narrow (AND); repeats of one kind widen (OR). A repeat reaches
//! the resource joined by a line break: the inputs are declared newline-separated lists, so the
//! HTTP door joins `category=a&category=b` for them and a value may hold a comma.
//!
//! The counts are worked out over the harness this request already loaded, as the ledger
//! listing's facet counts are (`src/web/facet.rs`): each choice says how many rules it would
//! match with the other kinds' filters kept. That is arithmetic over a few hundred rules held in
//! memory, so they are drawn with the page rather than fetched after it.

use super::*;
use ikigai_core::ListSeparator;

/// The value of `pack=` for a rule of the repository's own harness: never a pack's name, since
/// a pack's name is lowercase kebab-case words.
pub const OWN: &str = "-";

/// The one coverage question whose rows are not rules: a filter leaves it whole.
const UNFILTERED: &str = "incidents-without-sensor";

/// The vocabulary's categories, in the order the page offers them.
const CATEGORIES: [&str; 4] = [
    "behavior",
    "authoritySafety",
    "maintainability",
    "architectureFitness",
];

/// One kind of filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Facet {
    Pack,
    Category,
    Stage,
    Direction,
    Execution,
    Enforced,
    Fired,
    Gap,
}

impl Facet {
    pub const ALL: [Facet; 8] = [
        Facet::Pack,
        Facet::Category,
        Facet::Stage,
        Facet::Direction,
        Facet::Execution,
        Facet::Enforced,
        Facet::Fired,
        Facet::Gap,
    ];

    /// Its query parameter.
    pub fn param(self) -> &'static str {
        match self {
            Facet::Pack => "pack",
            Facet::Category => "category",
            Facet::Stage => "stage",
            Facet::Direction => "direction",
            Facet::Execution => "execution",
            Facet::Enforced => "enforced",
            Facet::Fired => "fired",
            Facet::Gap => "gap",
        }
    }

    /// Its fieldset's legend.
    fn legend(self) -> &'static str {
        match self {
            Facet::Pack => "Pack",
            Facet::Category => "Category",
            Facet::Stage => "Lifecycle stage",
            Facet::Direction => "Direction",
            Facet::Execution => "Execution",
            Facet::Enforced => "Enforced at",
            Facet::Fired => "Firings in the log",
            Facet::Gap => "Coverage gap",
        }
    }

    /// Its values, when the vocabulary fixes them.
    pub fn fixed(self) -> Option<&'static [&'static str]> {
        match self {
            Facet::Direction => Some(&["feedforward", "feedback"]),
            Facet::Execution => Some(&["computational", "inferential"]),
            Facet::Fired => Some(&["fired", "never", "refused", "warned"]),
            Facet::Gap => Some(&["unchecked-guide", "unstated-control"]),
            _ => None,
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Facet::Pack => {
                "Only the rules of this pack, by its name; `-` for the repository's own harness. \
                 Repeat to widen."
            }
            Facet::Category => {
                "Only the rules of this harness:category, by its local name (authoritySafety). \
                 Repeat to widen."
            }
            Facet::Stage => "Only the rules at this harness:stage (toolCall). Repeat to widen.",
            Facet::Direction => {
                "Only guides (feedforward) or sensors (feedback), by harness:direction. Repeat to \
                 widen."
            }
            Facet::Execution => {
                "Only computational or inferential rules, by harness:execution. Repeat to widen."
            }
            Facet::Enforced => {
                "Only the rules enforced at this harness:enforcedAt, the literal. Repeat to widen."
            }
            Facet::Fired => {
                "Only the rules that fired, never fired, refused or warned in the firing log. \
                 Repeat to widen."
            }
            Facet::Gap => {
                "Only the guides no computational control checks (unchecked-guide) or the \
                 sensors no guide states (unstated-control), as the coverage questions answer. \
                 Repeat to widen."
            }
        }
    }
}

/// The filter inputs, for a resource's description: on the page they choose its rules, on a
/// detail page they are kept in its link back.
pub fn args(page: bool) -> Vec<ArgSpec> {
    let kept = |summary: &str| {
        if page {
            summary.to_string()
        } else {
            format!("{summary} Kept in the HTML face's links back to the Harness page.")
        }
    };
    let mut out: Vec<ArgSpec> = Facet::ALL
        .iter()
        .map(|f| {
            let spec = web::arg(f.param(), &kept(f.summary()))
                .optional()
                .list(ListSeparator::Newline);
            match f.fixed() {
                Some(values) => spec.one_of(values.iter().copied()),
                None => spec,
            }
        })
        .collect();
    out.push(
        web::arg(
            "q",
            &kept(
                "Only the rules whose label or description holds every one of these words. \
                 Repeat to widen.",
            ),
        )
        .optional()
        .list(ListSeparator::Newline),
    );
    out
}

/// What the request asked to see: each kind's values, in the order given, and the `q=` words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filters {
    chosen: BTreeMap<Facet, Vec<String>>,
    q: Vec<String>,
}

impl Filters {
    /// The filters `inv` carries. A value outside a fixed kind's terms is refused, naming them.
    pub fn from_inv(inv: &Invocation<'_>) -> Result<Filters> {
        let mut out = Filters::default();
        for facet in Facet::ALL {
            let Some(given) = inv.optional_str(facet.param())? else {
                continue;
            };
            let mut values: Vec<String> = Vec::new();
            for value in ListSeparator::Newline.split(given) {
                if let Some(fixed) = facet.fixed() {
                    if !fixed.contains(&value) {
                        return Err(Error::InvalidArgument {
                            name: facet.param().to_string(),
                            detail: format!("`{value}` is not one of {}", fixed.join(", ")),
                        });
                    }
                }
                if !values.iter().any(|v| v == value) {
                    values.push(value.to_string());
                }
            }
            if !values.is_empty() {
                out.chosen.insert(facet, values);
            }
        }
        if let Some(given) = inv.optional_str("q")? {
            for value in ListSeparator::Newline.split(given) {
                let words = value.split_whitespace().collect::<Vec<_>>().join(" ");
                if !words.is_empty() && !out.q.contains(&words) {
                    out.q.push(words);
                }
            }
        }
        Ok(out)
    }

    /// Whether nothing is filtered: the whole harness.
    pub fn is_empty(&self) -> bool {
        self.chosen.is_empty() && self.q.is_empty()
    }

    /// The values chosen for `facet`.
    pub fn values(&self, facet: Facet) -> &[String] {
        self.chosen.get(&facet).map(Vec::as_slice).unwrap_or(&[])
    }

    /// `root=…` and every filter as a query, in a fixed order: what each link of the page and of
    /// the pages it links to carries, so a filtered view is a link that round-trips.
    pub fn query(&self, root: &str) -> String {
        let mut out = format!("root={}", url_segment(root));
        for (facet, values) in &self.chosen {
            for value in values {
                let _ = write!(out, "&{}={}", facet.param(), url_segment(value));
            }
        }
        for q in &self.q {
            let _ = write!(out, "&q={}", url_segment(q));
        }
        out
    }

    /// Whether `rule` passes every kind but `except`.
    fn admits(&self, rule: &Rule, except: Option<Facet>) -> bool {
        let kinds = self.chosen.iter().filter(|(f, _)| Some(**f) != except);
        for (facet, wanted) in kinds {
            let Some(has) = rule.values.get(facet) else {
                return false;
            };
            if !wanted.iter().any(|w| has.contains(w)) {
                return false;
            }
        }
        self.q.is_empty()
            || self.q.iter().any(|q| {
                q.to_lowercase()
                    .split_whitespace()
                    .all(|word| rule.text.contains(word))
            })
    }
}

/// One rule's values, per kind, and its words in lowercase.
struct Rule {
    values: BTreeMap<Facet, BTreeSet<String>>,
    text: String,
}

/// A term's value in a query: its local name in the harness vocabulary, else the whole IRI.
fn term_key(iri: &str) -> &str {
    iri.strip_prefix(H).unwrap_or(iri)
}

/// The first column of `question`'s answer, as a set.
fn answered(harness: &Harness, question: &str) -> Result<BTreeSet<String>> {
    let rows = harness.select(super::question(question)?.text)?;
    Ok(rows
        .rows
        .iter()
        .filter_map(|r| r.first()?.as_ref().map(value))
        .collect())
}

/// One choice of a fieldset: its value, its words, and how many rules it would match.
pub struct Choice {
    pub value: String,
    pub label: String,
    pub count: usize,
    pub checked: bool,
}

/// The rules of one harness, and which of them the filters show.
pub struct Selection {
    pub filters: Filters,
    rules: BTreeMap<String, Rule>,
    shown: BTreeSet<String>,
    /// `(name, words)` of each pack loaded, in config order.
    packs: Vec<(String, String)>,
    /// The guides `guides-without-sensor` answers.
    unchecked: BTreeSet<String>,
}

impl Selection {
    /// Every rule of `harness`, its values, and the ones its filters admit.
    pub fn new(harness: &Harness, all: &BTreeMap<String, Props>) -> Result<Selection> {
        let unchecked = answered(harness, "guides-without-sensor")?;
        let unstated = answered(harness, "sensors-without-guide")?;
        let mut fired: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for f in &harness.firings {
            let entry = fired.entry(f.control.as_str()).or_default();
            if f.class == "Refusal" {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
        let mut rules = BTreeMap::new();
        for (iri, props) in all {
            if !is_a(props, "Guide") && !is_a(props, "Control") {
                continue;
            }
            let mut values: BTreeMap<Facet, BTreeSet<String>> = BTreeMap::new();
            let pack = harness
                .origins
                .get(iri)
                .and_then(|at| harness.packs.get(*at))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| OWN.to_string());
            values.entry(Facet::Pack).or_default().insert(pack);
            for (facet, local) in [
                (Facet::Category, "category"),
                (Facet::Stage, "stage"),
                (Facet::Direction, "direction"),
                (Facet::Execution, "execution"),
            ] {
                for v in vals(props, local) {
                    values
                        .entry(facet)
                        .or_default()
                        .insert(term_key(&v).to_string());
                }
            }
            for v in vals(props, "enforcedAt") {
                values.entry(Facet::Enforced).or_default().insert(v);
            }
            let (refused, warned) = fired.get(iri.as_str()).copied().unwrap_or_default();
            let firing = values.entry(Facet::Fired).or_default();
            firing.insert(
                if refused + warned > 0 {
                    "fired"
                } else {
                    "never"
                }
                .to_string(),
            );
            if refused > 0 {
                firing.insert("refused".to_string());
            }
            if warned > 0 {
                firing.insert("warned".to_string());
            }
            if unchecked.contains(iri) {
                values
                    .entry(Facet::Gap)
                    .or_default()
                    .insert("unchecked-guide".to_string());
            }
            if unstated.contains(iri) {
                values
                    .entry(Facet::Gap)
                    .or_default()
                    .insert("unstated-control".to_string());
            }
            let mut text = label_of(all, iri);
            for comment in props.get(RDFS_COMMENT).into_iter().flatten() {
                text.push('\n');
                text.push_str(&value(comment));
            }
            rules.insert(
                iri.clone(),
                Rule {
                    values,
                    text: text.to_lowercase(),
                },
            );
        }
        let filters = harness.filters.clone();
        let shown = rules
            .iter()
            .filter(|(_, r)| filters.admits(r, None))
            .map(|(iri, _)| iri.clone())
            .collect();
        let packs = harness
            .packs
            .iter()
            .map(|p| {
                let mut words = format!("\u{201c}{}\u{201d}", p.label);
                if let Some(version) = &p.version {
                    let _ = write!(words, " {version}");
                }
                (p.name.clone(), words)
            })
            .collect();
        Ok(Selection {
            filters,
            rules,
            shown,
            packs,
            unchecked,
        })
    }

    /// Whether anything is filtered.
    pub fn filtered(&self) -> bool {
        !self.filters.is_empty()
    }

    /// Whether the filters show `iri`: every rule, when nothing is filtered.
    pub fn shows(&self, iri: &str) -> bool {
        !self.filtered() || self.shown.contains(iri)
    }

    /// Whether `iri` is a guide no computational control checks (`gap=unchecked-guide`).
    pub fn unchecked(&self, iri: &str) -> bool {
        self.unchecked.contains(iri)
    }

    /// How many rules there are, and how many the filters show.
    pub fn counts(&self) -> (usize, usize) {
        (self.shown.len(), self.rules.len())
    }

    /// The words of `value` of `facet`: the vocabulary's label for a term.
    fn label(&self, facet: Facet, value: &str) -> String {
        let iri = if value.contains(':') {
            value.to_string()
        } else {
            format!("{H}{value}")
        };
        match facet {
            Facet::Pack if value == OWN => "this repository's own harness".to_string(),
            Facet::Pack => self
                .packs
                .iter()
                .find(|(name, _)| name == value)
                .map(|(_, words)| words.clone())
                .unwrap_or_else(|| value.to_string()),
            Facet::Direction => match value {
                "feedforward" => "guide".to_string(),
                "feedback" => "sensor".to_string(),
                _ => term_word(&iri),
            },
            Facet::Category | Facet::Stage | Facet::Execution => term_word(&iri),
            Facet::Enforced => value.to_string(),
            Facet::Fired => match value {
                "fired" => "fired",
                "never" => "never fired",
                "refused" => "refused an action",
                _ => "warned",
            }
            .to_string(),
            Facet::Gap => match value {
                "unchecked-guide" => "a guide no computational control checks",
                _ => "a sensor no guide states",
            }
            .to_string(),
        }
    }

    /// The choices of `facet`, each with how many rules it would match given the other kinds'
    /// filters. A choice that would match nothing is offered with 0; a value asked for that the
    /// harness does not hold is offered too, so it can be unchecked.
    pub fn choices(&self, facet: Facet) -> Vec<Choice> {
        let held: BTreeSet<&str> = self
            .rules
            .values()
            .filter_map(|r| r.values.get(&facet))
            .flatten()
            .map(String::as_str)
            .collect();
        let mut offered: Vec<String> = match facet {
            Facet::Pack => {
                let mut out = Vec::new();
                if held.contains(OWN) {
                    out.push(OWN.to_string());
                }
                out.extend(self.packs.iter().map(|(name, _)| name.clone()));
                out
            }
            Facet::Category => CATEGORIES.iter().map(|c| c.to_string()).collect(),
            Facet::Stage => STAGES.iter().map(|s| s.to_string()).collect(),
            _ => facet
                .fixed()
                .map(|f| f.iter().map(|v| v.to_string()).collect())
                .unwrap_or_default(),
        };
        let mut extra: Vec<String> = held
            .iter()
            .filter(|v| !offered.iter().any(|o| o == *v))
            .map(|v| v.to_string())
            .collect();
        if facet == Facet::Stage {
            extra.sort_by_key(|s| (stage_rank(&format!("{H}{s}")), s.clone()));
        }
        offered.extend(extra);
        for asked in self.filters.values(facet) {
            if !offered.contains(asked) {
                offered.push(asked.clone());
            }
        }
        let others: Vec<&Rule> = self
            .rules
            .values()
            .filter(|r| self.filters.admits(r, Some(facet)))
            .collect();
        offered
            .into_iter()
            .map(|value| Choice {
                count: others
                    .iter()
                    .filter(|r| r.values.get(&facet).is_some_and(|h| h.contains(&value)))
                    .count(),
                label: self.label(facet, &value),
                checked: self.filters.values(facet).contains(&value),
                value,
            })
            .collect()
    }

    /// `2 of 5 rules, filtered by: behavior or authority/safety, tool-call.`: what the page shows.
    pub fn summary(&self) -> String {
        let (shown, total) = self.counts();
        let mut by: Vec<String> = self
            .filters
            .chosen
            .iter()
            .map(|(facet, values)| {
                values
                    .iter()
                    .map(|v| self.label(*facet, v))
                    .collect::<Vec<_>>()
                    .join(" or ")
            })
            .collect();
        if !self.filters.q.is_empty() {
            by.push(
                self.filters
                    .q
                    .iter()
                    .map(|q| format!("the words \u{201c}{q}\u{201d}"))
                    .collect::<Vec<_>>()
                    .join(" or "),
            );
        }
        format!(
            "{shown} of {}, filtered by: {}.",
            plural(total, "rule", "rules"),
            by.join(", ")
        )
    }

    /// `q`'s answer narrowed to the rules shown: a row whose first column is a rule the filters
    /// leave out is dropped, and the days are counted again over the shown controls' firings.
    /// Whole when nothing is filtered, and for the one question whose rows are not rules.
    pub fn restrict(&self, harness: &Harness, q: &Question, rows: Rows) -> Rows {
        if !self.filtered() || q.name == UNFILTERED {
            return rows;
        }
        if q.name == "firings-by-day" {
            let mut per: BTreeMap<(String, &str), i64> = BTreeMap::new();
            for f in harness
                .firings
                .iter()
                .filter(|f| self.shown.contains(&f.control))
            {
                let day = f.at.get(..10).unwrap_or(&f.at).to_string();
                *per.entry((day, f.class)).or_default() += 1;
            }
            let mut tallies: Vec<_> = per.into_iter().collect();
            // The question's own order: newest day first, then the outcome.
            tallies.sort_by(|((a, x), _), ((b, y), _)| b.cmp(a).then(x.cmp(y)));
            let tallied = tallies
                .into_iter()
                .map(|((day, outcome), n)| {
                    rows.vars
                        .iter()
                        .map(|var| match var.as_str() {
                            "day" => Some(Literal::new_simple_literal(&day).into()),
                            "outcome" => Some(Literal::new_simple_literal(outcome).into()),
                            "firings" => Some(Literal::from(n).into()),
                            _ => None,
                        })
                        .collect()
                })
                .collect();
            return Rows {
                rows: tallied,
                vars: rows.vars,
            };
        }
        Rows {
            rows: rows
                .rows
                .into_iter()
                .filter(|r| {
                    r.first()
                        .and_then(|t| t.as_ref())
                        .is_some_and(|t| self.shown.contains(&value(t)))
                })
                .collect(),
            vars: rows.vars,
        }
    }

    /// Whether `q`'s answer is left whole by the filters.
    pub fn whole(&self, q: &Question) -> bool {
        self.filtered() && q.name == UNFILTERED
    }

    /// The repository picker, what is shown, and the filter form, as HTML.
    pub fn form_html(&self, harness: &Harness, roots: &[String]) -> String {
        let root = harness.root.as_str();
        let mut out = String::from(
            "<form class=\"search harness-root\" method=\"get\" action=\"/harness\"><div \
             class=\"search-field\"><label for=\"harness-root\">Repository</label><select \
             id=\"harness-root\" name=\"root\" aria-describedby=\"harness-root-help\">",
        );
        for r in roots {
            let _ = write!(
                out,
                "<option value=\"{}\"{}>{}</option>",
                attr(r),
                if r == root { " selected" } else { "" },
                html_text(r)
            );
        }
        out.push_str(
            "</select></div><button type=\"submit\" class=\"quiet\">Show its harness</button><p \
             id=\"harness-root-help\" class=\"note\">Each repository you may read that has a \
             harness: its own, or a pack that applies to it. Another repository is shown \
             whole, with no filter.</p></form>",
        );
        let mut shown = Vec::new();
        if harness.own {
            shown.push(format!("its own {GRAPH_FILE}"));
        }
        let packs: Vec<String> = self.packs.iter().map(|(_, w)| w.clone()).collect();
        match packs.len() {
            0 => {}
            1 => shown.push(format!("the pack {}", packs[0])),
            _ => shown.push(format!("the packs {}", packs.join(", "))),
        }
        let _ = write!(
            out,
            "<p class=\"harness-shown\">Showing the harness of \u{201c}{}\u{201d}: {}.</p>",
            html_text(root),
            html_text(&shown.join(", and "))
        );

        out.push_str(
            "<h2 id=\"harness-filters\">Choose the rules</h2><form class=\"harness-filter\" \
             method=\"get\" action=\"/harness\" aria-labelledby=\"harness-filters\">",
        );
        let _ = write!(
            out,
            "<input type=\"hidden\" name=\"root\" value=\"{}\">",
            attr(root)
        );
        let words = if self.filters.q.is_empty() {
            vec![String::new()]
        } else {
            self.filters.q.clone()
        };
        out.push_str("<div class=\"search harness-words\">");
        for (at, q) in words.iter().enumerate() {
            let (id, label) = if at == 0 {
                ("harness-q".to_string(), "Words in the label or description")
            } else {
                (format!("harness-q-{at}"), "Or these words")
            };
            let _ = write!(
                out,
                "<div class=\"search-field\"><label for=\"{id}\">{label}</label><input \
                 id=\"{id}\" type=\"search\" name=\"q\" value=\"{}\"></div>",
                attr(q)
            );
        }
        out.push_str("</div><div class=\"harness-facets\">");
        for facet in Facet::ALL {
            if facet == Facet::Pack && self.packs.is_empty() {
                continue;
            }
            let choices = self.choices(facet);
            if choices.is_empty() {
                continue;
            }
            let _ = write!(
                out,
                "<fieldset class=\"harness-facet\"><legend>{}</legend><ul class=\"facet-terms\">",
                html_text(facet.legend())
            );
            for c in choices {
                let _ = write!(
                    out,
                    "<li><label><input type=\"checkbox\" name=\"{}\" value=\"{}\"{}> <span \
                     class=\"facet-term\">{}</span> <span class=\"facet-count\">({})</span></label></li>",
                    facet.param(),
                    attr(&c.value),
                    if c.checked { " checked" } else { "" },
                    html_text(&c.label),
                    html_text(&plural(c.count, "rule", "rules")),
                );
            }
            out.push_str("</ul></fieldset>");
        }
        out.push_str(
            "</div><p id=\"harness-filter-help\" class=\"note\">Choices of one kind widen the \
             rules shown (any may match); choices of different kinds narrow them (all must \
             match). Each count is how many rules that choice would show with the other kinds' \
             choices kept.</p><button type=\"submit\" aria-describedby=\"harness-filter-help\">\
             Show these rules</button></form>",
        );
        // The sentence itself is in #status, the page's one live region (src/harness.rs).
        if self.filtered() {
            let _ = write!(
                out,
                "<p class=\"harness-clear\">{} to show all {}.</p>",
                link(
                    &page_href(PAGE_IRI, &Filters::default().query(root)),
                    "Clear every filter"
                ),
                html_text(&plural(self.rules.len(), "rule", "rules"))
            );
        }
        out
    }
}

/// Every root this caller may read that has a harness: a graph of its own, or a pack that
/// applies to it. `current` is listed without asking again.
pub async fn roots(
    web: &Web,
    packs: &crate::packs::Config,
    inv: &Invocation<'_>,
    current: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for root in crate::k::readable_roots(web, inv.capability) {
        let has = root == current
            || read_file(inv, &root, GRAPH_FILE)
                .await
                .is_ok_and(|own| own.is_some())
            || (!packs.is_empty()
                && crate::packs::assemble(web, packs, inv, &root, None)
                    .await
                    .is_ok_and(|a| !a.packs.is_empty()));
        if has {
            out.push(root);
        }
    }
    out
}

/// `description` with the filter inputs ([`args`]).
pub fn described(description: Description, page: bool) -> Description {
    args(page).into_iter().fold(description, Description::input)
}
