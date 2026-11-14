# Backup segments and the 10-million-quad drill (#3650)

## Decision

Keep complete snapshots, but serialize and restore them incrementally in bounded segments.
A per-graph-only limit would still strand the dominant browse graph. A delta chain would
need durable change tracking, deletion records, and retention rules tying each delta to a
base. Neither is needed to remove the current limit safely. Each archive remains independent
of every earlier archive and comes from one store iterator's snapshot.

Segments target 64 MiB or 250,000 quads; a single larger quad occupies its own segment, up to
the caller's byte grant. Time, row, and byte grants apply to each segment. The complete backup
has explicit limits of 64 GiB of quad data, one billion quads, and 2 GiB compressed. Backup
publication, scripts pairing, rotation, archive downloads, and restore's staging boundary
retain their existing behavior. A failure publishes no partial archive or restored store.

## Format and verification

A small backup retains the original single gzip member, including identical compressed
bytes. A larger backup consists of concatenated gzip members:

1. `# gonk-segmented/1` followed by a newline.
2. Members containing complete N-Quads lines, in the single iterator's order.
3. `# gonk-end/1 ` followed by one JSON object and a newline. The object records `bytes`,
   `quads`, `sha256`, and `graphs` (graph-name to quad-count).

All framing is an N-Quads comment: standard `gzip -dc` still yields valid N-Quads.
The byte count and SHA-256 cover just the quad lines. The archive SHA-256 and content id
cover every compressed byte. Sidecars distinguish `store_format: "gonk-segmented/1"` from
`"n-quads"`; older sidecars omit the key.

Restore uses one parser for all members, preserving shared blank nodes across segment and
graph boundaries. It validates the completion record, the quad-byte digest, gzip checksums,
and the real EOF before accepting the load. A complete-member truncation still lacks the
required completion record. A separate count query over the restored indexes must match the
record's graph set and per-graph counts, with the existing blank-graph shape comparison.
The loader deduplicates on disk, without a dataset-sized in-memory set.

The private restore store caps RocksDB's open-file cache at 64 and allows compaction during
bulk loading. Atomicity comes from the staging directory, renamed only after verification.
Parsing, loading, and verification run on a separate thread; cancellation drops the staging
guard without publication. The loader targets 64 MB with two threads, in addition to the
compressed input, RocksDB caches, and the current quad. No expanded document is collected.

`restore archive=<name> into=<new-directory>` reads a file in the configured backup directory
and also requires the backup capability. This avoids routing a large archive through the
socket's pipe budget. Paths and simultaneous `archive` and `content` arguments are refused.
Legacy gzip and plain N-Quads restores retain their 1 GiB expanded limit. Older gonk releases
cannot restore a segmented archive beyond that limit.

## Reproduction and drill, 2026-11-14

Scratch data only, under `target/backup-drill`; macOS, rustc 1.99.1, release builds, default
256-descriptor soft limit. Other repository tests were running concurrently; these elapsed
times are evidence of completion, not a latency guarantee.

The existing `backup-memory seed` fixture generated 10,000,000 distinct quads, with varied
literals approximately 227 bytes per serialized quad. Seed completed in 284.94 seconds.
The unmodified backup implementation refused this store after 33.29 seconds:

```text
invalid argument `query`: the backup's dump of every quad: ...
the answer exceeds 1073741824 bytes. It was refused, not truncated ...
Nothing was written.
```

Separately, an ordinary gzip expanding to 1,074,003,968 bytes of valid repeated N-Quads
reproduced the legacy decompressor's 1 GiB refusal. The measurement host's stream-memory
ceiling was raised to 2 GiB for that reproduction so its normal pipe budget would not hide
the decompressor limit.

| Operation | Result |
| --- | --- |
| New backup under exactly `JOB_SCOPES` | 75.65 seconds; 10,000,000 quads |
| Serialized quad bytes | 2,270,000,010 (2.11 GiB) |
| Compressed archive bytes | 282,772,046 |
| Restore by local archive name | 26.69 seconds; 10,000,000 quads; graph verification passed |
| Browse graph | 9,285,714 quads |
| Ledger graph | 714,286 quads |
| Archive SHA-256 | `4ab76bf1c9d5fa6d6ca37b85edf81fa006decbf131353e1715fb0884145bd7cc` |

Reproduce with:

```sh
cargo build --release --example backup-memory
target/release/examples/backup-memory seed target/drill/source 10000000
target/release/examples/backup-memory backup target/drill/source target/drill/backups
target/release/examples/backup-memory restore target/drill/backups/<archive>.nq.gz target/drill/restored
target/release/examples/backup-memory verify target/drill/source target/drill/restored
```

The final command independently reopens both synthetic stores and checks the total and exact
membership of every quad. The synthetic fixture has no blank nodes; shared blank nodes are
covered separately by the segment regression tests.

CI-sized tests force small segment boundaries to cover crossing aggregate answer bounds,
shared blank nodes and blank-node graphs, complete-member truncation, CRC corruption,
trailing members, false byte records, changed graph partitions with valid byte digests,
local-archive authority, traversal refusal, staging cleanup and retry, and merge refusal.
Existing backup integration tests continue to cover the legacy format and byte compatibility.
