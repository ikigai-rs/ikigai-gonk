//! Shared by the integration tests that run a passkey ceremony over real HTTP.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use p256::pkcs8::EncodePublicKey;
use sha2::{Digest, Sha256};

/// A software authenticator: a P-256 key, and the two ceremonies' byte layouts.
pub struct Authenticator {
    pub key: SigningKey,
    pub id: Vec<u8>,
}

// ⚠ Each test binary compiles this module WHOLE and uses part of it, so a constructor no
// single binary calls is normal here rather than dead: `tests/web.rs` signs with `new`,
// `tests/browse.rs` enrolls several identities and needs `named`.
#[allow(dead_code)]
impl Authenticator {
    pub fn new() -> Authenticator {
        Authenticator {
            key: SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap(),
            id: b"software-credential-1".to_vec(),
        }
    }

    /// A DIFFERENT authenticator, deterministically derived from `name`.
    ///
    /// ⚠ Needed the moment one test enrolls twice: a credential id is unique per server, so
    /// a second [`Authenticator::new`] is refused with "this credential is already
    /// enrolled" — which reads like a test-harness bug and is the server being right.
    pub fn named(name: &str) -> Authenticator {
        let seed = Sha256::digest(name.as_bytes());
        Authenticator {
            key: SigningKey::from_bytes(&seed).expect("a valid P-256 scalar"),
            id: format!("software-credential-{name}").into_bytes(),
        }
    }

    /// The label [`Authenticator::register_body`] enrolls it under: `software` for
    /// [`Authenticator::new`], `software-<name>` for [`Authenticator::named`]. Each its own,
    /// because enrollment refuses a label another passkey already has (ledger #3384).
    pub fn label(&self) -> String {
        let id = String::from_utf8_lossy(&self.id);
        match id.strip_prefix("software-credential-") {
            Some("1") | None => "software".to_string(),
            Some(name) => format!("software-{name}"),
        }
    }

    /// [`Authenticator::register_body`] under the label `label`.
    pub fn register_body_labeled(
        &self,
        challenge: &str,
        invite: &str,
        origin: &str,
        label: &str,
    ) -> String {
        let mut body: serde_json::Value =
            serde_json::from_str(&self.register_body(challenge, invite, origin)).unwrap();
        body["label"] = label.into();
        body.to_string()
    }

    fn client_data(kind: &str, challenge: &str, origin: &str) -> Vec<u8> {
        format!(r#"{{"type":"{kind}","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#)
            .into_bytes()
    }

    pub fn register_body(&self, challenge: &str, invite: &str, origin: &str) -> String {
        let spki = p256::ecdsa::VerifyingKey::from(&self.key)
            .to_public_key_der()
            .unwrap();
        serde_json::json!({
            "challenge": challenge,
            "invite": invite,
            "label": self.label(),
            "id": URL_SAFE_NO_PAD.encode(&self.id),
            "publicKey": URL_SAFE_NO_PAD.encode(spki.as_bytes()),
            "clientDataJSON": URL_SAFE_NO_PAD.encode(Self::client_data("webauthn.create", challenge, origin)),
        })
        .to_string()
    }

    pub fn login_body(&self, challenge: &str, origin: &str, count: u32) -> String {
        let client = Self::client_data("webauthn.get", challenge, origin);
        let mut auth = Sha256::digest(b"localhost").to_vec();
        auth.push(0x05); // user present + user verified
        auth.extend_from_slice(&count.to_be_bytes());
        let mut signed = auth.clone();
        signed.extend_from_slice(&Sha256::digest(&client));
        let signature: Signature = self.key.sign(&signed);
        serde_json::json!({
            "challenge": challenge,
            "id": URL_SAFE_NO_PAD.encode(&self.id),
            "authenticatorData": URL_SAFE_NO_PAD.encode(&auth),
            "clientDataJSON": URL_SAFE_NO_PAD.encode(&client),
            "signature": URL_SAFE_NO_PAD.encode(signature.to_der().as_bytes()),
        })
        .to_string()
    }
}
