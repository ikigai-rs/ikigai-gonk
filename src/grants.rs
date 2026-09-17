//! The capability tokens a non-root caller needs for one ledger — computed from
//! `ikigai-ledger`'s and `ikigai-store`'s own spellings, never transcribed.
//!
//! A ledger caller needs **both halves**: the ledger's grant, and the store's per-graph
//! token for that ledger's named graph, because a sub-request carries the caller's
//! capability unchanged. [`grants_for`] is the whole list.
//!
//! ★ Since 2026-09-16 there is a second subject: **the browse graph**
//! ([`browse_graph_grants`]). It is the same shape — a per-graph store token — and it exists
//! at all only because [`crate::browse::Graph::chosen`] named a graph; the default graph has
//! no IRI, so browse's quads used to sit outside this file's reach entirely.
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

/// ★ **Obligation 2 of the graph decision: the browse graph's tokens, computed from the
/// choice.**
///
/// Before 2026-09-16 browse's quads were in the DEFAULT graph, which has no IRI — so there
/// was no token to compute, no grant an operator could write, and every query over an
/// annotation or an archived explanation was a ROOT one. [`crate::browse::Graph::chosen`]
/// named a graph, and these are the two tokens that names buys:
///
/// ```
/// use ikigai_gonk::grants::{browse_graph_grants, Authority};
/// assert_eq!(
///     browse_graph_grants(Authority::Write).unwrap(),
///     [
///         "urn:cap:store:read:graph:urn:iki:browse:graph:default",
///         "urn:cap:store:write:graph:urn:iki:browse:graph:default",
///     ]
/// );
/// ```
///
/// # What they are, and what they are not
///
/// They are authority over **quads in one graph**, through
/// `urn:iki:store:graph-{select,ask,construct,describe}` and `urn:iki:store:graph-update`.
/// Read gets every annotation, every archived explanation and every review finding as data —
/// *the archive without the spend*, which no grant could express while the data sat in the
/// default graph. Write lets an identity mint or edit those quads directly.
///
/// They are **not** the browse family: `urn:cap:browse:read:*` (file contents, trees, the
/// PR rows), `urn:cap:annotate` (the annotation endpoints) and `urn:cap:net:*` (deriving)
/// are separate, and this function mints none of them. An identity holding only these two
/// can read browse's graph and cannot read a file.
///
/// ⚠ **Delete and Purge are the ledger's vocabulary, not this one.** A ledger's Delete needs
/// a second graph (its graveyard) and Purge is a ledger verb; browse has neither — deleting
/// an annotation is an ordinary write in this one graph. So anything above [`Authority::Read`]
/// is exactly read+write here, and it says so rather than refusing, because an operator
/// spelling `--browse-graph delete` means "and may remove things".
///
/// # Errors
///
/// When this server's choice is the DEFAULT graph ([`crate::browse::Graph::TheDefault`]),
/// which has no IRI for a token to name. The error is the explanation, because the operator
/// asking has every reason to think a grant should exist.
#[must_use = "these are capability tokens; minting them and dropping them grants nothing"]
pub fn browse_graph_grants(authority: Authority) -> Result<Vec<String>, String> {
    let chosen = crate::browse::Graph::chosen();
    let graph = chosen.named().ok_or_else(|| {
        "this server writes browse's quads in the DEFAULT graph, which has no IRI — no \
         `urn:cap:store:*:graph:` token can name it, so there is no grant to mint. See \
         `browse::Graph`."
            .to_string()
    })?;
    let mut grants = vec![ikigai_store::cap_read_graph(graph.as_str())];
    if authority != Authority::Read {
        grants.push(ikigai_store::cap_write_graph(graph.as_str()));
    }
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

/// The backup family's two tokens — the only authority in this server that is not scoped to
/// a ledger, and the reason it is refused as a grant rather than merely never minted.
///
/// ★ **These are the airlock.** `urn:cap:gonk:backup` reads EVERY graph (a backup is the
/// whole dataset in one file, which is precisely the authority the per-graph boundary exists
/// to avoid handing out) and `urn:cap:gonk:restore` builds a store from bytes a caller
/// supplies. Neither can be attached to a certificate or a passkey, so neither is reachable
/// from the QUIC door or the HTTP door at all: the privileged half of the feature lives
/// behind the owner-only socket, whose caller can read the dataset's files anyway. That is
/// the contact-intake airlock's shape — its own door, not a flag on the public one.
///
/// ⚠ Refused as a GRANT, not as a requirement. The endpoints declare them and the root
/// capability on the socket satisfies them, exactly as it satisfies everything else.
pub const CAP_GONK_ADMIN: [&str; 2] = [crate::backup::CAP_BACKUP, crate::backup::CAP_RESTORE];

/// The scopes in `scopes` that are one of the backup family's tokens.
pub fn gonk_admin_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|scope| CAP_GONK_ADMIN.contains(&scope.as_str()))
        .cloned()
        .collect()
}

/// `urn:cap:net:*` — the OFFERING wildcard `ikigai-browse` declares on every derivation
/// (`explain`, `review`, the PR layers), which as a GRANT means "reach any host".
pub const CAP_NET_ANY: &str = "urn:cap:net:*";

/// The scopes in `scopes` that grant unbounded network reach.
///
/// ★ **The same footgun as [`unbounded_exec_scopes`], on the newest surface.** Deriving an
/// explanation is a network act, so `ikigai-browse` declares `urn:cap:net:*` — "holds some
/// grant under this prefix". Written into `grants.json` it says the opposite: this identity
/// may reach ANY host that anything in reach of this kernel can dial. Today that is the
/// mounted peer and nothing else, because `urn:llm:` is the only prefix a mount may claim
/// and no HTTP client is linked — but a grant is durable and a manifest is not, and the
/// narrow spelling costs an operator one word. Name the host:
/// `urn:cap:net:localhost` for a peer on this machine.
///
/// ⚠ It is refused as a grant, NOT as a requirement: the narrow grant still satisfies
/// browse's wildcard declaration, which is what the offering form means.
pub fn unbounded_net_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|scope| *scope == CAP_NET_ANY)
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

    /// ★ The browse graph's tokens, as literals for the same reason the ledger's are: they
    /// go into an operator's `grants.json`, and they are the FIRST spelling of "may read
    /// browse's data" this server has ever been able to offer.
    ///
    /// ⚠ The token embeds the graph IRI, so changing `browse::Graph::chosen` invalidates
    /// every grant already written — which is the other half of why that IRI is pinned by a
    /// test in `browse.rs`. A store migration and a grant rewrite, not one of them.
    #[test]
    fn the_browse_graphs_tokens_are_the_choices_two_store_doors() {
        assert_eq!(
            browse_graph_grants(Authority::Read).unwrap(),
            ["urn:cap:store:read:graph:urn:iki:browse:graph:default"]
        );
        let write = browse_graph_grants(Authority::Write).unwrap();
        assert_eq!(
            write,
            [
                "urn:cap:store:read:graph:urn:iki:browse:graph:default",
                "urn:cap:store:write:graph:urn:iki:browse:graph:default",
            ]
        );
        // Nothing broad, and nothing that widens what the identity may RUN: these are data
        // tokens, and a reader of grants.json should be able to see that at a glance.
        assert!(broad_store_scopes(&write).is_empty());
        assert!(unbounded_net_scopes(&write).is_empty());
        assert!(unbounded_exec_scopes(&write).is_empty());
        assert!(gonk_admin_scopes(&write).is_empty());
        // Delete and Purge are the ledger's words; here they are write.
        assert_eq!(browse_graph_grants(Authority::Delete).unwrap(), write);
        assert_eq!(browse_graph_grants(Authority::Purge).unwrap(), write);
    }

    /// ★ The token names the graph the server actually writes — derived, not transcribed.
    /// A test that spelled the IRI itself would pass on a server that had moved.
    #[test]
    fn the_browse_token_names_the_graph_this_server_chose() {
        let chosen = crate::browse::Graph::chosen();
        let graph = chosen.named().expect("this server names a browse graph");
        assert_eq!(
            browse_graph_grants(Authority::Read).unwrap(),
            [ikigai_store::cap_read_graph(graph.as_str())]
        );
    }

    #[test]
    fn a_name_that_cannot_be_a_ledger_is_refused() {
        assert!(grants_for("items", Authority::Read).is_err(), "reserved");
        assert!(
            grants_for("a:b", Authority::Read).is_err(),
            "would forge a token"
        );
    }

    /// ★ The wildcard pair, both directions: refused as a GRANT, and the narrow spelling
    /// left alone — a rule that refused `urn:cap:net:localhost` would make deriving
    /// ungrantable rather than bounded.
    #[test]
    fn the_offering_wildcards_are_refused_as_grants_and_the_narrow_ones_are_not() {
        let wild = vec![
            "urn:cap:net:*".to_string(),
            "urn:cap:exec:*".to_string(),
            "urn:cap:browse:read:core".to_string(),
        ];
        assert_eq!(unbounded_net_scopes(&wild), ["urn:cap:net:*"]);
        assert_eq!(unbounded_exec_scopes(&wild), ["urn:cap:exec:*"]);
        let narrow = vec![
            "urn:cap:net:localhost".to_string(),
            "urn:cap:net:api.example".to_string(),
            "urn:cap:browse:read:*".to_string(),
        ];
        assert!(unbounded_net_scopes(&narrow).is_empty(), "{narrow:?}");
        assert!(unbounded_exec_scopes(&narrow).is_empty());
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
