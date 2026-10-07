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
//! [`broad_store_scopes`] is one of the three checks `quic::grant_refusal` applies to every
//! grant, on every path.

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
    // ★ Purge READS the graveyard (ikigai-ledger 0.3.0 resolves a deleted item through its
    // tombstone and clears what it archived), and since ikigai-store 0.2.6 an update with a
    // WHERE needs the read grant on the graph it reads. A plain delete only INSERTs there,
    // so the read token is purge's alone (ledger #760, #768).
    grants.push(ikigai_store::cap_read_graph(&ledger.deleted_graph()));
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
/// are separate, and this function mints none of them ([`browse_role_grants`] does). An
/// identity holding only these two can read browse's graph and cannot read a file.
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

/// What a browsing identity may do with the repositories this server serves — a ROLE, the way
/// `--browse-graph` is one, never a list of scopes an operator assembles (ledger
/// [#435](http://localhost:1060/l/default/item/435)).
///
/// Cumulative, like [`Authority`]: `derive` holds everything `read` does.
///
/// ★ **`derive` is the dangerous capability, and it is ONE word on purpose.** Deriving an
/// explanation or a review spends the mounted peer's inference. As three loose tokens
/// (`urn:cap:browse:read:*`, `urn:cap:annotate`, `urn:cap:net:{host}`) an operator had to
/// assemble that authority by hand and could not see it in a flag list; as a role it is a
/// thing `--help` names and a grant visibly carries.
///
/// ⚠ **And `derive` bundles `urn:cap:annotate` with the net grant, which is load-bearing for
/// the review interlock.** A headless reviewer needs net WITHOUT annotate, and
/// [`crate::trigger::check_reviewer`] refuses any grant that can publish. No role here mints
/// net without annotate, so nothing this server mints can arm the trigger: a role that
/// spent inference but could not publish would be exactly a mintable reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseRole {
    /// Read the repositories (file contents, trees, git state, the archive listings and the
    /// annotations on them through `urn:repo:{root}:*`), and the browse graph's quads through
    /// the store's per-graph read door. Spends nothing and writes nothing.
    Read,
    /// …plus `urn:cap:annotate` (mint annotations and publish findings) and
    /// `urn:cap:net:{host}` for the mounted peer, so the identity may EXPLAIN and REVIEW —
    /// which spends that peer's inference.
    Derive,
}

impl std::str::FromStr for BrowseRole {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        match value {
            "read" => Ok(BrowseRole::Read),
            "derive" => Ok(BrowseRole::Derive),
            other => Err(format!("`{other}` is not a browse role (read | derive)")),
        }
    }
}

/// Why `derive` cannot be minted on a server with no `gonk.mount` — the refusal itself, so
/// the CLI and the tests say the same sentence.
pub const DERIVE_NEEDS_A_MOUNT: &str =
    "--browse derive grants explain and review, which derive through the peer a `gonk.mount` \
     line names — and this server has none, so there is no host for the net grant to name and \
     nothing a derivation could reach. Add `gonk.mount = \"prefer urn:llm:=…\"` to config.toml \
     (README, \"Explaining what it browses\"), or grant `--browse read`";

/// Every token a browsing identity needs for `role` — over every root (`roots` empty, the
/// all-roots wildcard) or over exactly the roots named.
///
/// `net_host` is where the mounted LLM peer lives ([`crate::config::Settings::mount_host`]);
/// `derive` needs it and `read` ignores it.
///
/// ```
/// use ikigai_gonk::grants::{browse_role_grants, BrowseRole};
/// assert_eq!(
///     browse_role_grants(BrowseRole::Read, &[], None).unwrap(),
///     [
///         "urn:cap:browse:read:*",
///         "urn:cap:store:read:graph:urn:iki:browse:graph:default",
///     ]
/// );
/// assert_eq!(
///     browse_role_grants(BrowseRole::Derive, &["ikigai-core".to_string()], Some("127.0.0.1"))
///         .unwrap(),
///     [
///         "urn:cap:browse:read:ikigai-core",
///         "urn:cap:store:read:graph:urn:iki:browse:graph:default",
///         "urn:cap:annotate",
///         "urn:cap:net:127.0.0.1",
///     ]
/// );
/// ```
///
/// ⚠ **No browse-graph WRITE token, and that is measured, not forgotten.** `ikigai-browse`
/// writes its archive, its annotations and its findings through its own handle on this
/// dataset, not through `urn:iki:store:graph-update`, so neither deriving nor annotating
/// needs it. That token is raw quad authority over the whole graph — every annotation, every
/// finding — and stays what `--browse-graph write` says it is.
///
/// # Errors
///
/// `derive` with no `net_host` ([`DERIVE_NEEDS_A_MOUNT`]); a root that could not be a root
/// name (`*`, whitespace, or anything [`crate::browse::check_root_name`] refuses — each would
/// forge a different token); and the browse graph's own refusal ([`browse_graph_grants`]).
#[must_use = "these are capability tokens; minting them and dropping them grants nothing"]
pub fn browse_role_grants(
    role: BrowseRole,
    roots: &[String],
    net_host: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut grants: Vec<String> = Vec::new();
    if roots.is_empty() {
        grants.push(ikigai_browse::CAP_WILDCARD.to_string());
    }
    for root in roots {
        if root == "*" || root.chars().any(char::is_whitespace) {
            return Err(format!(
                "--root `{root}` cannot be a browse root name; omit --root for every root"
            ));
        }
        crate::browse::check_root_name(root)?;
        let token = format!("{}{root}", ikigai_browse::CAP_PREFIX);
        if !grants.contains(&token) {
            grants.push(token);
        }
    }
    grants.extend(browse_graph_grants(Authority::Read)?);
    if role == BrowseRole::Read {
        return Ok(grants);
    }
    let host = net_host.ok_or_else(|| DERIVE_NEEDS_A_MOUNT.to_string())?;
    if host.is_empty() || host == "*" || host.chars().any(char::is_whitespace) {
        return Err(format!(
            "`{host}` cannot be the mounted peer's host in a net grant"
        ));
    }
    grants.push(ikigai_browse::CAP_ANNOTATE.to_string());
    grants.push(format!("urn:cap:net:{host}"));
    Ok(grants)
}

/// [`browse_role_grants`] for THIS server's configuration: every `--root` must be a
/// configured `gonk.browse.root`, and `derive`'s net grant names the host of this server's
/// own `gonk.mount` ([`crate::config::Settings::mount_host`]).
///
/// ★ Both checks turn a silent denial into a refusal at mint time. A root this server does
/// not serve is a token that matches nothing; a net grant for a host the mount does not name
/// would say the identity may reach somewhere it may not, and a hard-coded `localhost` says
/// exactly that the moment the peer moves to another machine.
///
/// # Errors
///
/// A root that is not configured, `derive` on a server that binds no explain or review (no
/// `gonk.mount`, or no `gonk.browse.root` for it to derive over), or any refusal of
/// [`browse_role_grants`].
pub fn browse_role_for(
    settings: &crate::config::Settings,
    role: BrowseRole,
    roots: &[String],
) -> Result<Vec<String>, String> {
    let configured: Vec<&str> = settings
        .browse_roots
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    for root in roots {
        if !configured.contains(&root.as_str()) {
            return Err(format!(
                "--root `{root}` is not a gonk.browse.root on this server ({}), so a token \
                 naming it would match nothing",
                if configured.is_empty() {
                    "none is configured".to_string()
                } else {
                    format!("configured: {}", configured.join(", "))
                }
            ));
        }
    }
    let host = settings.mount_host();
    if role == BrowseRole::Derive && host.is_some() && configured.is_empty() {
        return Err(
            "--browse derive grants explain and review over this server's browse roots, and \
             it has no gonk.browse.root — so neither is bound here and the grant would spend \
             nothing. Add a root (`ikigai-gonk checkout … --write-config`) first"
                .to_string(),
        );
    }
    browse_role_grants(role, roots, host.as_deref())
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

/// The scopes in `scopes` that are one of the backup family's tokens — asked by
/// `quic::grant_refusal`, and through it by every path that turns a grant into scopes.
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
/// browse's wildcard declaration, which is what the offering form means — and since ledger
/// #805 the mount then checks that the narrow grant names the host it dials
/// ([`crate::mount::net_grant`]), which the kernel's prefix match on the wildcard cannot.
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
        assert_eq!(
            purge[grants.len()..],
            [
                "urn:cap:ledger:purge:acme",
                "urn:cap:store:read:graph:urn:iki:ledger:graph:acme:deleted",
            ]
        );
        assert!(broad_store_scopes(&purge).is_empty());
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

    /// ★ Each browse ROLE's tokens as literals (ledger #435), for the reason the ledger's
    /// are: they go into an operator's `grants.json`, and a change to how `ikigai-browse`
    /// spells a scope, or how this server names its graph, has to fail HERE, where the
    /// operator's file would have to change too.
    #[test]
    fn each_browse_role_mints_exactly_these_tokens() {
        assert_eq!(
            browse_role_grants(BrowseRole::Read, &[], None).unwrap(),
            [
                "urn:cap:browse:read:*",
                "urn:cap:store:read:graph:urn:iki:browse:graph:default",
            ]
        );
        assert_eq!(
            browse_role_grants(BrowseRole::Derive, &[], Some("127.0.0.1")).unwrap(),
            [
                "urn:cap:browse:read:*",
                "urn:cap:store:read:graph:urn:iki:browse:graph:default",
                "urn:cap:annotate",
                "urn:cap:net:127.0.0.1",
            ]
        );
        let roots = vec!["ikigai-core".to_string(), "ikigai-gonk".to_string()];
        assert_eq!(
            browse_role_grants(BrowseRole::Derive, &roots, Some("localhost")).unwrap(),
            [
                "urn:cap:browse:read:ikigai-core",
                "urn:cap:browse:read:ikigai-gonk",
                "urn:cap:store:read:graph:urn:iki:browse:graph:default",
                "urn:cap:annotate",
                "urn:cap:net:localhost",
            ]
        );
        // Read ignores the host: it spends nothing, so it names no peer.
        assert_eq!(
            browse_role_grants(BrowseRole::Read, &[], Some("127.0.0.1")).unwrap(),
            browse_role_grants(BrowseRole::Read, &[], None).unwrap()
        );
    }

    /// ★ Nothing a role mints is a token this server refuses as a GRANT: no broad store
    /// token, no offering wildcard, no backup family — so a minted grant always admits the
    /// identity it was minted for. And the browse graph's WRITE door is not in either role.
    #[test]
    fn a_role_mints_nothing_this_server_refuses_and_no_raw_graph_write() {
        for role in [BrowseRole::Read, BrowseRole::Derive] {
            let grants = browse_role_grants(role, &[], Some("127.0.0.1")).unwrap();
            assert!(broad_store_scopes(&grants).is_empty(), "{grants:?}");
            assert!(unbounded_net_scopes(&grants).is_empty(), "{grants:?}");
            assert!(unbounded_exec_scopes(&grants).is_empty(), "{grants:?}");
            assert!(gonk_admin_scopes(&grants).is_empty(), "{grants:?}");
            let write = &browse_graph_grants(Authority::Write).unwrap()[1];
            assert!(!grants.contains(write), "{role:?} must not carry {write}");
        }
    }

    #[test]
    fn derive_without_a_mount_and_a_root_that_forges_a_token_are_refused() {
        let refused = browse_role_grants(BrowseRole::Derive, &[], None).unwrap_err();
        assert_eq!(refused, DERIVE_NEEDS_A_MOUNT);
        assert!(refused.contains("gonk.mount"), "{refused}");
        for host in ["*", "", "a b"] {
            assert!(
                browse_role_grants(BrowseRole::Derive, &[], Some(host)).is_err(),
                "`{host}`"
            );
        }
        for root in ["*", "a:b", "", "a b", "x/y"] {
            assert!(
                browse_role_grants(BrowseRole::Read, &[root.to_string()], None).is_err(),
                "`{root}`"
            );
        }
        assert!("write".parse::<BrowseRole>().is_err());
        assert_eq!("derive".parse::<BrowseRole>(), Ok(BrowseRole::Derive));
    }

    /// ★ **The net scope is this server's mount's host, read from the config — never typed.**
    /// A quic mount at `127.0.0.1` mints `urn:cap:net:127.0.0.1` (not `localhost`); a socket
    /// mints `localhost`; a server with no mount refuses `derive` and still mints `read`; and
    /// a `--root` this server does not serve is refused rather than written as a token that
    /// matches nothing.
    #[test]
    fn the_net_scope_is_derived_from_this_servers_own_mount() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let repo = dir.path().join("repo");
        let certs = dir.path().join("certs");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(&certs).unwrap();
        let homes = crate::config::Homes {
            home: dir.path().to_path_buf(),
            config: dir.path().join("config"),
            data: dir.path().join("data"),
        };
        let settings = |text: String| {
            crate::config::settings(&crate::config::Flags::default(), &text, &homes)
                .expect("a scratch config")
        };
        let root = format!("gonk.browse.root = \"demo={}\"\n", repo.display());
        let quic = settings(format!(
            "{root}gonk.mount = \"prefer urn:llm:=quic://127.0.0.1:4433 {}\"\n",
            certs.display()
        ));
        assert_eq!(
            browse_role_for(&quic, BrowseRole::Derive, &[]).unwrap(),
            [
                "urn:cap:browse:read:*",
                "urn:cap:store:read:graph:urn:iki:browse:graph:default",
                "urn:cap:annotate",
                "urn:cap:net:127.0.0.1",
            ]
        );
        let remote = settings(format!(
            "{root}gonk.mount = \"prefer urn:llm:=quic://plasma.local:4433 {}\"\n",
            certs.display()
        ));
        assert_eq!(
            browse_role_for(&remote, BrowseRole::Derive, &["demo".to_string()])
                .unwrap()
                .last()
                .map(String::as_str),
            Some("urn:cap:net:plasma.local")
        );
        let socket = settings(format!(
            "{root}gonk.mount = \"prefer urn:llm:=/nonexistent/llm.sock\"\n"
        ));
        assert_eq!(
            browse_role_for(&socket, BrowseRole::Derive, &[])
                .unwrap()
                .last()
                .map(String::as_str),
            Some("urn:cap:net:localhost")
        );

        let unmounted = settings(root.clone());
        assert_eq!(
            browse_role_for(&unmounted, BrowseRole::Derive, &[]).unwrap_err(),
            DERIVE_NEEDS_A_MOUNT
        );
        assert!(browse_role_for(&unmounted, BrowseRole::Read, &[]).is_ok());

        let elsewhere =
            browse_role_for(&quic, BrowseRole::Read, &["core".to_string()]).unwrap_err();
        assert!(elsewhere.contains("`core`"), "{elsewhere}");
        assert!(elsewhere.contains("configured: demo"), "{elsewhere}");

        // A mount with no root binds no explain and no review: derive would spend nothing.
        let rootless = settings(format!(
            "gonk.mount = \"prefer urn:llm:=quic://127.0.0.1:4433 {}\"\n",
            certs.display()
        ));
        let refused = browse_role_for(&rootless, BrowseRole::Derive, &[]).unwrap_err();
        assert!(refused.contains("no gonk.browse.root"), "{refused}");
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
