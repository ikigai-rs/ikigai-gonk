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

impl Authenticator {
    pub fn new() -> Authenticator {
        Authenticator {
            key: SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap(),
            id: b"software-credential-1".to_vec(),
        }
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
            "label": "software",
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
