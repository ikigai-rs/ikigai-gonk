# Changelog

## Unreleased

- A tenant can export its whole ledger graph, however large, under its own read grant (ledger #3649): `Source urn:iki:gonk:export:graph` with `graph=<iri>` streams every quad of that one named graph from one snapshot as N-Quads, a 64 KiB chunk at a time. Before, a tenant's scoped SPARQL read was refused past 100,000 quads or 16 MiB, and the store's dump needs the broad read no tenant may hold. The caller needs the read token for exactly that graph, and the raw-read rule applies at every door as it does to the store's scoped reads. The stream is cut into windows of at most 100,000 quads and 16 MiB, further bounded by the caller's answer grants, and each window must be produced within the caller's time budget. A window past a bound ends the stream with an error, so a failed export is never mistaken for a complete one. At most two exports read at once; a third is refused RetryAfter.
- The Layers page's unread cursors and feed subscriptions follow the person after the people upgrade (ledger #4436). Before people they were keyed by the passkey IRI, so after the upgrade every layer read as unread and the subscriptions were gone. A person now adopts the entries of every passkey that names it: the larger cursor per layer, and the union of the subscriptions. They are written under the person at the next save. The passkeys' entries stay for a downgrade, and what was adopted is recorded in `layers-view/cursors-adopted.json` and `layers-view/subscription-adopted.json`.

## 0.1.3

- Optional arguments now report invalid values that previously selected a default silently. Omitting an argument still selects its default. No stored data changes.
