//! Shared by the integration tests that run a passkey ceremony over real HTTP.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};

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
        self.login_body_at(challenge, origin, "localhost", count)
    }

    /// [`Authenticator::login_body`] for the relying party `rp_id` (ledger #4367).
    #[allow(dead_code)]
    pub fn login_body_at(&self, challenge: &str, origin: &str, rp_id: &str, count: u32) -> String {
        let client = Self::client_data("webauthn.get", challenge, origin);
        let mut auth = Sha256::digest(rp_id.as_bytes()).to_vec();
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

/// What a POST to a passkey door answered over real HTTP: its status, the session token its
/// `Set-Cookie` carries (the ONLY place a sign-in puts the token, ledger #4542), its head, and
/// its body.
#[allow(dead_code)]
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub token: Option<String>,
    pub head: String,
    pub body: String,
}

#[allow(dead_code)]
impl Answer {
    /// The body as JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {self:?}"))
    }

    /// The session token the answer's `Set-Cookie` opened; panics, with the answer, if none.
    pub fn session(&self) -> String {
        self.token
            .clone()
            .unwrap_or_else(|| panic!("no session cookie in the answer: {self:?}"))
    }
}

/// The value of the `gonk_session` cookie a response head's `Set-Cookie` sets, if it sets a
/// non-empty one (a sign-out's clearing `Set-Cookie` sets none).
#[allow(dead_code)]
pub fn session_cookie(head: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.trim().eq_ignore_ascii_case("set-cookie") {
            return None;
        }
        let token = value.trim().strip_prefix("gonk_session=")?;
        let token = token.split(';').next().unwrap_or("").trim();
        (!token.is_empty()).then(|| token.to_string())
    })
}

/// POST `body` to `path` on the local listener at `addr` the way the page does: same-origin
/// JSON from `http://localhost:{port}`.
#[allow(dead_code)]
pub fn post_page(addr: SocketAddr, path: &str, body: &str) -> Answer {
    let host = format!("localhost:{}", addr.port());
    let origin = format!("http://{host}");
    post_page_at(addr, &host, &origin, path, body)
}

/// [`post_page`] with the request's `Host` and `Origin` given, for a listener answering under
/// another name (the public origin, ledger #4367).
#[allow(dead_code)]
pub fn post_page_at(addr: SocketAddr, host: &str, origin: &str, path: &str, body: &str) -> Answer {
    post(addr, host, origin, None, path, body)
}

/// [`post_page`] from a page signed in with `token`, which rides in the cookie as a browser
/// sends it.
#[allow(dead_code)]
pub fn post_page_signed_in(addr: SocketAddr, token: &str, path: &str, body: &str) -> Answer {
    let host = format!("localhost:{}", addr.port());
    let origin = format!("http://{host}");
    post(addr, &host, &origin, Some(token), path, body)
}

fn post(
    addr: SocketAddr,
    host: &str,
    origin: &str,
    token: Option<&str>,
    path: &str,
    body: &str,
) -> Answer {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let cookie = token
        .map(|t| format!("Cookie: gonk_session={t}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\n\
         Content-Type: application/json\r\nOrigin: {origin}\r\n{cookie}\
         Sec-Fetch-Site: same-origin\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(body.as_bytes()).expect("write body");
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let response = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line for POST {path}: {response}"));
    Answer {
        status,
        token: session_cookie(head),
        head: head.to_string(),
        body: body.to_string(),
    }
}
