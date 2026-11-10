//! A deployment's own stylesheet and script on gonk's pages (ledger
//! [#3379](http://localhost:1062/l/default/item/3379)), with a line between gonk and the
//! deployment's code that a person can see and act on.
//!
//! ```text
//! gonk.page.style  = "/face/name/ops/site.css"
//! gonk.page.script = "/face/name/ops/site.js"
//! ```
//!
//! Each key repeats, and each value is a path on this server (`/…`, never `//…` or a scheme), so
//! the door's content security policy (`style-src 'self'`, `script-src 'self'`) is unchanged.
//! Every page drawn through gonk's shared head (`web/gonk.xsl`, the `view:page` shell: item
//! pages, lists, refusals, sign-in) links them, the stylesheet LAST in `<head>` so it wins a tie,
//! the script deferred AFTER `gonk.js` so gonk's own behavior is wired before it runs. A face's
//! page gets them inside gonk's frame ([`crate::face_frame`]). `/toolbox` keeps its own page and
//! policy and is not covered.
//!
//! # The line between gonk and the deployment
//!
//! - **Safe mode**: `?safe=1` on any covered page leaves the deployment's assets out
//!   ([`safe_of`]), and `/static/gonk-safe.js` (a per-browser toggle, `localStorage`) does the
//!   same on every page in one browser. Every covered page's footer says it exists, refusals
//!   included, so "is it gonk or my script?" is one click.
//! - **Attribution**: a comment in `<head>` names the config key before the deployment's assets,
//!   and the footer says in words that this deployment adds its own style or script.
//! - **Isolation**: `gonk.js` is deferred ahead of the deployment's scripts, so its listeners are
//!   in place before a deployment script runs, and a deployment script that throws stops only
//!   itself.
//! - **A short list of stable hooks**: [`STABLE_HOOKS`], documented in the README; everything
//!   else may change at any pin move.

use ikigai_core::Invocation;

use crate::render::{element, wrap};

/// The config key for a deployment stylesheet: one same-origin path per line.
pub const STYLE_KEY: &str = "gonk.page.style";

/// The config key for a deployment script: one same-origin path per line.
pub const SCRIPT_KEY: &str = "gonk.page.script";

/// The query argument that loads a page without the deployment's assets.
pub const SAFE_ARG: &str = "safe";

/// The per-browser safe-mode gate under `/static/`: a blocking script ahead of the deployment's
/// assets, linked only when there are some.
pub const GATE: &str = "gonk-safe.js";

/// The ids and classes gonk keeps stable across releases for a deployment's stylesheet and
/// script, beside `ikigai-ui`'s tokens. Everything else may change at any pin move.
/// `tests/page_assets_3379.rs` pins that each still appears where the README says.
pub const STABLE_HOOKS: [&str; 10] = [
    "#main",
    "header.top",
    "#status",
    "#item",
    ".item-title",
    ".comment-list",
    ".comment",
    ".comment-meta",
    "#comment-order",
    "#deployment-note",
];

/// A deployment's own stylesheets and scripts, in config order. Empty for none: no gate, no
/// footer, and every page is as it was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageAssets {
    styles: Vec<String>,
    scripts: Vec<String>,
}

impl PageAssets {
    /// Every `gonk.page.style` and `gonk.page.script` line, each checked by [`check_path`].
    pub fn parse(styles: &[String], scripts: &[String]) -> Result<PageAssets, String> {
        let checked = |key: &str, lines: &[String]| {
            lines
                .iter()
                .map(|line| {
                    check_path(line.trim()).map_err(|why| format!("{key} = {line:?}: {why}"))
                })
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(PageAssets {
            styles: checked(STYLE_KEY, styles)?,
            scripts: checked(SCRIPT_KEY, scripts)?,
        })
    }

    /// Whether there is nothing of the deployment's to add.
    pub fn is_empty(&self) -> bool {
        self.styles.is_empty() && self.scripts.is_empty()
    }

    /// The stylesheets' paths, in config order.
    pub fn styles(&self) -> &[String] {
        &self.styles
    }

    /// The scripts' paths, in config order.
    pub fn scripts(&self) -> &[String] {
        &self.scripts
    }

    /// What the footer says the deployment adds, in words.
    pub fn adds(&self) -> &'static str {
        match (self.styles.is_empty(), self.scripts.is_empty()) {
            (false, false) => "its own style and script",
            (false, true) => "its own style",
            (true, false) => "its own script",
            (true, true) => "nothing",
        }
    }

    /// The `view:page-assets` element the shell draws the head's links and the footer from, or
    /// nothing when there are no assets.
    pub fn view(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut children: String = self
            .styles
            .iter()
            .map(|href| element("page-style", &[("href", href.as_str())], ""))
            .collect();
        children.extend(
            self.scripts
                .iter()
                .map(|src| element("page-script", &[("src", src.as_str())], "")),
        );
        wrap("page-assets", &[("adds", self.adds())], &children)
    }
}

/// A same-origin path a page may link: `/` then anything but a second `/`, with no space, quote,
/// angle bracket, backslash, control character or `--` (the head's comment cannot hold one, and
/// no path needs one). The door's CSP would refuse anything else; this refuses it at start, named.
pub fn check_path(path: &str) -> Result<String, String> {
    if !path.starts_with('/') || path.starts_with("//") {
        return Err("must be a path on this server, starting with one `/`".to_string());
    }
    if let Some(bad) = path.chars().find(|c| {
        c.is_control() || c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`')
    }) {
        return Err(format!("may not hold {bad:?}"));
    }
    if path.contains("--") {
        return Err("may not hold `--`".to_string());
    }
    Ok(path.to_string())
}

/// Whether `?safe=1` asks for this page without the deployment's assets.
pub fn safe_of(inv: &Invocation<'_>) -> bool {
    inv.inline_str(SAFE_ARG).is_ok_and(|asked| asked == "1")
}

/// [`safe_of`] for an answer with no invocation (a refusal page): the raw request target's query.
pub fn safe_in_target(raw_target: &str) -> bool {
    raw_target
        .split_once('?')
        .is_some_and(|(_, query)| query.split('&').any(|pair| pair == "safe=1"))
}

/// The envelope attribute's value for [`safe_of`].
pub fn flag(safe: bool) -> &'static str {
    if safe {
        "true"
    } else {
        "false"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_on_this_server_is_the_only_kind_accepted() {
        for good in ["/face/name/ops/site.css", "/face/name/ops/js/site.js?v=2"] {
            assert_eq!(check_path(good).as_deref(), Ok(good));
        }
        for bad in [
            "site.css",
            "//cdn.example/site.css",
            "https://cdn.example/site.css",
            "/a b.css",
            "/a\".css",
            "/a<b.css",
            "/a\\b.css",
            "/a--b.css",
            "",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_footer_names_what_the_deployment_adds() {
        let style = |s: &[&str], j: &[&str]| {
            let lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            PageAssets::parse(&lines(s), &lines(j)).unwrap()
        };
        assert_eq!(
            style(&["/a.css"], &["/a.js"]).adds(),
            "its own style and script"
        );
        assert_eq!(style(&["/a.css"], &[]).adds(), "its own style");
        assert_eq!(style(&[], &["/a.js"]).adds(), "its own script");
        assert!(style(&[], &[]).view().is_empty());
        let view = style(&["/a.css"], &["/a.js"]).view();
        assert!(
            view.contains("href=\"/a.css\"") && view.contains("src=\"/a.js\""),
            "{view}"
        );
    }

    #[test]
    fn a_refusal_reads_safe_mode_from_its_query() {
        assert!(safe_in_target("/l/default?safe=1"));
        assert!(safe_in_target("/x?a=b&safe=1"));
        assert!(!safe_in_target("/x?safe=0"));
        assert!(!safe_in_target("/x?unsafe=1"));
        assert!(!safe_in_target("/x"));
    }
}
