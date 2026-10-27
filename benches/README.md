# Per-request authority resolution

Run `cargo bench --bench authz > authz.csv` from the repository root. No gonk process,
network, real configuration home, store, or operator credentials are used. `cargo bench --no-run`
is the optimized compilation check; CI also checks the benchmark through
`clippy --all-targets`. The harness uses only public gonk/core APIs and existing test
software-authenticator support; it does not depend on core's internal representation.

The seven deterministic fixtures have 1, 32, 128, and 512 principals at eight scopes
per grant, then 32 principals at 32, 128, and 512 scopes per grant. Each principal
has one QUIC credential, one passkey, and one named grant. Scopes are distinct
per-ledger read tokens. HTTP's anonymous grant overlaps half the signed-in grant.
The same files serve both doors. Their byte sizes appear in the output. These
are enrollment/large-grant scenarios, not a claim about any operator's actual files.

A temporary configuration home holds generated `clients.json` and `grants.json`.
An empty, unchanged audit log exercises the normal metadata check.
An actual signed P-256 login establishes the HTTP session before measurement;
challenge/session randomness never changes the fixture files or timed workload.
Every call uses a fixed logical time and the same live credential. The QUIC
connection capability is minted before measurement. Preconditions assert the
expected scopes and unchanged connection authority, so refused/anonymous paths
cannot accidentally stand in for successful resolution.

Each operation warms up 100 times before recording 2,000 individual calls per
worker (10,000 for isolated in-memory operations). At N=1, 4, and 16, persistent
threads start at a barrier and run closed-loop against the same files and, for
HTTP, the same session table. Threads and sample vectors are created outside
the timers. Median and p99 are nearest-rank percentiles across individual call
latencies, including scheduling and operation result destruction. N is the
maximum active worker count, not a promise that every instant has N calls active.
This measures service time under closed-loop contention, not queueing latency
under an externally imposed request arrival rate.

The CSV reports these operations:

| Operation | Included work |
| --- | --- |
| `http_identity` | Real `Passkeys::identity`: token hash, session mutex/audit metadata check, both file reads/parses, enrollment and validated grant lookup, idle-clock update |
| `http_scopes` | Real HTTP scope resolver: identity plus anonymous/signed-in grant union and door checks, using an in-memory same-origin request |
| `quic_current_authority` | Real per-call credential/grant resolution and capability/principal construction |
| `quic_standing_scopes` | Real QUIC admission recheck, including authority resolution, attenuation and comparison against the connection capability |
| `read_files_only` | Both raw file reads and buffer destruction, without JSON parsing; a diagnostic baseline, not a subtractable estimate of parser time |
| `quic_authority_in_memory` | Real `quic::authority` over pre-parsed enrollment/grants: lookup, validation, capability/principal construction |
| `http_union_in_memory` | Snapshot of `http_scopes`' Vec/contains union loop, including cloning an owned input grant; asserted equal to the live resolver's output before timing |
| `clamp_in_memory` | Real scoped `Capability::clamp` with a carried grant containing half the ceiling's scopes; preserves the ceiling principal |

There is no handler or request I/O in these samples; configuration file I/O is
intentionally included in the live resolvers. HTTP identity returns a scope list,
not a clamped capability; QUIC admission attenuates its connection capability.
The isolated clamp row measures the separate core operation rather than
attributing a clamp to functions that do not perform one. `http_cap` currently
calls identity for scopes and again for principal; these per-resolution numbers
are not a claim that an entire HTTP request resolves identity only once.

Linux additionally reports average thread CPU time per call over a separate
1,000-call serial batch (`CLOCK_THREAD_CPUTIME_ID`); other platforms print `n/a`.
A separate 100-call serial batch counts allocation/reallocation calls and total
requested bytes, including zeroed allocations and the new size of reallocations.
These are not retained bytes or RSS. The union diagnostic includes an input grant
clone to supply owned scopes, which identity already supplies in the live HTTP
resolver; diagnostic rows must not be added together as a request total. Allocation tracking is disabled during all
latency and CPU samples; its inactive thread-local check remains in the benchmark
allocator. All CPU/allocation columns are serial diagnostics, including rows
whose latency samples use multiple workers.

Run on an otherwise idle machine after builds finish. Record CPU, OS/kernel,
Rust version, build profile, file sizes, and sample counts with results. Repeat
the complete run to check tail stability. No machine-specific timing assertion
is a CI test: CI checks formatting, linting, tests, and benchmark compilation.
Fixture assertions execute when the benchmark is run. Timing noise and a finite
fixture matrix limit any extrapolation to larger enrollments or other hardware.
