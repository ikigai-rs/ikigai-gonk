//! An upload through gonk's HTTP door is held for the signed-in caller who delivered it, and
//! that caller's own Sink reads it (ledger #3225, under ikigai-core's scoped content store,
//! ledger #2965).
//!
//! ikigai-web 0.4 spills an upload and holds it with `put_replay_for` under the capability its
//! Sink is issued with. A hold that names nobody is root's alone, so the door spills only for a
//! caller whose capability names a principal; anyone else's body is read into memory under the
//! body limit, as before. gonk's door names a signed-in caller twice over: `http_cap` puts the
//! passkey IRI on the capability, and `edge_config` wires `http_principal` as the edge's
//! `principal_fn`. gonk mounts no upload route of its own today, so this file serves gonk's two
//! hooks with one, over a test kernel, and pins what a route added later would get.
mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ikigai_core::{
    cap_stream_spill, ArgRef, AsyncFnEndpoint, EndpointSpace, Exact, Kernel, MemoryContentStore,
    ReprType, Representation,
};
use ikigai_gonk::{doors, identity, quic};
use p256::pkcs8::EncodePublicKey;
use serde_json::json;

const UPLOAD: &str = "urn:test:upload";

/// What the Sink saw: whether its content came as a held id, who its capability names, and
/// the bytes it read under that capability.
fn echo() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("upload", |inv| {
        Box::pin(async move {
            let held = matches!(inv.request.args.get("content"), Some(ArgRef::Content(_)));
            let body = inv.content_body("content").await?;
            let bytes = inv.collect(body).await?;
            let answer = format!(
                "{}|{}|{}",
                if held { "held" } else { "inline" },
                inv.capability.principal().unwrap_or("nobody"),
                String::from_utf8_lossy(&bytes),
            );
            Ok(Representation::new(
                ReprType::new("text/plain"),
                answer.into_bytes(),
            ))
        })
    })
}

struct Server {
    port: u16,
    token: String,
    principal: String,
    _home: tempfile::TempDir,
}

impl Server {
    fn start() -> Server {
        let home = tempfile::tempdir().unwrap();
        let spills = home.path().join("spill");
        std::fs::create_dir(&spills).unwrap();
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
            json!({"passkeys": { &id: {"grant": "person", "public_key": key}}}).to_string(),
        )
        .unwrap();
        // Both callers hold a disk grant, so what decides whether an upload spills is only
        // whether the door names who delivered it.
        let spill = cap_stream_spill(1 << 20);
        std::fs::write(
            layout.grants_json(),
            json!({"person": [&spill]}).to_string(),
        )
        .unwrap();
        std::fs::write(identity::audit_log(&layout), b"").unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let passkeys = Arc::new(identity::Passkeys::new(layout.clone(), addr.port()));
        let now = identity::now_seconds();
        let challenge = passkeys.challenge(identity::Purpose::Login, now).unwrap();
        let body = auth.login_body(&challenge, passkeys.origin(), 0);
        let (token, who) = passkeys.login(body.as_bytes(), now).unwrap();
        let principal = identity::passkey_iri(&who.enrolled.credential_id);

        let door = doors::HttpDoor {
            anonymous: vec![spill],
            bind: addr,
            passkeys: Some(passkeys),
            public: Vec::new(),
            anonymous_sparql_budget_ms: 5000,
        };
        let store = Arc::new(MemoryContentStore::new(1 << 20));
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new(UPLOAD), echo()),
        ))
        .with_content_store(store.clone())
        .with_spill(Arc::new(ikigai_fs::FileSpill::new(&spills).unwrap()));
        let mut config = doors::edge_config(door.clone());
        config.routes.routes.insert(
            0,
            ikigai_web::Route {
                pattern: "/upload".to_string(),
                iri_template: UPLOAD.to_string(),
                redirect: None,
                cap: None,
                cors: None,
                csp: None,
            },
        );
        let uploads = ikigai_web::Uploads::new(store).route("/upload", 1 << 20);
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_uploads(
                Arc::new(kernel),
                doors::http_cap(door),
                listener,
                config,
                uploads,
            ))
        });
        Server {
            port: addr.port(),
            token,
            principal,
            _home: home,
        }
    }

    /// POST `body` to the upload route, signed in or not; the status and the answer's body.
    fn upload(&self, signed_in: bool, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        let mut head = format!(
            "POST /upload HTTP/1.1\r\nHost: localhost:{port}\r\n\
             Origin: http://localhost:{port}\r\nSec-Fetch-Site: same-origin\r\n\
             Accept: text/plain\r\nContent-Type: application/octet-stream\r\n",
            port = self.port
        );
        if signed_in {
            head.push_str(&format!(
                "Cookie: {}={}\r\n",
                identity::SESSION_COOKIE,
                self.token
            ));
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let response = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        (status, body.to_string())
    }
}

#[test]
fn a_signed_in_uploader_s_own_sink_reads_the_upload_held_for_it() {
    let server = Server::start();
    let (status, body) = server.upload(true, "the uploader's bytes");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        format!("held|{}|the uploader's bytes", server.principal),
        "the upload was spilled, held for the passkey that delivered it, and read back by \
         that caller's own Sink"
    );
}

#[test]
fn an_anonymous_upload_is_not_spilled_where_its_sink_could_not_read_it() {
    let server = Server::start();
    let (status, body) = server.upload(false, "anonymous bytes");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body, "inline|nobody|anonymous bytes",
        "a caller the door names nobody is read into memory, not held where only root reads"
    );
}
