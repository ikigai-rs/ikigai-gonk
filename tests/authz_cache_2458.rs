//! File-version changes reach both doors on their very next authority resolution.
mod common;

use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ikigai_gonk::{admit, doors, identity, quic};
use p256::pkcs8::EncodePublicKey;
use serde_json::json;

const NOW: u64 = 1_800_000_000;
const FINGERPRINT: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const READ: &str = "urn:cap:ledger:read:default";

struct Fixture {
    _home: tempfile::TempDir,
    layout: quic::Layout,
    door: doors::HttpDoor,
    request: ikigai_web::HttpRequest,
    minted: ikigai_core::Capability,
}

impl Fixture {
    fn new() -> Self {
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
        std::fs::write(
            layout.clients_json(),
            json!({
                "clients": { FINGERPRINT: {"grant": "person"}},
                "passkeys": { id: {"grant": "person", "public_key": key}}
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(layout.grants_json(), json!({"person": [READ]}).to_string()).unwrap();
        std::fs::write(identity::audit_log(&layout), b"").unwrap();
        let passkeys = Arc::new(identity::Passkeys::new(layout.clone(), 1062));
        let challenge = passkeys.challenge(identity::Purpose::Login, NOW).unwrap();
        let body = auth.login_body(&challenge, passkeys.origin(), 0);
        let (token, _) = passkeys.login(body.as_bytes(), NOW).unwrap();
        let (_, minted) = quic::current_authority(&layout, FINGERPRINT).unwrap();
        let door = doors::HttpDoor {
            anonymous: vec![],
            bind: "127.0.0.1:1062".parse().unwrap(),
            passkeys: Some(passkeys),
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
        let f = Self {
            _home: home,
            layout,
            door,
            request,
            minted,
        };
        assert!(doors::http_scopes(&f.door, &f.request, NOW)
            .iter()
            .any(|s| s == READ));
        assert!(doors::http_cap(f.door.clone())(&f.request).allows(READ));
        assert_eq!(admit::standing_scopes(&f.layout, &f.minted).unwrap(), None);
        f
    }

    fn refused(&self) {
        assert!(!doors::http_cap(self.door.clone())(&self.request).allows(READ));
        assert!(!doors::http_scopes(&self.door, &self.request, NOW)
            .iter()
            .any(|s| s == READ));
        assert!(admit::standing_scopes(&self.layout, &self.minted).is_err());
    }
}

#[test]
fn same_length_rewrite_with_original_mtime_refuses_next_request_on_both_doors() {
    for clients in [false, true] {
        let f = Fixture::new();
        let path = if clients {
            f.layout.clients_json()
        } else {
            f.layout.grants_json()
        };
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let original = std::fs::read_to_string(&path).unwrap();
        let revoked = original.replace("person", "absent");
        assert_eq!(original.len(), revoked.len());
        std::fs::write(&path, revoked).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        f.refused();
    }
}

#[test]
fn atomic_replacement_refuses_next_request_on_both_doors() {
    for clients in [false, true] {
        let f = Fixture::new();
        let path = if clients {
            f.layout.clients_json()
        } else {
            f.layout.grants_json()
        };
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("person", "absent");
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, text).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&temp)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        std::fs::rename(temp, path).unwrap();
        f.refused();
    }
}

#[test]
fn deleted_or_malformed_file_never_uses_stale_authority() {
    for clients in [false, true] {
        for delete in [false, true] {
            let f = Fixture::new();
            let path = if clients {
                f.layout.clients_json()
            } else {
                f.layout.grants_json()
            };
            if delete {
                std::fs::remove_file(path).unwrap();
            } else {
                std::fs::write(path, "not JSON").unwrap();
            }
            f.refused();
        }
    }
}

#[test]
fn http_set_union_preserves_the_old_order_on_benchmark_scope_fixtures() {
    let mut f = Fixture::new();
    for count in [8, 32, 128, 512] {
        let granted: Vec<_> = (0..count)
            .map(|i| format!("urn:cap:ledger:read:ledger{i:04}"))
            .collect();
        std::fs::write(
            f.layout.grants_json(),
            json!({"person": granted}).to_string(),
        )
        .unwrap();
        f.door.anonymous = granted.iter().step_by(2).cloned().collect();
        let mut old = f.door.anonymous.clone();
        for scope in &granted {
            if !old.contains(scope) {
                old.push(scope.clone());
            }
        }
        assert_eq!(doors::http_scopes(&f.door, &f.request, NOW), old);
        // Existing anonymous duplicates also retain their positions.
        f.door.anonymous.push(granted[0].clone());
        let mut old = f.door.anonymous.clone();
        for scope in &granted {
            if !old.contains(scope) {
                old.push(scope.clone());
            }
        }
        assert_eq!(doors::http_scopes(&f.door, &f.request, NOW), old);
    }
}

#[test]
fn cached_configuration_keeps_principals_separate() {
    let f = Fixture::new();
    let other = format!("{:064x}", 1);
    let mut clients: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.layout.clients_json()).unwrap()).unwrap();
    clients["clients"][&other] = json!({"grant": "second"});
    std::fs::write(f.layout.clients_json(), clients.to_string()).unwrap();
    std::fs::write(
        f.layout.grants_json(),
        json!({
            "person": [READ], "second": ["urn:cap:ledger:read:second"]
        })
        .to_string(),
    )
    .unwrap();
    for _ in 0..3 {
        let (name, capability) = quic::current_authority(&f.layout, &other).unwrap();
        assert_eq!(name, "second");
        assert!(capability.allows("urn:cap:ledger:read:second"));
        assert!(!capability.allows(READ));
        let (name, capability) = quic::current_authority(&f.layout, FINGERPRINT).unwrap();
        assert_eq!(name, "person");
        assert!(capability.allows(READ));
        assert!(!capability.allows("urn:cap:ledger:read:second"));
        let scopes = doors::http_scopes(&f.door, &f.request, NOW);
        assert_eq!(scopes, [READ]);
    }
}

#[cfg(unix)]
#[test]
fn unreadable_file_never_uses_stale_authority() {
    use std::os::unix::fs::PermissionsExt;
    for clients in [false, true] {
        let f = Fixture::new();
        let path = if clients {
            f.layout.clients_json()
        } else {
            f.layout.grants_json()
        };
        let permissions = std::fs::metadata(&path).unwrap().permissions();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0)).unwrap();
        // Root can read mode-000 files; assert refusal when the OS enforces it.
        if std::fs::File::open(&path).is_err() {
            f.refused();
        }
        std::fs::set_permissions(path, permissions).unwrap();
    }
}

#[test]
fn restoring_a_deleted_clients_file_does_not_revive_its_sessions() {
    let f = Fixture::new();
    let original = std::fs::read(f.layout.clients_json()).unwrap();
    std::fs::remove_file(f.layout.clients_json()).unwrap();
    f.refused();
    std::fs::write(f.layout.clients_json(), original).unwrap();
    assert!(!doors::http_cap(f.door.clone())(&f.request).allows(READ));
    // QUIC resolves the restored enrollment afresh, with no HTTP session involved.
    assert!(quic::current_authority(&f.layout, FINGERPRINT).is_ok());
}

#[test]
fn restoring_a_deleted_grants_file_keeps_authentication_only() {
    let f = Fixture::new();
    let original = std::fs::read(f.layout.grants_json()).unwrap();
    std::fs::remove_file(f.layout.grants_json()).unwrap();
    f.refused();
    std::fs::write(f.layout.grants_json(), original).unwrap();
    assert!(doors::http_cap(f.door.clone())(&f.request).allows(READ));
    assert!(quic::current_authority(&f.layout, FINGERPRINT).is_ok());
}
