//! Deterministic fixtures; only fixture construction and authentication are untimed.
#[path = "../tests/common/mod.rs"]
mod common;

use std::alloc::{GlobalAlloc, Layout as AllocLayout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::time::Instant;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ikigai_core::Capability;
use ikigai_gonk::{admit, doors, identity, quic};
use p256::pkcs8::EncodePublicKey;
use serde_json::{json, Map, Value};

// Count requested allocation bytes, not live bytes/RSS. Tracking is enabled only in a
// separate, single-threaded pass, never in the latency samples. realloc counts as one.
thread_local! {
    static ALLOCS: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}
struct Counting;
fn allocated(bytes: usize) {
    ALLOCS.with(|counter| {
        if let Some((calls, total)) = counter.get() {
            counter.set(Some((calls + 1, total + bytes)));
        }
    });
}
// SAFETY: every operation delegates unchanged pointers/layouts to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: AllocLayout) -> *mut u8 {
        allocated(layout.size());
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: AllocLayout) -> *mut u8 {
        allocated(layout.size());
        System.alloc_zeroed(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: AllocLayout) {
        System.dealloc(ptr, layout);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: AllocLayout, size: usize) -> *mut u8 {
        allocated(size);
        System.realloc(ptr, layout, size)
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

const NOW: u64 = 1_800_000_000;
const FINGERPRINT: &str = "0000000000000000000000000000000000000000000000000000000000000000";

struct Fixture {
    _home: tempfile::TempDir,
    layout: quic::Layout,
    passkeys: Arc<identity::Passkeys>,
    token: String,
    door: doors::HttpDoor,
    request: ikigai_web::HttpRequest,
    minted: Capability,
    scopes: Vec<String>,
    clients_bytes: u64,
    grants_bytes: u64,
}

impl Fixture {
    fn new(principals: usize, scope_count: usize) -> Self {
        let home = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(home.path());
        std::fs::create_dir_all(layout.clients_json().parent().unwrap()).unwrap();
        let auth = common::Authenticator::new();
        let id = URL_SAFE_NO_PAD.encode(&auth.id);
        let key = URL_SAFE_NO_PAD.encode(
            p256::ecdsa::VerifyingKey::from(&auth.key)
                .to_public_key_der()
                .unwrap()
                .as_bytes(),
        );
        let scopes: Vec<_> = (0..scope_count)
            .map(|i| format!("urn:cap:ledger:read:ledger{i:04}"))
            .collect();
        let mut clients = Map::new();
        let mut passkeys = Map::new();
        let mut grants = Map::new();
        for i in 0..principals {
            let grant = format!("principal{i:06}");
            clients.insert(format!("{i:064x}"), json!({"grant": grant}));
            passkeys.insert(
                if i == 0 {
                    id.clone()
                } else {
                    format!("credential{i:06}")
                },
                json!({"grant": grant, "label": grant, "public_key": key, "sign_count": 0}),
            );
            grants.insert(grant, json!(scopes));
        }
        std::fs::write(
            layout.clients_json(),
            json!({"clients": clients, "passkeys": passkeys}).to_string(),
        )
        .unwrap();
        std::fs::write(layout.grants_json(), Value::Object(grants).to_string()).unwrap();
        std::fs::write(identity::audit_log(&layout), b"").unwrap();
        let passkeys = Arc::new(identity::Passkeys::new(layout.clone(), 1062));
        let challenge = passkeys.challenge(identity::Purpose::Login, NOW).unwrap();
        let body = auth.login_body(&challenge, passkeys.origin(), 0);
        let (token, who) = passkeys.login(body.as_bytes(), NOW).unwrap();
        assert_eq!(who.scopes, scopes);
        let (_, minted) = quic::current_authority(&layout, FINGERPRINT).unwrap();
        assert_eq!(admit::standing_scopes(&layout, &minted).unwrap(), None);
        let door = doors::HttpDoor {
            anonymous: scopes.iter().step_by(2).cloned().collect(),
            bind: "127.0.0.1:1062".parse().unwrap(),
            passkeys: Some(Arc::clone(&passkeys)),
            anonymous_sparql_budget_ms: 5000,
        };
        let request = ikigai_web::HttpRequest {
            method: "GET".into(),
            path: "/l/default".into(),
            query: vec![],
            body: vec![],
            headers: vec![
                ("host".into(), "localhost:1062".into()),
                ("cookie".into(), format!("gonk_session={token}")),
            ],
            peer: Some("127.0.0.1".parse().unwrap()),
        };
        let resolved = doors::http_scopes(&door, &request, NOW);
        assert_eq!(resolved.len(), scopes.len());
        assert!(scopes.iter().all(|s| resolved.contains(s)));
        let clients_bytes = std::fs::metadata(layout.clients_json()).unwrap().len();
        let grants_bytes = std::fs::metadata(layout.grants_json()).unwrap().len();
        Self {
            _home: home,
            layout,
            passkeys,
            token,
            door,
            request,
            minted,
            scopes,
            clients_bytes,
            grants_bytes,
        }
    }
}

// Snapshot of the union loop in doors::http_scopes, isolated from identity/file reads.
// The fixture checks its output against the live door before timing it.
fn union(anonymous: &[String], granted: Vec<String>) -> Vec<String> {
    let mut scopes = anonymous.to_vec();
    for scope in granted {
        if !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    scopes
}

// A batched thread CPU clock measurement separates CPU work from scheduler delay.
// Other platforms still run the wall-clock/allocation benchmark and print n/a here.
#[cfg(target_os = "linux")]
fn cpu_ns() -> Option<u128> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid, writable timespec for this synchronous clock call.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    assert_eq!(result, 0, "thread CPU clock must be available");
    Some(ts.tv_sec as u128 * 1_000_000_000 + ts.tv_nsec as u128)
}
#[cfg(not(target_os = "linux"))]
fn cpu_ns() -> Option<u128> {
    None
}

fn measure(name: &str, n: usize, samples: usize, operation: &(impl Fn() + Sync)) {
    // Warm both the OS page cache and the allocator before starting any sample.
    for _ in 0..100 {
        operation();
    }
    let barrier = Barrier::new(n);
    let mut times = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let barrier = &barrier;
                scope.spawn(move || {
                    let mut times = Vec::with_capacity(samples);
                    barrier.wait();
                    for _ in 0..samples {
                        let start = Instant::now();
                        operation();
                        times.push(start.elapsed().as_nanos());
                    }
                    times
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    times.sort_unstable();
    // Nearest-rank percentiles of individual calls, never batch averages.
    let percentile = |p: usize| times[(times.len() * p).div_ceil(100) - 1] as f64 / 1000.0;
    let cpu_start = cpu_ns();
    for _ in 0..1000 {
        operation();
    }
    let cpu_us = cpu_start
        .zip(cpu_ns())
        .map(|(start, end)| format!("{:.3}", (end - start) as f64 / 1_000_000.0))
        .unwrap_or_else(|| "n/a".into());
    ALLOCS.with(|c| c.set(Some((0, 0))));
    for _ in 0..100 {
        operation();
    }
    let (allocs, bytes) = ALLOCS.with(|c| c.replace(None).unwrap());
    println!(
        "{name},{n},{},{:.3},{:.3},{cpu_us},{:.2},{:.2}",
        times.len(),
        percentile(50),
        percentile(99),
        allocs as f64 / 100.0,
        bytes as f64 / 100.0
    );
}

fn main() {
    // Cargo passes --bench; accept it. Keep every workload and sample count fixed.
    println!(
        "# authz: optimized bench profile; latency in us; allocations in separate serial pass"
    );
    println!("operation,concurrency,samples,median_us,p99_us,serial_cpu_us_per_call,allocations_per_call,requested_bytes_per_call");
    for (principals, scope_count) in [
        (1, 8),
        (32, 8),
        (128, 8),
        (512, 8),
        (32, 32),
        (32, 128),
        (32, 512),
    ] {
        let f = Fixture::new(principals, scope_count);
        println!("# principals={principals} (each with one QUIC credential and one passkey), scopes_per_grant={scope_count}, clients_bytes={}, grants_bytes={}", f.clients_bytes, f.grants_bytes);
        let enrollment = quic::read_enrolment(&f.layout.clients_json())
            .unwrap()
            .unwrap();
        let grants = quic::read_grants(&f.layout.grants_json()).unwrap();
        let carried = Capability::scoped(f.scopes.iter().step_by(2).cloned());
        assert_eq!(
            union(&f.door.anonymous, f.scopes.clone()),
            doors::http_scopes(&f.door, &f.request, NOW)
        );
        for n in [1, 4, 16] {
            measure("http_identity", n, 2000, &|| {
                black_box(
                    f.passkeys
                        .identity(black_box(&f.token), NOW)
                        .expect("live identity"),
                );
            });
            measure("http_scopes", n, 2000, &|| {
                black_box(doors::http_scopes(&f.door, black_box(&f.request), NOW));
            });
            measure("quic_current_authority", n, 2000, &|| {
                black_box(quic::current_authority(&f.layout, black_box(FINGERPRINT)).unwrap());
            });
            measure("quic_standing_scopes", n, 2000, &|| {
                black_box(admit::standing_scopes(&f.layout, black_box(&f.minted)).unwrap());
            });
        }
        measure("read_files_only", 1, 2000, &|| {
            black_box(std::fs::read(f.layout.clients_json()).unwrap());
            black_box(std::fs::read(f.layout.grants_json()).unwrap());
        });
        measure("quic_authority_in_memory", 1, 10000, &|| {
            black_box(
                quic::authority(black_box(&enrollment), black_box(&grants), FINGERPRINT).unwrap(),
            );
        });
        measure("http_union_in_memory", 1, 10000, &|| {
            black_box(union(
                black_box(&f.door.anonymous),
                black_box(&f.scopes).clone(),
            ));
        });
        measure("clamp_in_memory", 1, 10000, &|| {
            black_box(f.minted.clamp(black_box(&carried)));
        });
    }
}
