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

/// How much authority over one ledger. Cumulative, except [`Authority::Audit`] and
/// [`Authority::Worker`]: each level includes the ones before it, because there is no useful
/// "may delete but may not read".
///
/// ★ **`Worker` sits beside the chain, between `Read` and `Write`** (ledger
/// [#1143](http://localhost:1060/l/default/item/1143)): everything `Read` holds, plus taking,
/// renewing and releasing a claim and commenting, and NOTHING else. `Write` holds everything
/// `Worker` does (the ledger's write grant covers claim and comment), so a worker is not a
/// prefix of the next.
///
/// ★ **`Audit` sits beside the chain too, between `Read` and `Purge`** (ledger
/// [#1611](http://localhost:1060/l/default/item/1611)): everything `Read` holds, plus READING the
/// ledger's graveyard graph, and NOTHING that writes. Before it, the graveyard's read token came
/// only with `Purge`, so whoever could see what was deleted could also destroy it. `Purge` holds
/// everything `Audit` does; `Delete` does not (a delete only inserts into the graveyard), so
/// `--ledger L=delete --ledger L=audit` is delete with the graveyard visible and no purge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// `items`, `item:{id}`, `next`, `ledgers`.
    Read,
    /// …plus reading the ledger's graveyard (`urn:iki:ledger:graph:{name}:deleted`) through the
    /// store's per-graph read doors (`urn:sparql:*` with `graph=` names it). Writes nothing:
    /// not the ledger, not either graph, and not a purge.
    Audit,
    /// `Read` (not `Audit`), plus `claim` (take, renew, release) and `comment` (a hand-back
    /// included), through `ikigai-ledger` 0.7's per-verb grants. Not `close` or `reopen` (the dispatcher's verdict
    /// on the work), and not filing, linking, labeling, deferring or editing an item.
    Worker,
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
            "audit" => Ok(Authority::Audit),
            "worker" => Ok(Authority::Worker),
            "write" => Ok(Authority::Write),
            "delete" => Ok(Authority::Delete),
            "purge" => Ok(Authority::Purge),
            other => Err(format!(
                "`{other}` is not an authority (read | audit | worker | write | delete | purge)"
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
/// // A WORKER: claim and comment, never close (ikigai-ledger's published worker grant).
/// assert_eq!(
///     grants_for("acme", Authority::Worker).unwrap(),
///     [
///         "urn:cap:ledger:read:acme",
///         "urn:cap:store:read:graph:urn:iki:ledger:graph:acme",
///         "urn:cap:ledger:claim:acme",
///         "urn:cap:ledger:comment:acme",
///         "urn:cap:store:write:graph:urn:iki:ledger:graph:acme",
///     ]
/// );
/// // An AUDITOR: read, plus the graveyard's read token, and nothing that writes (ledger #1611).
/// assert_eq!(
///     grants_for("acme", Authority::Audit).unwrap(),
///     [
///         "urn:cap:ledger:read:acme",
///         "urn:cap:store:read:graph:urn:iki:ledger:graph:acme",
///         "urn:cap:store:read:graph:urn:iki:ledger:graph:acme:deleted",
///     ]
/// );
/// ```
///
/// ⚠ **A worker holds the ledger graph's store WRITE token**, because every ledger write goes to
/// the store under the caller's capability. On a bare kernel that token is raw quad authority
/// (one SPARQL update could close an item without the close grant); behind this server's doors
/// it is not, because a raw `urn:iki:store:graph-update` against a ledger graph is refused without
/// the explicit raw grant ([`crate::admit::raw_write_refusal`], ledger #878), which this function
/// never mints.
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
    if authority == Authority::Audit {
        // The token purge holds for the same graph (below), alone: the store's per-graph read
        // doors answer it, and no write door accepts a read token.
        grants.push(ikigai_store::cap_read_graph(&ledger.deleted_graph()));
        return Ok(grants);
    }
    if authority == Authority::Worker {
        grants.push(ledger.cap_claim());
        grants.push(ledger.cap_comment());
        grants.push(ikigai_store::cap_write_graph(&graph));
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
    // so among the WRITING roles the read token is purge's alone (ledger #760, #768); the one
    // other role holding it is the read-only audit (ledger #1611).
    grants.push(ikigai_store::cap_read_graph(&ledger.deleted_graph()));
    Ok(grants)
}

/// Whether `held` already carries `token`, counting what one ledger grant implies about
/// another: `urn:cap:ledger:write:{L}` covers `…:claim:{L}`, `…:comment:{L}` and `…:close:{L}`
/// (`ikigai-ledger` 0.7.0), and `…:about:{L}` (0.10.0). Used to say a minted grant ADDS something to what a caller already
/// holds, which a literal comparison would misjudge for a worker.
///
/// ```
/// use ikigai_gonk::grants::{grants_for, implied_by, Authority};
/// let write = grants_for("acme", Authority::Write).unwrap();
/// assert!(grants_for("acme", Authority::Worker)
///     .unwrap()
///     .iter()
///     .all(|t| implied_by(t, &write)));
/// assert!(implied_by("urn:cap:ledger:about:acme", &write));
/// assert!(!implied_by("urn:cap:ledger:claim:other", &write));
/// assert!(!implied_by("urn:cap:ledger:delete:acme", &write));
/// ```
pub fn implied_by(token: &str, held: &[String]) -> bool {
    if held.iter().any(|h| h == token) {
        return true;
    }
    ["claim", "comment", "close", "about"].iter().any(|verb| {
        token
            .strip_prefix(&format!("urn:cap:ledger:{verb}:"))
            .and_then(|name| Ledger::parse(name).ok())
            .is_some_and(|ledger| held.contains(&ledger.cap_write()))
    })
}

/// The prefix every ledger's graphs share: `urn:iki:ledger:graph:{name}` and its graveyard
/// `urn:iki:ledger:graph:{name}:deleted` (`ikigai_ledger::Ledger::graph`, `deleted_graph`).
/// A test pins it against both, so a change in how the ledger names its graphs fails here.
pub const LEDGER_GRAPH_PREFIX: &str = "urn:iki:ledger:graph:";

/// Every raw-write grant starts here; [`cap_raw_write_graph`] appends the graph.
pub const CAP_RAW_WRITE_PREFIX: &str = "urn:cap:gonk:raw-write:graph:";

/// The token that makes a ledger graph's store write token RAW authority at a network door
/// (ledger [#878](http://localhost:1060/l/default/item/878)) — required by the door's admission
/// ([`crate::admit::raw_write_refusal`]) and by no endpoint.
///
/// ```
/// use ikigai_gonk::grants::cap_raw_write_graph;
/// assert_eq!(
///     cap_raw_write_graph("urn:iki:ledger:graph:default"),
///     "urn:cap:gonk:raw-write:graph:urn:iki:ledger:graph:default"
/// );
/// ```
pub fn cap_raw_write_graph(graph: &str) -> String {
    format!("{CAP_RAW_WRITE_PREFIX}{graph}")
}

/// Why `worker` is refused for the browse graph: it is a LEDGER authority (claim and comment),
/// and the browse graph has neither.
pub const WORKER_IS_A_LEDGER_AUTHORITY: &str =
    "`worker` is a ledger authority (claim and comment, no close): the browse graph has no \
     claims or comments to grant. Use `read` or `write`";

/// Why `audit` is refused for the browse graph: it reads a LEDGER's graveyard, and the browse
/// graph has none.
pub const AUDIT_IS_A_LEDGER_AUTHORITY: &str =
    "`audit` is a ledger authority (read, plus the ledger's graveyard of deleted items): the \
     browse graph has no graveyard. Use `read` or `write`";

/// ★ **A ledger's graph as a STORE grant**: raw SPARQL UPDATE over the triples of one ledger's
/// graph, through `urn:iki:store:graph-update` at a network door — the operator role that
/// `--ledger-graph <ledger>` mints, and the only thing that does.
///
/// ```
/// use ikigai_gonk::grants::ledger_graph_grants;
/// assert_eq!(
///     ledger_graph_grants("acme").unwrap(),
///     [
///         "urn:cap:store:read:graph:urn:iki:ledger:graph:acme",
///         "urn:cap:store:write:graph:urn:iki:ledger:graph:acme",
///         "urn:cap:gonk:raw-write:graph:urn:iki:ledger:graph:acme",
///     ]
/// );
/// ```
///
/// # Why it is not [`grants_for`] at any authority
///
/// A ledger grant carries the same store write token, because `ikigai-ledger` issues its own
/// writes through the store under the caller's capability (`append` fails without it). So
/// that token, on its own, means "may use the ledger" as often as "may write its triples",
/// and through 1d02e53 a door treated it as the second: any ledger writer, the anonymous
/// loopback grant included, could insert a `ledger:author` naming someone else's passkey. The
/// last token here is what says "raw" — the door refuses a raw write to a ledger graph
/// without it, and nothing else requires it, so it adds no authority anywhere else.
///
/// ⚠ **One graph, not the graveyard.** A raw repair of a deleted item's archive is rare enough
/// to belong to the owner's socket, whose root reaches every graph.
///
/// # Errors
///
/// When `ledger` cannot be a ledger name, as [`grants_for`].
#[must_use = "these are capability tokens; minting them and dropping them grants nothing"]
pub fn ledger_graph_grants(ledger: &str) -> Result<Vec<String>, String> {
    let graph = Ledger::parse(ledger).map_err(|e| e.to_string())?.graph();
    Ok(vec![
        ikigai_store::cap_read_graph(&graph),
        ikigai_store::cap_write_graph(&graph),
        cap_raw_write_graph(&graph),
    ])
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
/// an annotation is an ordinary write in this one graph. So `write` and above are exactly
/// read+write here, and it says so rather than refusing, because an operator spelling
/// `--browse-graph delete` means "and may remove things". `worker` is refused
/// ([`WORKER_IS_A_LEDGER_AUTHORITY`]): it means claim and comment, which this graph has not.
/// So is `audit` ([`AUDIT_IS_A_LEDGER_AUTHORITY`]): it means a ledger's graveyard, which this
/// graph has not either, and reading it as `read` would hide that the operator asked for more.
///
/// # Errors
///
/// When this server's choice is the DEFAULT graph ([`crate::browse::Graph::TheDefault`]),
/// which has no IRI for a token to name. The error is the explanation, because the operator
/// asking has every reason to think a grant should exist. And [`Authority::Worker`] and
/// [`Authority::Audit`].
#[must_use = "these are capability tokens; minting them and dropping them grants nothing"]
pub fn browse_graph_grants(authority: Authority) -> Result<Vec<String>, String> {
    let chosen = crate::browse::Graph::chosen();
    let graph = chosen.named().ok_or_else(|| {
        "this server writes browse's quads in the DEFAULT graph, which has no IRI — no \
         `urn:cap:store:*:graph:` token can name it, so there is no grant to mint. See \
         `browse::Graph`."
            .to_string()
    })?;
    if authority == Authority::Worker {
        return Err(WORKER_IS_A_LEDGER_AUTHORITY.to_string());
    }
    if authority == Authority::Audit {
        return Err(AUDIT_IS_A_LEDGER_AUTHORITY.to_string());
    }
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
    let mut configured: Vec<&str> = settings
        .browse_roots
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    configured.sort_by(|a, b| crate::roots::name_order(a, b));
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

/// How much authority over ONE script, or over a NAMESPACE of them (ledger
/// [#939](http://localhost:1060/l/default/item/939), [`crate::script`]) —
/// `--script <name>=<read|run|publish>` or `--script <namespace>-*=<…>`, the way `--ledger`
/// names a ledger. Cumulative, like [`Authority`].
///
/// ★ **A name, or a namespace; never "every script".** Since `ikigai-script` 0.3.0 (ledger
/// #1174, #1196) a script grant is `urn:cap:script:{act}:{name}` for one script or
/// `urn:cap:script:{act}:{namespace}-*` for every name beginning `{namespace}-`
/// (`ikigai_script::authority::cap_namespace`; `team-*` covers `team-report` and
/// `team-a-report`, never `teammate` or `team`), and the library's `authority::holds` is the
/// whole rule. There is still no "may publish any script" token below root:
/// `urn:cap:script:write:*` is spelled like the family each door declares and grants nothing,
/// and a namespace must be a well-formed name prefix ending at a `-`. The owner's socket
/// (root) publishes anything.
///
/// The name `public` is the one exception, and it is the library's: it is reserved as a script
/// name, so `urn:cap:script:{run,read}:public` are the grants that run and read every script
/// its publisher marked `public=true` — `--script public=run`. The anonymous HTTP caller holds
/// them already ([`crate::script::ANONYMOUS`]); a QUIC client holds them only when granted.
///
/// ⚠ `publish` on a CERTIFICATE saves drafts and nothing more: the QUIC door refuses a machine's
/// publish and retire (a machine drafts, a person publishes; [`crate::admit`]). It is the same
/// grant table for both doors, so the rule lives at the door rather than in the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptAuthority {
    /// Read the script's source and head record without running it: `urn:cap:script:read:{name}`.
    Read,
    /// …plus run it, as a read (`…:result`) and for effects (`…:runs`): `urn:cap:script:run:{name}`.
    Run,
    /// …plus publish, replace or draft it (`urn:cap:script:write:{name}`) and retire it
    /// (`urn:cap:script:delete:{name}`).
    Publish,
}

impl std::str::FromStr for ScriptAuthority {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        match value {
            "read" => Ok(ScriptAuthority::Read),
            "run" => Ok(ScriptAuthority::Run),
            "publish" => Ok(ScriptAuthority::Publish),
            other => Err(format!(
                "`{other}` is not a script authority (read | run | publish)"
            )),
        }
    }
}

/// Every token an identity needs for `authority` over the script `name`, or over every script
/// in a namespace when `name` is spelled `{namespace}-*`.
///
/// ```
/// use ikigai_gonk::grants::{script_grants, ScriptAuthority};
/// assert_eq!(
///     script_grants("open-by-label", ScriptAuthority::Publish).unwrap(),
///     [
///         "urn:cap:script:read:open-by-label",
///         "urn:cap:script:run:open-by-label",
///         "urn:cap:script:write:open-by-label",
///         "urn:cap:script:delete:open-by-label",
///     ]
/// );
/// // A NAMESPACE: every script whose name begins `team-` (ikigai-script 0.3.0).
/// assert_eq!(
///     script_grants("team-*", ScriptAuthority::Publish).unwrap(),
///     [
///         "urn:cap:script:read:team-*",
///         "urn:cap:script:run:team-*",
///         "urn:cap:script:write:team-*",
///         "urn:cap:script:delete:team-*",
///     ]
/// );
/// // Every PUBLIC script, to run and read: the anonymous HTTP caller's pair.
/// assert_eq!(
///     script_grants("public", ScriptAuthority::Run).unwrap(),
///     ["urn:cap:script:read:public", "urn:cap:script:run:public"]
/// );
/// assert!(script_grants("public", ScriptAuthority::Publish).is_err());
/// assert!(script_grants("Not A Name", ScriptAuthority::Run).is_err());
/// // No "every script" grant, and a wildcard only after a `-`.
/// assert!(script_grants("*", ScriptAuthority::Run).is_err());
/// assert!(script_grants("team*", ScriptAuthority::Run).is_err());
/// assert!(script_grants("-*", ScriptAuthority::Run).is_err());
/// ```
///
/// ⚠ No store or ledger token: what a script READS is the runner's own grant (`--ledger`),
/// intersected with what its publisher held. A run grant with no ledger grant runs nothing that
/// reads a ledger.
///
/// # Errors
///
/// When `name` cannot be a script name (`ikigai_script::name::validate`) or a namespace
/// (`ikigai_script::authority::cap_namespace`), or is `public` with more than `run`. A grant
/// that matches nothing would otherwise be a silent denial at first use.
pub fn script_grants(name: &str, authority: ScriptAuthority) -> Result<Vec<String>, String> {
    use ikigai_script::authority::{cap, cap_namespace, Act};
    let acts: &[Act] = match authority {
        ScriptAuthority::Read => &[Act::Read],
        ScriptAuthority::Run => &[Act::Read, Act::Run],
        ScriptAuthority::Publish => &[Act::Read, Act::Run, Act::Write, Act::Delete],
    };
    if let Some(namespace) = name.strip_suffix("-*") {
        return acts
            .iter()
            .map(|act| cap_namespace(*act, namespace).map_err(|e| format!("--script {name}: {e}")))
            .collect();
    }
    if name == "public" {
        if authority == ScriptAuthority::Publish {
            return Err(
                "`public=publish`: `public` names every public script, to run and read; \
                 a script is published under its own name (`--script <name>=publish`)"
                    .to_string(),
            );
        }
    } else {
        ikigai_script::name::validate(name).map_err(|e| {
            format!("--script {name}: {e} (a namespace is spelled `<namespace>-*`)")
        })?;
    }
    Ok(acts.iter().map(|act| cap(*act, name)).collect())
}

/// Why `--nl draft` cannot be minted on a server with no `gonk.mount`: the refusal itself, so
/// the CLI and the tests say the same sentence.
pub const NL_DRAFT_NEEDS_A_MOUNT: &str =
    "--nl draft grants drafting with urn:nl:sparql, which asks the model the `gonk.mount` line \
     names — and this server has none, so it binds no drafter and there is no host for the net \
     grant to name. Add `gonk.mount = \"prefer urn:llm:=…\"` to config.toml";

/// The token that lets an identity DRAFT a query from a sentence (`--nl draft`, ledger
/// [#1138](http://localhost:1060/l/default/item/1138)): `urn:cap:net:{host}` for the mounted
/// peer, which [`crate::nl`] declares on `urn:nl:sparql` and the mount enforces exactly
/// ([`crate::mount::net_grant`]).
///
/// ```
/// use ikigai_gonk::grants::{nl_draft_grants, NL_DRAFT_NEEDS_A_MOUNT};
/// assert_eq!(nl_draft_grants(Some("127.0.0.1")).unwrap(), ["urn:cap:net:127.0.0.1"]);
/// assert_eq!(nl_draft_grants(None).unwrap_err(), NL_DRAFT_NEEDS_A_MOUNT);
/// ```
///
/// # Why ONE token, and what it does not carry
///
/// It is the spend: the model is the only thing a draft reaches that the caller's other grants
/// do not already reach. What the draft READS is the caller's own `--ledger` / `--browse-graph`
/// grant (the grounding and the dry run run under it), and where it is SAVED is a
/// `--script <name>=publish` name, or a namespace (`--script nl-sparql-*=publish` covers the
/// drafter's own default names, ikigai-script 0.3.0); a draft with no grant for its name is
/// answered unsaved. ⚠ The same token is `--browse derive`'s net half, and it reaches `urn:llm:*`
/// directly through every door, as it does for derive: the authority is "may spend the
/// mounted model", not "may draft". No anonymous grant carries it.
///
/// # Errors
///
/// [`NL_DRAFT_NEEDS_A_MOUNT`] with no mount, and a host that could not be one.
#[must_use = "these are capability tokens; minting them and dropping them grants nothing"]
pub fn nl_draft_grants(net_host: Option<&str>) -> Result<Vec<String>, String> {
    let host = net_host.ok_or_else(|| NL_DRAFT_NEEDS_A_MOUNT.to_string())?;
    if host.is_empty() || host == "*" || host.chars().any(char::is_whitespace) {
        return Err(format!(
            "`{host}` cannot be the mounted peer's host in a net grant"
        ));
    }
    Ok(vec![format!("urn:cap:net:{host}")])
}

/// Everything an ANONYMOUS loopback caller of the HTTP door holds: the READ tokens of the
/// ledgers in `gonk.http.ledger`, and [`crate::script::ANONYMOUS`] (run and read a PUBLIC
/// script). Signing in (a passkey session) or a client certificate is what writes.
///
/// ★ Ledger [#1687](http://localhost:1062/l/default/item/1687): this was `Authority::Write`, so
/// any process on this machine, and on every machine a reverse tunnel puts this door on, could
/// file, comment and label items, and an unattended worker that selects by label and reads
/// comments into its prompt would act on them. The passkey sign-in flow needs no ledger token
/// (`urn:iki:gonk:passkey:*` are this server's own names), so it is unchanged.
///
/// ```
/// use ikigai_gonk::grants::anonymous_http_grants;
/// assert_eq!(
///     anonymous_http_grants(&["default".to_string()]).unwrap(),
///     [
///         "urn:cap:ledger:read:default",
///         "urn:cap:store:read:graph:urn:iki:ledger:graph:default",
///         "urn:cap:script:run:public",
///         "urn:cap:script:read:public",
///     ]
/// );
/// ```
///
/// # Errors
///
/// When a ledger name cannot be a ledger, as [`grants_for`].
pub fn anonymous_http_grants(ledgers: &[String]) -> Result<Vec<String>, String> {
    let mut tokens = grants_for_all(ledgers, Authority::Read)?;
    tokens.extend(crate::script::ANONYMOUS.iter().map(|s| s.to_string()));
    Ok(tokens)
}

/// The read the HTTP door's caller needs for the RDF Toolbox ([`crate::toolbox`], ledger
/// [#1933](http://localhost:1062/l/default/item/1933)): `urn:cap:fs:read:<dist>/<name>` for each
/// file in `ikigai_toolbox::FILES`, and nothing else.
///
/// The toolbox reads its build under its CALLER's capability, so the door's caller must hold a
/// read on the build. Named file by file rather than as the directory: the family already reads
/// only its allowlist, but the directory is also reachable by its own name
/// (`urn:iki:toolbox:dist:{path}`), and a directory grant would serve anything else an operator
/// left in it. No write or delete token: nothing here writes the build.
///
/// ```
/// use std::path::Path;
/// use ikigai_gonk::grants::toolbox_read_grants;
/// let grants = toolbox_read_grants(Path::new("/srv/toolbox/dist"));
/// assert_eq!(grants.len(), ikigai_toolbox::FILES.len());
/// assert_eq!(grants[0], "urn:cap:fs:read:/srv/toolbox/dist/toolbox.html");
/// assert!(grants.contains(&"urn:cap:fs:read:/srv/toolbox/dist/engine.wasm".to_string()));
/// assert!(grants.iter().all(|g| g.starts_with("urn:cap:fs:read:/srv/toolbox/dist/")));
/// ```
pub fn toolbox_read_grants(dist: &std::path::Path) -> Vec<String> {
    ikigai_toolbox::FILES
        .iter()
        .map(|(name, _)| format!("urn:cap:fs:read:{}", dist.join(name).to_string_lossy()))
        .collect()
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

/// `urn:cap:exec:*` — the OFFERING wildcard over `ikigai-repo`'s per-tool exec scopes, which
/// as a GRANT means "run any program".
pub const CAP_EXEC_ANY: &str = "urn:cap:exec:*";

/// The scopes in `scopes` that grant unbounded process execution.
///
/// ★ **A wildcard is an offering form, not a grant form, and the two look identical in a
/// JSON file.** `ikigai-repo` declared `urn:cap:exec:*` on `urn:system:exec` (served through
/// 0.2.x, gone since 0.3.0, ledger #1706) to say "holds some grant under this prefix"; the
/// same string written into `grants.json` says "may run
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
/// ★ And the onboarding controller's writes (ledger #2009, [`crate::onboard::CAP_ADMIN`]):
/// minting an invite, approving one (a signature under the controller key), revoking, applying.
/// Brian, 2026-10-22: the controller key lives on plasma only, so an approval never comes from
/// the passkey door or a QUIC client — the same airlock.
///
/// ⚠ Refused as a GRANT, not as a requirement. The endpoints declare them and the root
/// capability on the socket satisfies them, exactly as it satisfies everything else.
pub const CAP_GONK_ADMIN: [&str; 3] = [
    crate::backup::CAP_BACKUP,
    crate::backup::CAP_RESTORE,
    crate::onboard::CAP_ADMIN,
];

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

    /// ★ Ledger #1611: the AUDIT role reads the graveyard and writes nothing. Literals, for
    /// the reason above. Before it, the graveyard's read token came only with purge.
    #[test]
    fn an_audit_grant_reads_the_graveyard_and_writes_nothing() {
        let audit = grants_for("acme", Authority::Audit).unwrap();
        assert_eq!(
            audit,
            [
                "urn:cap:ledger:read:acme",
                "urn:cap:store:read:graph:urn:iki:ledger:graph:acme",
                "urn:cap:store:read:graph:urn:iki:ledger:graph:acme:deleted",
            ]
        );
        assert_eq!("audit".parse::<Authority>(), Ok(Authority::Audit));
        assert!(broad_store_scopes(&audit).is_empty());
        // No write of any kind: not a ledger verb, not either graph's store write token.
        for token in &audit {
            assert!(
                !token.contains(":write:")
                    && !token.contains(":delete:")
                    && !token.contains(":purge:")
                    && !token.contains(":claim:")
                    && !token.contains(":comment:"),
                "{token}"
            );
        }
        // Purge holds everything audit does; delete does not hold the graveyard read.
        let purge = grants_for("acme", Authority::Purge).unwrap();
        assert!(audit.iter().all(|t| purge.contains(t)), "{purge:?}");
        let delete = grants_for("acme", Authority::Delete).unwrap();
        assert!(!delete.contains(&audit[2]), "{delete:?}");
        // And it is a ledger authority: the browse graph has no graveyard.
        assert_eq!(
            browse_graph_grants(Authority::Audit),
            Err(AUDIT_IS_A_LEDGER_AUTHORITY.to_string())
        );
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

    /// ★ The prefix the door's raw-write rule reads is the ledger's own spelling of both its
    /// graphs, and no authority of a LEDGER grant carries the raw token — that is the whole
    /// difference between the two kinds of grant (ledger #878).
    #[test]
    fn the_ledger_graph_prefix_is_the_ledgers_own_and_no_ledger_grant_is_raw() {
        let acme = Ledger::parse("acme").unwrap();
        assert!(acme.graph().starts_with(LEDGER_GRAPH_PREFIX));
        assert!(acme.deleted_graph().starts_with(LEDGER_GRAPH_PREFIX));
        for authority in [
            Authority::Read,
            Authority::Audit,
            Authority::Worker,
            Authority::Write,
            Authority::Delete,
            Authority::Purge,
        ] {
            let grants = grants_for("acme", authority).unwrap();
            assert!(
                grants.iter().all(|g| !g.starts_with(CAP_RAW_WRITE_PREFIX)),
                "{authority:?}: {grants:?}"
            );
        }
        let raw = ledger_graph_grants("acme").unwrap();
        assert!(broad_store_scopes(&raw).is_empty());
        assert!(gonk_admin_scopes(&raw).is_empty());
        assert!(ledger_graph_grants("items").is_err(), "reserved");
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
