# Local directory redirects (gonk ledger #3220)

This is the published `ikigai-web` 0.2.3 source package, copied from crates.io. Its crate
checksum is `665636d8a37e78dea9251749786ec26b7791eb0e45c51d515da8c842cad6d0cf`.
The original source, tests, examples, README, and normalized Cargo manifest are retained;
registry bookkeeping and the dependency crate's lockfile are omitted. The manifest adds
`publish = false`. Nothing is published by this change.

The local patch consists of:

- `src/lib.rs`: an opt-in `EdgeConfig::directory_redirects` list, empty by default.
- `src/door.rs`: after bounded intake and admission, GET and HEAD of an explicitly listed
  path answer 308 to the slash form, preserving the raw query from the request URI. The
  response goes through the existing HEAD and global security/CORS policy. All other
  requests use the original answer path. Endpoint `Location` envelopes remain write-only.

Gonk pins this patched version exactly, so an unlocked resolve cannot replace it with a
newer registry release that lacks the local API. Gonk registers only configured face names. This avoids redirecting unknown faces, changing
the file jail or media types, loosening CSP, or implementing a second HTTP listener. The
published library has no hook for read redirects: its route redirect and endpoint Location
support both implement Post/Redirect/Get (303). The copy is isolated in the first PR commit
so the functional patch can be reviewed in the following commit.

The real-HTTP regression tests are in gonk's `tests/face_3199.rs`. Remove this local patch
when a published ikigai-web release provides the same directory redirect support.
