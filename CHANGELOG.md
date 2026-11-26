# Changelog

## Unreleased

- The Layers page's unread cursors and feed subscriptions follow the person after the people upgrade (ledger #4436). Before people they were keyed by the passkey IRI, so after the upgrade every layer read as unread and the subscriptions were gone. A person now adopts the entries of every passkey that names it: the larger cursor per layer, and the union of the subscriptions. They are written under the person at the next save. The passkeys' entries stay for a downgrade, and what was adopted is recorded in `layers-view/cursors-adopted.json` and `layers-view/subscription-adopted.json`.

## 0.1.3

- Optional arguments now report invalid values that previously selected a default silently. Omitting an argument still selects its default. No stored data changes.
