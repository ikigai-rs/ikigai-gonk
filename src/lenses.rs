//! gonk's lenses over the ledger graph (ledger #3959; ikigai-ui `docs/design/workspace.md`
//! section 7, proposals 14 and 15): data ikigai-ui's lens machinery reads, beside gonk's own
//! XSLT pages, never a replacement for them.
//!
//! | IRI | what it is |
//! |---|---|
//! | `urn:iki:ui:lens:name:gonk-item` (and `:query`) | the item lens: the item at the focus, its state, its children, its comments, and the dispatcher's close with its evidence (decision 17) |
//! | `urn:iki:ui:lens:name:gonk-dashboard` (and `:query`) | the Dashboard lens: the open and in-flight items of the ledger whose graph is the focus, a table whose rows carry `?item`, so a pane over it emits `item` |
//! | `urn:iki:ui:lens:type:https://ikigai-rs.dev/ns/ledger#Item` | `["gonk-item"]`: a ledger item's lenses |
//!
//! Bound in the HTTP door's page space ([`crate::web::space_with`]), beside the item and page
//! views they are lenses over, and not in the hub: a lens is a page's data, and the socket and
//! QUIC doors serve no page.
//!
//! Each query is read-only and names no SERVICE, so `urn:iki:ui:lens:prepare` accepts it; it runs
//! through `urn:sparql:select` under the CALLER's capability with the focus bound as a term, never
//! spliced. Serving them as pages (`/lens/name/{lens}/resource/{iri}`) and a workspace over them
//! are proposal 16, not this module.

use ikigai_core::{
    Description, EndpointSpace, Exact, FnEndpoint, Invocation, ReprType, Representation, Verb,
};

/// A lens's name under `urn:iki:ui:lens:name:`.
pub const NAME: &str = "urn:iki:ui:lens:name:";
/// The item lens's name.
pub const ITEM: &str = "gonk-item";
/// The Dashboard lens's name.
pub const DASHBOARD: &str = "gonk-dashboard";
/// A ledger item's type lens list.
pub const ITEM_TYPE: &str = "urn:iki:ui:lens:type:https://ikigai-rs.dev/ns/ledger#Item";

/// gonk's lenses: (name, lens JSON, query).
pub const LENSES: [(&str, &str, &str); 2] = [
    (
        ITEM,
        include_str!("lenses/gonk-item.json"),
        include_str!("lenses/gonk-item.rq"),
    ),
    (
        DASHBOARD,
        include_str!("lenses/gonk-dashboard.json"),
        include_str!("lenses/gonk-dashboard.rq"),
    ),
];

fn fixed(id: &str, title: &str, media: &'static str, body: &'static str) -> FnEndpoint {
    FnEndpoint::new(id, move |_: &Invocation<'_>| {
        // Not marked cacheable: the HTTP door's kernel stores nothing (`crate::doors`), as for
        // the page assets, and a fixed body is cheap to hand back.
        Ok(Representation::new(
            ReprType::new(media),
            body.as_bytes().to_vec(),
        ))
    })
    .with_description(
        Description::new(id)
            .title(title)
            .summary(title)
            .verb(Verb::Source)
            .output(media),
    )
}

/// `space` with the lenses, their queries and the item type's list bound.
pub fn bind(space: EndpointSpace) -> EndpointSpace {
    let mut space = space.bind(
        Exact::new(ITEM_TYPE),
        fixed(
            "gonk-lens-type-item",
            "A ledger item's lenses",
            "application/json",
            r#"["gonk-item"]"#,
        ),
    );
    for (name, lens, query) in LENSES {
        space = space
            .bind(
                Exact::new(format!("{NAME}{name}")),
                fixed(
                    &format!("gonk-lens-{name}"),
                    "A gonk lens",
                    "application/json",
                    lens,
                ),
            )
            .bind(
                Exact::new(format!("{NAME}{name}:query")),
                fixed(
                    &format!("gonk-lens-{name}-query"),
                    "A gonk lens's query",
                    "application/sparql-query",
                    query,
                ),
            );
    }
    space
}
