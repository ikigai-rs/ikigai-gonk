//! Keeping the cache sound over a dataset whose handle left `ikigai-store`.
//!
//! # The problem, stated exactly
//!
//! `DurableStore::open` keeps the dataset handle inside `ikigai-store`, so every write is a
//! write the kernel saw, and the store's reads are `.cacheable()` under its three write
//! threads. `open_shared` hands a second `Arc<Store>` out — which is the only way to give
//! `ikigai-browse` the annotation family over the SAME dataset the ledger lives in — and
//! `ikigai-store` responds by making **every** read `Expiry::Always`, permanently
//! (its `endpoints::with_freshness`, keyed on `DurableStore::is_covered`).
//!
//! That blanket is right for a crate that cannot see who else holds the handle. It is also
//! expensive in a way nothing catches: expiry PROPAGATES from dependencies, so a ledger read
//! — which is a sub-request to `urn:iki:store:graph-select` — becomes uncacheable too, even
//! though `ikigai-ledger` declares `.cacheable()` on its own representation. Taking the
//! handle would have de-cached the ledger as a side effect of adding a browse face.
//! Measured on 247 items: `urn:iki:ledger:items` goes from a cached read to a full re-query
//! every time (`tests/browse.rs` prints both).
//!
//! # ★ The answer: the sharer cannot reach a NAMED graph, so scoped reads stay fresh
//!
//! Accepting the blanket is not the only option, because gonk knows something
//! `ikigai-store` cannot: **exactly who the other holder is, and where it writes.**
//!
//! `ikigai-browse` writes every quad it stores — annotations, explanation archive, review
//! passes — into the store's **default graph**, hard-coded (`annotate.rs`, `explain.rs`,
//! `review.rs` all name `GraphName::DefaultGraph`; there is no configuration knob). And
//! `ikigai-store`'s scoped read face confines a query to one NAMED graph by construction:
//! it sets the query's default graph and its available named graphs to `[G]`, and the real
//! default graph has no IRI, so no `urn:cap:store:read:graph:` token can name it and no
//! scoped query can see it (`ikigai-store/src/scope.rs`, first section).
//!
//! So the invisible writer is invisible **only to reads that can see the default graph**:
//!
//! | read | can see browse's writes? | freshness here |
//! |---|---|---|
//! | `urn:iki:store:graph-{select,ask,construct,describe}` | no — confined to one named graph | cacheable, under the store's own three write threads |
//! | `urn:iki:store:{select,ask,construct,describe}` | YES — the broad faces read the whole dataset | `Expiry::Always`, as `open_shared` left them |
//! | `urn:iki:store:info` | YES — it counts every quad | `Expiry::Always` |
//!
//! [`scoped_reads_stay_fresh`] restores exactly the first row: the same
//! `.cacheable().depends_on(…)` triple `ikigai-store` would have declared on a covered
//! store, on the four scoped faces and nothing else. Every ledger read rides on it, because
//! every ledger read is a scoped sub-request. The one that does not is
//! `urn:iki:ledger:ledgers`, which asks *which graphs exist* through the broad
//! `urn:iki:store:select` under a root capability — so it is uncacheable here, by name, and
//! that is the whole list of what this composition gives up.
//!
//! ⚠ **The argument rests on a fact about another crate, so a test pins it at the boundary**
//! rather than trusting this comment: `tests/browse.rs::a_browse_write_touches_no_named_graph`
//! writes an annotation through the kernel and asserts the dataset's set of named graphs is
//! unchanged. The day `ikigai-browse` puts a quad in a named graph, that test is red here,
//! where the assumption is made — not silently stale in the cache of a running server.
//!
//! # What this deliberately does NOT do
//!
//! It does not widen a capability: the wrapper adds freshness to a representation and touches
//! nothing else, and the endpoint underneath still runs its own scope check. It does not make
//! a read cacheable whose endpoint made a volatile sub-request — the kernel meets a declared
//! expiry with `Invocation::dependency_expiry()` after `invoke` returns, so an inner
//! sub-request that is `Always` still wins. The store's query endpoints make no sub-requests.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    Description, Endpoint, EndpointSpace, Invocation, Representation, Request, Resolution, Result,
    Scope, Space, SpaceEntry,
};

/// The store faces whose answer is confined to one NAMED graph, and which therefore cannot
/// see a write made through a handle that left `ikigai-store`.
///
/// Exact IRIs, not a prefix: `urn:iki:store:graph-update` shares the prefix and is a WRITE,
/// and a prefix match here would have declared a write cacheable.
pub const SCOPED_READS: [&str; 4] = [
    "urn:iki:store:graph-select",
    "urn:iki:store:graph-ask",
    "urn:iki:store:graph-construct",
    "urn:iki:store:graph-describe",
];

/// Wrap `store` — the space from `ikigai_store::space` over a store opened with
/// `open_shared` — so its scoped reads are cacheable again. See the module docs.
///
/// A covered store needs none of this and must not be wrapped: `ikigai-store` already
/// declares the same three threads, and wrapping would be a second, quieter place where
/// this server decides what is fresh.
pub fn scoped_reads_stay_fresh(store: EndpointSpace) -> ScopedReadsStayFresh {
    ScopedReadsStayFresh { inner: store }
}

/// The space [`scoped_reads_stay_fresh`] builds.
pub struct ScopedReadsStayFresh {
    inner: EndpointSpace,
}

impl Space for ScopedReadsStayFresh {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        let restore =
            !request.verb.is_mutating() && SCOPED_READS.contains(&request.target.as_str());
        let resolution = self.inner.resolve(request, scope);
        if !restore {
            return resolution;
        }
        // `map_endpoint` rather than a hand-built `Resolved`: rebuilding one drops
        // `canonical`, which splits cache identity and golden threads in two.
        resolution.map_endpoint(|endpoint| Arc::new(Fresh { inner: endpoint }) as Arc<dyn Endpoint>)
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        // The catalog is the store's, unchanged: freshness is not a resource.
        self.inner.entries()
    }
}

/// One scoped read, with the freshness a covered store would have declared.
struct Fresh {
    inner: Arc<dyn Endpoint>,
}

#[async_trait]
impl Endpoint for Fresh {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        Ok(self
            .inner
            .invoke(inv)
            .await?
            // The store's own three, by name from `ikigai-store` rather than transcribed: a
            // fourth write door in that crate must appear here too, and a rename must not
            // compile.
            .cacheable()
            .depends_on(ikigai_store::UPDATE_THREAD)
            .depends_on(ikigai_store::LOAD_THREAD)
            .depends_on(ikigai_store::GRAPH_UPDATE_THREAD))
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        // Unchanged: the contract a mounting client reads, the capabilities the kernel
        // baseline-checks, and the arguments the engine routes are all the store's.
        self.inner.describe()
    }
}
