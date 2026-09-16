//! The capability tokens a non-root caller needs for one ledger — computed from
//! `ikigai-ledger`'s and `ikigai-store`'s own spellings, never transcribed.
//!
//! A ledger caller needs **both halves**: the ledger's grant, and the store's per-graph
//! token for that ledger's named graph, because a sub-request carries the caller's
//! capability unchanged. [`grants_for`] is the whole list.
//!
//! ⚠ **The broad store tokens are refused everywhere in this server.** `urn:cap:store:read`
//! is every graph in the dataset and `urn:cap:store:write` is `DROP ALL`; a grant naming
//! either would make the per-ledger boundary decorative. They are also useless here —
//! `urn:iki:store:graph-update` refuses the broad key by design — so a grant built around
//! them yields a ledger that cannot write and a `Denied` that looks like a ledger bug.
//! [`broad_store_scopes`] is the check `quic.rs` applies to every grant.

use ikigai_ledger::Ledger;

/// How much authority over one ledger. Cumulative: each level includes the ones before it,
/// because there is no useful "may delete but may not read".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// `items`, `item:{id}`, `next`, `ledgers`.
    Read,
    /// …plus `append`, `comment`, `close`, `reopen`, `claim`, `defer`, `link`, `label`, and
    /// editing an item.
    Write,
    /// …plus `Delete` on an item, which moves it into the ledger's graveyard graph.
    Delete,
    /// …plus `purge`, which destroys the content in both graphs.
    Purge,
}

impl std::str::FromStr for Authority {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        match value {
            "read" => Ok(Authority::Read),
            "write" => Ok(Authority::Write),
            "delete" => Ok(Authority::Delete),
            "purge" => Ok(Authority::Purge),
            other => Err(format!(
                "`{other}` is not an authority (read | write | delete | purge)"
            )),
        }
    }
}

/// Every token a non-root caller needs to use the ledger `ledger` at `authority`.
///
/// ```
/// use ikigai_gonk::grants::{grants_for, Authority};
/// assert_eq!(
///     grants_for("acme", Authority::Write).unwrap(),
///     [
///         "urn:cap:ledger:read:acme",
///         "urn:cap:store:read:graph:urn:iki:ledger:graph:acme",
///         "urn:cap:ledger:write:acme",
///         "urn:cap:store:write:graph:urn:iki:ledger:graph:acme",
///     ]
/// );
/// ```
///
/// # Errors
///
/// When `ledger` cannot be a ledger name — wrong characters, too long, or one of the words
/// the bare `urn:iki:ledger:*` form reserves. A token is matched exactly, so a typo refused
/// here would otherwise be a silent denial at first use.
pub fn grants_for(ledger: &str, authority: Authority) -> Result<Vec<String>, String> {
    let ledger = Ledger::parse(ledger).map_err(|e| e.to_string())?;
    let graph = ledger.graph();
    let mut grants = vec![ledger.cap_read(), ikigai_store::cap_read_graph(&graph)];
    if authority == Authority::Read {
        return Ok(grants);
    }
    grants.push(ledger.cap_write());
    grants.push(ikigai_store::cap_write_graph(&graph));
    if authority == Authority::Write {
        return Ok(grants);
    }
    // The graveyard is a SECOND graph, and a scoped write cannot reach across.
    grants.push(ledger.cap_delete());
    grants.push(ikigai_store::cap_write_graph(&ledger.deleted_graph()));
    if authority == Authority::Delete {
        return Ok(grants);
    }
    grants.push(ledger.cap_purge());
    Ok(grants)
}

/// The union of [`grants_for`] over several ledgers at one authority, first-seen order.
pub fn grants_for_all(ledgers: &[String], authority: Authority) -> Result<Vec<String>, String> {
    let mut union: Vec<String> = Vec::new();
    for ledger in ledgers {
        for token in grants_for(ledger, authority)? {
            if !union.contains(&token) {
                union.push(token);
            }
        }
    }
    Ok(union)
}

/// The scopes in `scopes` that are one of the store's two broad tokens.
pub fn broad_store_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|scope| *scope == ikigai_store::CAP_READ || *scope == ikigai_store::CAP_WRITE)
        .cloned()
        .collect()
}

/// `urn:cap:exec:*` — the OFFERING wildcard `ikigai-repo` declares on `urn:system:exec`,
/// which as a GRANT means "run any program".
pub const CAP_EXEC_ANY: &str = "urn:cap:exec:*";

/// The scopes in `scopes` that grant unbounded process execution.
///
/// ★ **A wildcard is an offering form, not a grant form, and the two look identical in a
/// JSON file.** `ikigai-repo` declares `urn:cap:exec:*` on `urn:system:exec` to say "holds
/// some grant under this prefix"; the same string written into `grants.json` says "may run
/// anything on this machine", over a network door. The per-tool spelling
/// (`urn:cap:exec:git`) is what that crate enforces at dispatch and what an operator means,
/// so the wildcard is refused the way the broad store tokens are — a grant a reader could
/// mistake for a narrowing must not be silently the opposite.
pub fn unbounded_exec_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|scope| *scope == CAP_EXEC_ANY)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ Literals, the one place that does not compute them: these strings go into an
    /// operator's `grants.json`, so a change to how either crate spells a graph or a token
    /// has to fail HERE, where the operator's file would have to change too.
    #[test]
    fn a_delete_grant_carries_the_graveyard_and_nothing_broad() {
        let grants = grants_for("acme", Authority::Delete).unwrap();
        assert_eq!(
            grants,
            [
                "urn:cap:ledger:read:acme",
                "urn:cap:store:read:graph:urn:iki:ledger:graph:acme",
                "urn:cap:ledger:write:acme",
                "urn:cap:store:write:graph:urn:iki:ledger:graph:acme",
                "urn:cap:ledger:delete:acme",
                "urn:cap:store:write:graph:urn:iki:ledger:graph:acme:deleted",
            ]
        );
        assert!(broad_store_scopes(&grants).is_empty());
        let purge = grants_for("acme", Authority::Purge).unwrap();
        assert_eq!(purge.last().unwrap(), "urn:cap:ledger:purge:acme");
    }

    #[test]
    fn a_name_that_cannot_be_a_ledger_is_refused() {
        assert!(grants_for("items", Authority::Read).is_err(), "reserved");
        assert!(
            grants_for("a:b", Authority::Read).is_err(),
            "would forge a token"
        );
    }

    #[test]
    fn the_broad_store_tokens_are_named_when_present() {
        let scopes = vec![
            "urn:cap:ledger:read:default".to_string(),
            "urn:cap:store:read".to_string(),
            "urn:cap:store:write".to_string(),
        ];
        assert_eq!(
            broad_store_scopes(&scopes),
            ["urn:cap:store:read", "urn:cap:store:write"]
        );
    }

    #[test]
    fn the_union_over_ledgers_keeps_each_token_once() {
        let ledgers = vec![
            "default".to_string(),
            "acme".to_string(),
            "default".to_string(),
        ];
        let union = grants_for_all(&ledgers, Authority::Read).unwrap();
        assert_eq!(union.len(), 4, "{union:?}");
    }
}
