//! Where gonk keeps its secrets (ledger #4506, epic ledger #4485): the `[secret]` section of the
//! config home's `config.toml`, read once at start into ikigai-secret's routing backend.
//!
//! Gonk keeps two kinds of secret: the onboarding controller's private key
//! ([`crate::onboard::DEFAULT_KEY`], `ikigai-gonk.onboard.controller`) and each open invite's
//! pairing code ([`crate::onboard::INVITE_SECRET_PREFIX`]). Both go through ONE
//! [`Keystore`], built here once and handed to every place gonk reads or writes a secret, so a
//! `[[secret.route]]` for `ikigai-gonk.*` moves all of them to the store it names.
//!
//! ```toml
//! [secret]
//! default = "keychain"
//!
//! [secret.store.keychain]
//! kind = "keychain"
//!
//! [secret.store.gonk]
//! kind = "file"
//! path = "~/.ikigai/gonk-secrets"
//!
//! [[secret.route]]
//! match = "ikigai-gonk.*"
//! store = "gonk"
//! ```
//!
//! With no section, the keystore is exactly what `ikigai_secret::default_backend()` built before
//! (the Keychain under `ikigai-secret` on a Mac, the file store in the data home elsewhere), and
//! its space is the plain `urn:secret:*` space. A section that ikigai-secret refuses stops the
//! server (and `onboard init`) with ikigai-secret's own sentence; nothing in it is half-applied.
//!
//! The rest of `config.toml` is gonk's `key = value` line grammar, which a whole-document TOML
//! parse refuses (repeated keys, bare values), so only the `[secret]` tables are parsed as TOML:
//! every other line is blanked, which keeps the line numbers a TOML error names the file's own.
//!
//! The section is read at start only: gonk has no reload of it, so nothing here cuts
//! [`ikigai_secret::CONFIG_THREAD`]; a changed section takes effect at the next start.

use std::path::Path;
use std::sync::Arc;

use ikigai_core::EndpointSpace;
use ikigai_secret::{Backend, PresenceGate, RoutingBackend, SecretConfig, SecretSection};

/// The secrets every part of gonk reads and writes: one routing backend, built once at start.
#[derive(Clone)]
pub struct Keystore {
    routing: Arc<RoutingBackend>,
    configured: bool,
}

impl Keystore {
    /// The keystore `config.toml`'s text names, `home` being the home directory a store path
    /// beginning with `~/` is read under.
    ///
    /// # Errors
    ///
    /// A `[secret]` section that does not read as TOML, or that ikigai-secret refuses: the
    /// sentence ikigai-secret wrote (every problem, each naming its line), unchanged.
    pub fn from_config_text(text: &str, home: Option<&Path>) -> Result<Keystore, String> {
        let section = section(text)?;
        let config =
            SecretConfig::from_section(section.as_ref(), home).map_err(|e| e.to_string())?;
        Ok(Keystore::from_config(config))
    }

    /// The keystore over a checked configuration.
    pub fn from_config(config: SecretConfig) -> Keystore {
        let configured = config.is_configured();
        Keystore {
            routing: Arc::new(config.build()),
            configured,
        }
    }

    /// The backend, for the places that read and write a secret directly (the signer, the
    /// invites' codes). The same routing backend every time.
    pub fn backend(&self) -> Arc<dyn Backend> {
        Arc::clone(&self.routing) as Arc<dyn Backend>
    }

    /// The routing backend itself, for its configuration.
    pub fn routing(&self) -> &Arc<RoutingBackend> {
        &self.routing
    }

    /// Whether a `[secret]` section configured this keystore (`false`: the platform store, as
    /// before).
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// The `urn:secret:*` space over this keystore, proving presence through `gate` (the
    /// platform's Touch ID when `None`).
    ///
    /// With no section it is the plain space it always was, so no secret name is newly
    /// reserved. With a section it is the routed space, whose inspection resources
    /// (`urn:secret:store`, `urn:secret:route` and the names under them) are bound ahead of
    /// `urn:secret:{name}`: [`Self::describe`] says so at start.
    pub fn space(&self, gate: Option<Arc<dyn PresenceGate>>) -> EndpointSpace {
        match (self.configured, gate) {
            (false, Some(gate)) => ikigai_secret::space_with_gate(self.backend(), gate),
            (false, None) => ikigai_secret::space(self.backend()),
            (true, Some(gate)) => {
                ikigai_secret::routed_space_with_gate(Arc::clone(&self.routing), gate)
            }
            (true, None) => ikigai_secret::routed_space(Arc::clone(&self.routing)),
        }
    }

    /// The one start-up sentence: where gonk's secrets are kept.
    pub fn describe(&self) -> String {
        let config = self.routing.config();
        if !self.configured {
            let store = config.default_store();
            return format!(
                "the {store} store, as before (no [secret] section in config.toml); gonk's keys \
                 and invite codes are kept there"
            );
        }
        let stores: Vec<&str> = config.stores().iter().map(|s| s.name()).collect();
        let gonk = config.route_for(crate::onboard::DEFAULT_KEY).store;
        let invites = config
            .route_for(&format!("{}x", crate::onboard::INVITE_SECRET_PREFIX))
            .store;
        let routes = config.routes().len();
        format!(
            "the [secret] section in config.toml: {count} {stores_word} ({names}), default \
             `{default}`, {routes} {routes_word}; the controller key is kept in `{gonk}` and \
             invite codes in `{invites}`. The names urn:secret:store and urn:secret:route are \
             reserved for inspecting the stores and routes, so no secret is named `store` or \
             `route`.",
            count = stores.len(),
            stores_word = if stores.len() == 1 { "store" } else { "stores" },
            names = stores.join(", "),
            default = config.default_store(),
            routes_word = if routes == 1 { "route" } else { "routes" },
        )
    }
}

/// The `[secret]` section of `config.toml`'s text, or `None` when it has none.
///
/// # Errors
///
/// The section's TOML does not read, or names a key ikigai-secret does not know.
pub fn section(text: &str) -> Result<Option<SecretSection>, String> {
    #[derive(serde::Deserialize)]
    struct Doc {
        secret: Option<SecretSection>,
    }
    let Some(only) = secret_lines(text) else {
        return Ok(None);
    };
    toml::from_str::<Doc>(&only)
        .map(|doc| doc.secret)
        .map_err(|e| {
            format!(
                "The [secret] section was refused and nothing in it was applied: {}",
                e.to_string().trim_end()
            )
        })
}

/// `text` with every line that is not part of a `[secret]` table blanked, or `None` when no
/// line is. A table is `[secret]`, `[secret.…]` or `[[secret.…]]`, and runs to the next header
/// that is not; a top-level `secret.… = …` line before any header is part of it too.
fn secret_lines(text: &str) -> Option<String> {
    let mut inside = false;
    let mut seen_header = false;
    let mut any = false;
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        let keep = if trimmed.starts_with('[') {
            seen_header = true;
            inside = header_is_secret(trimmed);
            inside
        } else if seen_header {
            inside
        } else {
            let key = trimmed.split_once('=').map(|(k, _)| k.trim()).unwrap_or("");
            key == "secret" || key.starts_with("secret.")
        };
        if keep {
            any = true;
            out.push_str(line);
        }
        out.push('\n');
    }
    any.then_some(out)
}

/// Whether a table header (`[a.b]` or `[[a.b]]`) names a table under `secret`.
fn header_is_secret(header: &str) -> bool {
    let name = header.trim_start_matches('[').trim_start();
    let first = name
        .split(|c: char| c == '.' || c == ']' || c.is_whitespace())
        .next()
        .unwrap_or("");
    matches!(first, "secret" | "\"secret\"" | "'secret'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_with_no_section_has_none() {
        let text = "gonk.bind = \"127.0.0.1:1062\"\ngonk.browse.root = core=~/x\n";
        assert_eq!(section(text).unwrap(), None);
        assert_eq!(section("").unwrap(), None);
    }

    #[test]
    fn only_the_secret_tables_are_parsed_and_lines_keep_their_numbers() {
        let text = "gonk.browse.root = core=~/x\ngonk.browse.root = core=~/y\n[secret]\n\
                    default = \"a\"\n[secret.store.a]\nkind = \"file\"\npath = \"/s\"\n\
                    [other]\nx = = 1\n[[secret.route]]\nmatch = \"ikigai-gonk.*\"\nstore = \"a\"\n";
        let only = secret_lines(text).unwrap();
        assert_eq!(only.lines().count(), text.lines().count());
        assert!(!only.contains("gonk.browse.root"));
        assert!(!only.contains("x = = 1"));
        let section = section(text).unwrap().unwrap();
        assert_eq!(section.default.as_deref(), Some("a"));
        assert_eq!(section.routes.len(), 1);
    }

    #[test]
    fn an_unknown_key_is_refused_naming_its_line() {
        let text = "gonk.bind = \"127.0.0.1:1062\"\n\n[secret]\ndefault = \"a\"\nbogus = 1\n";
        let e = section(text).unwrap_err();
        assert!(e.contains("line 5"), "{e}");
        assert!(e.contains("bogus"), "{e}");
    }
}
