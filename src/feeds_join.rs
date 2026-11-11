//! Joining a host's public feeds from a clean install (ledger
//! [#3291](http://localhost:1062/l/default/item/3291)): a gonk nobody enrolled asks a layer host
//! for `read` and `feed` on its public feeds, and the host answers by policy, with no person
//! deciding (ikigai-layers' join lobby, ledger #3290).
//!
//! ```toml
//! gonk.feeds.door = "https://ikigai-rs.dev"   # the default: a clean gonk needs no config line
//! ```
//!
//! ```text
//! /feed/join   urn:iki:gonk:feeds:join    Sink  join this door's public feeds   urn:cap:gonk:feeds:join
//! /feed/leave  urn:iki:gonk:feeds:leave   Sink  delete the joined identity      urn:cap:gonk:feeds:join
//! ```
//!
//! `ikigai-gonk feeds join` and `feeds leave` do the same from the owner's shell, and a running
//! server notices ([`PublicFeeds::sync`]).
//!
//! # The join
//!
//! 1. **The descriptor**, `https://<host>/.well-known/ikigai/layers.json`, over web PKI: the
//!    door's address, its `server.crt` and that certificate's sha256, the join lobby and the
//!    public layers. It is the one thing a web PKI root vouches for here, so everything else
//!    hangs from it ([`Fetch`], [`WebPki`]).
//! 2. **The pin.** `server.crt` is read beside the descriptor and refused unless its sha256 is
//!    the one the descriptor states; from there on the door is reached by that pin alone.
//! 3. **The certificate**, in `<data home>/feeds/identity/`, made if there is none: `client.crt`,
//!    `client.key` and the pinned `server.crt`, as `ikigai --cert-dir` lays them out. It is kept
//!    across a refused join, so a person retrying is one certificate to the host's limits.
//! 4. **The request**, one `onb:JoinRequest` naming every layer the descriptor lists. The host's
//!    answer is the grant; anything else is its reason, shown as it gave it. The record of the
//!    join (`joined.json`) is written last, so a join that did not finish left no join behind.
//!
//! # What the identity does
//!
//! It reads ([`crate::layers_view::Views`]): with no `gonk.layers.view` it is the view's whole
//! identity, and with one it only adds the public feeds the view identity lacks, which keeps its
//! own grants for everything it holds. **It never says anything**: the door a joined identity
//! reads through refuses every verb but Source ([`crate::layers_view::PublicReader`]), so a Say
//! or an Ask cannot reach it whatever a page does, and "Ask a question about this" is offered
//! only through the view identity's own channels.
//!
//! # Leaving
//!
//! Deletes the identity directory and tells the host nothing: the host lets the identity expire
//! when it stops being read (`join_idle_days`). The per-viewer subscriptions stay, as they are
//! this server's own and name only feeds, so joining again picks them up.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use ikigai_core::{ArgRef, Error, Iri, Request, Verb};
use serde::{Deserialize, Serialize};

use crate::layer::{is_layer_name, Door};
use crate::layers_view::{json, LayersView, PublicReader, Waits};
use crate::mount::{LazyMount, Target};

/// The config key naming the host whose descriptor gonk reads.
pub const DOOR_KEY: &str = "gonk.feeds.door";
/// The host a clean gonk joins.
pub const DEFAULT_DOOR: &str = "https://ikigai-rs.dev";
/// The token a passkey's grant needs to join or leave, the owner's to give.
pub const CAP_JOIN: &str = "urn:cap:gonk:feeds:join";
/// Joining the public feeds.
pub const JOIN_IRI: &str = "urn:iki:gonk:feeds:join";
/// [`JOIN_IRI`]'s route.
pub const JOIN_PATH: &str = "/feed/join";
/// Leaving the public feeds.
pub const LEAVE_IRI: &str = "urn:iki:gonk:feeds:leave";
/// [`LEAVE_IRI`]'s route.
pub const LEAVE_PATH: &str = "/feed/leave";

/// Where a host publishes its descriptor.
const WELL_KNOWN: &str = "/.well-known/ikigai/";
const DESCRIPTOR: &str = "layers.json";
/// The join's record, written last in the identity directory.
const RECORD: &str = "joined.json";
/// The most of a descriptor or a certificate gonk will read: both are small documents.
const MAX_FETCH_BYTES: u64 = 256 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Parse [`DOOR_KEY`]: `https://host[:port]`, and nothing after it.
///
/// ```
/// use ikigai_gonk::feeds_join::parse_door;
/// assert_eq!(parse_door("https://ikigai-rs.dev").unwrap(), "https://ikigai-rs.dev");
/// assert_eq!(parse_door("https://h.example:8443/").unwrap(), "https://h.example:8443");
/// assert!(parse_door("http://ikigai-rs.dev").is_err());
/// assert!(parse_door("https://u@h.example").is_err());
/// assert!(parse_door("https://h.example/x").is_err());
/// assert!(parse_door("https://").is_err());
/// assert!(parse_door("https://.").is_err());
/// ```
///
/// # Errors
///
/// A value that is not an `https` origin: the descriptor is the one thing web PKI vouches for,
/// so plain HTTP is never an option.
pub fn parse_door(value: &str) -> Result<String, String> {
    let refused = |why: &str| format!("{DOOR_KEY} `{value}`: {why}");
    let rest = value
        .strip_prefix("https://")
        .ok_or_else(|| refused("the host is named by `https://host[:port]`"))?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':');
    if !authority.starts_with(|c: char| c.is_ascii_alphanumeric()) || !authority.chars().all(plain)
    {
        return Err(refused(
            "expected only a host and optional port after `https://`",
        ));
    }
    Ok(format!("https://{authority}"))
}

/// The host of an `https://host[:port]` or `quic://host:port` address, lowercased.
pub(crate) fn host_of(address: &str) -> Option<String> {
    let (_, rest) = address.split_once("://")?;
    let host = rest.rsplit_once(':').map_or(rest, |(h, _)| h);
    Some(host.trim_end_matches('/').to_ascii_lowercase())
}

/// How gonk reads the descriptor and its certificate. [`WebPki`] in the binary; a test hands in
/// its own, as no scratch host holds a certificate a public root signed.
pub trait Fetch: Send + Sync {
    /// The body of a `200` answer at `url`.
    ///
    /// # Errors
    ///
    /// Any other answer, or a transport or certificate failure, as a sentence.
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// HTTPS with the Mozilla roots compiled in, nothing else trusted, and no redirects: the host
/// named in the config is the host that answered.
pub struct WebPki;

impl Fetch for WebPki {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .proxy(None)
            .timeout_global(Some(FETCH_TIMEOUT))
            .build()
            .into();
        let mut answer = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
        if answer.status() != 200 {
            return Err(format!("{url} answered {}", answer.status()));
        }
        answer
            .body_mut()
            .with_config()
            .limit(MAX_FETCH_BYTES)
            .read_to_vec()
            .map_err(|e| format!("{url}: {e}"))
    }
}

/// A descriptor, every field checked ([`read_descriptor`]).
#[derive(Debug)]
struct Descriptor {
    /// `quic://host:port`.
    address: String,
    certificate: String,
    sha256: String,
    /// The lobby's `urn:layer:{name}:say`.
    lobby: String,
    layers: Vec<String>,
}

fn read_descriptor(bytes: &[u8]) -> Result<Descriptor, String> {
    let body: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("the descriptor is not JSON: {e}"))?;
    let field = |name: &str| {
        body[name]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("the descriptor names no `{name}`"))
    };
    let address = field("door")?;
    address
        .strip_prefix("quic://")
        .filter(|a| a.contains(':') && !a.contains('/'))
        .ok_or_else(|| format!("the descriptor's door `{address}` is not quic://host:port"))?;
    let certificate = body["server_cert"]["url"]
        .as_str()
        .filter(|u| {
            !u.is_empty()
                && !u.contains("..")
                && u.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        })
        .ok_or("the descriptor's server_cert url is not a file beside the descriptor")?
        .to_string();
    let sha256 = body["server_cert"]["sha256"]
        .as_str()
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or("the descriptor's server_cert sha256 is not 64 hex digits")?
        .to_ascii_lowercase();
    let lobby = field("join")?;
    lobby
        .strip_prefix("urn:layer:")
        .and_then(|l| l.strip_suffix(":say"))
        .filter(|l| is_layer_name(l))
        .ok_or_else(|| format!("the descriptor's join `{lobby}` is not urn:layer:{{name}}:say"))?;
    let mut layers = Vec::new();
    for layer in body["public_layers"].as_array().into_iter().flatten() {
        let name = layer["name"]
            .as_str()
            .filter(|n| is_layer_name(n))
            .ok_or("the descriptor lists a public layer with no valid name")?;
        // A public layer is a read-only feed by the lobby's definition. One described otherwise
        // is not what this button joins, so nothing is asked for at all.
        if layer["type"].as_str() != Some("ik:Feed") || layer["read_only"].as_bool() != Some(true) {
            return Err(format!(
                "the descriptor lists `{name}`, which is not a read-only feed"
            ));
        }
        layers.push(name.to_string());
    }
    if layers.is_empty() {
        return Err("the descriptor lists no public layer".to_string());
    }
    layers.sort();
    layers.dedup();
    Ok(Descriptor {
        address,
        certificate,
        sha256,
        lobby,
        layers,
    })
}

/// A join that finished, as `joined.json` keeps it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Joined {
    /// The `https://` host the descriptor came from.
    pub host: String,
    /// The door, `quic://host:port`.
    pub address: String,
    /// The principal the host named this certificate.
    pub principal: String,
    /// The layers the host granted.
    pub layers: Vec<String>,
}

impl Joined {
    fn target(&self, certs: &Path) -> Result<Target, String> {
        let authority = self
            .address
            .strip_prefix("quic://")
            .ok_or_else(|| format!("{RECORD}: `{}` is not quic://host:port", self.address))?;
        Ok(Target::Quic {
            authority: authority.to_string(),
            certs: certs.to_path_buf(),
        })
    }
}

/// `value` as a quoted Turtle string.
fn turtle_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn request_turtle(pem: &str, fingerprint: &str, layers: &[String]) -> String {
    let layers = layers
        .iter()
        .map(|l| format!("<urn:layer:{l}>"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "@prefix onb: <https://ikigai-rs.dev/ns/onboard#> .\n\
         <urn:iki:onboard:join:{fingerprint}> a onb:JoinRequest ;\n  \
         onb:certificate {} ;\n  onb:fingerprint {} ;\n  onb:layer {layers} .\n",
        turtle_string(pem),
        turtle_string(fingerprint),
    )
}

/// One private file, replaced whole: the rename means a reader sees all of it or none.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let done = (|| {
        std::fs::write(&tmp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)
    })();
    done.map_err(|e| format!("writing {}: {e}", path.display()))
}

fn make_private_dir(dir: &Path) -> Result<(), String> {
    let done = (|| {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok::<(), std::io::Error>(())
    })();
    done.map_err(|e| format!("creating {}: {e}", dir.display()))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// What this process last read from the identity directory.
#[derive(Default)]
struct Attached {
    /// The record's length and time when it was last read; `None` when there was none.
    stamp: Option<(u64, SystemTime)>,
    joined: Option<Joined>,
    /// The joined identity's own view, when there is no `gonk.layers.view`.
    view: Option<Arc<LayersView>>,
}

/// This server's public-feeds identity: joining, leaving, and the reader it gives the view.
pub struct PublicFeeds {
    door: String,
    dir: PathBuf,
    cursors: Option<PathBuf>,
    fetch: Arc<dyn Fetch>,
    own: Option<Arc<LayersView>>,
    waits: Waits,
    /// One join at a time: two would mint one certificate each.
    joining: Mutex<()>,
    attached: Mutex<Attached>,
}

impl PublicFeeds {
    /// The public feeds of `door` (from [`parse_door`]), keeping the identity under
    /// `<data>/feeds/identity/`. `own` is the `gonk.layers.view` view, when there is one;
    /// `cursors` is where a view of the joined identity alone keeps its cursors.
    pub fn open(
        door: String,
        data: &Path,
        fetch: Arc<dyn Fetch>,
        own: Option<Arc<LayersView>>,
        cursors: Option<PathBuf>,
        waits: Waits,
    ) -> Arc<PublicFeeds> {
        let feeds = Arc::new(PublicFeeds {
            door,
            dir: data.join("feeds").join("identity"),
            cursors,
            fetch,
            own,
            waits,
            joining: Mutex::new(()),
            attached: Mutex::new(Attached::default()),
        });
        feeds.sync();
        feeds
    }

    /// The host this server joins.
    pub fn door(&self) -> &str {
        &self.door
    }

    /// Where the identity lives.
    pub fn identity_dir(&self) -> &Path {
        &self.dir
    }

    /// The join this server holds, if any.
    pub fn joined(&self) -> Option<Joined> {
        self.sync();
        lock(&self.attached).joined.clone()
    }

    /// The joined identity's own view (no `gonk.layers.view`), if joined.
    pub(crate) fn joined_view(&self) -> Option<Arc<LayersView>> {
        self.sync();
        lock(&self.attached).view.clone()
    }

    /// The view that shows the feeds right now, the configured one first.
    pub(crate) fn view_for_pages(&self) -> Option<Arc<LayersView>> {
        self.own.clone().or_else(|| self.joined_view())
    }

    fn record_path(&self) -> PathBuf {
        self.dir.join(RECORD)
    }

    fn stamp(&self) -> Option<(u64, SystemTime)> {
        let meta = std::fs::metadata(self.record_path()).ok()?;
        Some((meta.len(), meta.modified().ok()?))
    }

    /// Catch up with the identity directory when it has changed, so a `feeds join` or `feeds
    /// leave` from the owner's shell reaches a running server. One `stat` when nothing did.
    pub fn sync(&self) {
        let stamp = self.stamp();
        let mut attached = lock(&self.attached);
        if attached.stamp != stamp {
            self.attach(&mut attached, stamp);
        }
    }

    fn attach(&self, attached: &mut Attached, stamp: Option<(u64, SystemTime)>) {
        attached.stamp = stamp;
        attached.joined = None;
        self.detach_reader(attached);
        let record = match self.read_record() {
            Ok(Some(record)) => record,
            Ok(None) => return,
            Err(why) => {
                eprintln!("gonk: public feeds: {why}");
                return;
            }
        };
        let target = match record.target(&self.dir) {
            Ok(target) => target,
            Err(why) => {
                eprintln!("gonk: public feeds: {why}");
                return;
            }
        };
        let door = || {
            Arc::new(LazyMount::fresh(target.clone()).with_call_timeout(crate::layer::CALL_TIMEOUT))
                as Arc<dyn Door>
        };
        let reader = PublicReader::new(
            door(),
            door(),
            record.principal.clone(),
            record.layers.iter().cloned().collect(),
        );
        match &self.own {
            Some(own) => own.attach_public(reader),
            None => {
                attached.view = Some(LayersView::public_only(
                    reader,
                    record.address.clone(),
                    self.cursors.clone(),
                    self.waits,
                ));
            }
        }
        attached.joined = Some(record);
    }

    fn detach_reader(&self, attached: &mut Attached) {
        attached.view = None;
        if let Some(own) = &self.own {
            own.detach_public();
        }
    }

    fn read_record(&self) -> Result<Option<Joined>, String> {
        let path = self.record_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Whether the identity to be used exists; made if not. Answers its certificate.
    fn certificate(&self) -> Result<String, String> {
        make_private_dir(&self.dir)?;
        let crt = self.dir.join("client.crt");
        let key = self.dir.join("client.key");
        if let (Ok(pem), true) = (std::fs::read_to_string(&crt), key.is_file()) {
            return Ok(pem);
        }
        let minted = ikigai_quic::generate();
        write_private(&key, minted.key_pem.as_bytes())?;
        write_private(&crt, minted.cert_pem.as_bytes())?;
        Ok(minted.cert_pem)
    }

    /// Fetch the descriptor, check its certificate, and post the join. What the page or the
    /// shell says afterward is the `Err`'s sentence, or the record.
    ///
    /// # Errors
    ///
    /// A descriptor that does not read, a certificate that does not match it, a door that does
    /// not answer, or the host's refusal, with the host's reason.
    pub fn join(&self) -> Result<Joined, String> {
        let _one = lock(&self.joining);
        let base = format!("{}{WELL_KNOWN}", self.door);
        let bytes = self
            .fetch
            .get(&format!("{base}{DESCRIPTOR}"))
            .map_err(|why| format!("Could not read the descriptor: {why}"))?;
        let descriptor = read_descriptor(&bytes)?;
        let pem = self
            .fetch
            .get(&format!("{base}{}", descriptor.certificate))
            .map_err(|why| format!("Could not read the door's certificate: {why}"))?;
        let pem = String::from_utf8(pem)
            .map_err(|_| "the door's certificate is not PEM text".to_string())?;
        let actual = ikigai_quic::fingerprint_of_pem(&pem)
            .map_err(|e| format!("the door's certificate does not read: {e}"))?;
        if !actual.eq_ignore_ascii_case(&descriptor.sha256) {
            return Err(format!(
                "Refused: the door's certificate has sha256 {actual}, and the descriptor at {} \
                 states {}. Nothing was asked of the door.",
                self.door, descriptor.sha256
            ));
        }
        let cert = self.certificate()?;
        write_private(&self.dir.join("server.crt"), pem.as_bytes())?;
        let fingerprint = ikigai_quic::fingerprint_of_pem(&cert)
            .map_err(|e| format!("this server's certificate does not read: {e}"))?;
        let target = Target::Quic {
            authority: descriptor
                .address
                .strip_prefix("quic://")
                .expect("read_descriptor checked the door")
                .to_string(),
            certs: self.dir.clone(),
        };
        // ★ A door of its own, dropped before anything reads: the grant is live on the
        // identity's NEXT connection, and on this one it is still a stranger.
        let door = LazyMount::fresh(target).with_call_timeout(crate::layer::CALL_TIMEOUT);
        let request = Request::new(
            Verb::Sink,
            Iri::parse(&descriptor.lobby).map_err(|e| e.to_string())?,
        )
        .with_arg(
            "content",
            ArgRef::Inline(request_turtle(&cert, &fingerprint, &descriptor.layers).into_bytes()),
        )
        .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
        let answer = door.call(request).map_err(|e| match e {
            Error::Denied(why) => format!("The host refused the join: {why}"),
            other => format!("The host did not grant the join: {other}"),
        })?;
        drop(door);
        let body = json(&answer).map_err(|e| format!("The host's answer: {e}"))?;
        let granted: Vec<&str> = body["granted"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| g.as_str())
            .collect();
        if let Some(missing) = descriptor.layers.iter().find(|layer| {
            !granted.contains(&format!("urn:cap:layer:read:{layer}").as_str())
                || !granted.contains(&format!("urn:cap:layer:feed:{layer}").as_str())
        }) {
            return Err(format!(
                "The host answered without granting read and feed on `{missing}`; nothing was \
                 kept."
            ));
        }
        let principal = body["principal"]
            .as_str()
            .ok_or("The host's answer names no principal")?
            .to_string();
        let joined = Joined {
            host: self.door.clone(),
            address: descriptor.address,
            principal,
            layers: descriptor.layers,
        };
        let record = serde_json::to_vec_pretty(&joined).expect("a plain record");
        write_private(&self.record_path(), &record)?;
        let mut attached = lock(&self.attached);
        let stamp = self.stamp();
        self.attach(&mut attached, stamp);
        Ok(joined)
    }

    /// Delete the joined identity and stop reading with it.
    ///
    /// # Errors
    ///
    /// The directory could not be removed.
    pub fn leave(&self) -> Result<(), String> {
        let _one = lock(&self.joining);
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("removing {}: {e}", self.dir.display())),
        }
        let mut attached = lock(&self.attached);
        self.attach(&mut attached, None);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(change: impl FnOnce(&mut serde_json::Value)) -> Result<Descriptor, String> {
        let mut body = serde_json::json!({
            "door": "quic://h.example:4434",
            "server_cert": { "url": "server.crt", "sha256": "ab".repeat(32) },
            "join": "urn:layer:join:say",
            "public_layers": [
                { "name": "releases", "type": "ik:Feed", "read_only": true, "label": "Releases" },
            ],
        });
        change(&mut body);
        read_descriptor(serde_json::to_string(&body).unwrap().as_bytes())
    }

    #[test]
    fn a_descriptor_reads_when_every_field_is_right() {
        let read = descriptor(|_| {}).unwrap();
        assert_eq!(read.address, "quic://h.example:4434");
        assert_eq!(read.certificate, "server.crt");
        assert_eq!(read.lobby, "urn:layer:join:say");
        assert_eq!(read.layers, ["releases"]);
    }

    #[test]
    fn a_descriptor_cannot_send_the_certificate_read_elsewhere() {
        for url in ["../server.crt", "https://evil.example/x", "/etc/passwd", ""] {
            let err = descriptor(|d| d["server_cert"]["url"] = url.into()).unwrap_err();
            assert!(err.contains("server_cert url"), "{url}: {err}");
        }
    }

    #[test]
    fn a_descriptor_offering_anything_but_a_read_only_feed_is_refused() {
        let writable = descriptor(|d| d["public_layers"][0]["read_only"] = false.into());
        assert!(writable.unwrap_err().contains("not a read-only feed"));
        let channel = descriptor(|d| d["public_layers"][0]["type"] = "ik:Layer".into());
        assert!(channel.unwrap_err().contains("not a read-only feed"));
        let name = descriptor(|d| d["public_layers"][0]["name"] = "../x".into());
        assert!(name.unwrap_err().contains("no valid name"));
        let none = descriptor(|d| d["public_layers"] = serde_json::json!([]));
        assert!(none.unwrap_err().contains("no public layer"));
    }

    #[test]
    fn a_descriptor_with_a_bad_door_lobby_or_digest_is_refused() {
        assert!(descriptor(|d| d["door"] = "https://h.example".into()).is_err());
        assert!(descriptor(|d| d["join"] = "urn:iki:ledger:default:items".into()).is_err());
        assert!(descriptor(|d| d["server_cert"]["sha256"] = "abc".into()).is_err());
        assert!(read_descriptor(b"not json").is_err());
    }

    #[test]
    fn a_pem_survives_the_turtle_literal() {
        let quoted = turtle_string("line \"one\"\nline\\two");
        assert_eq!(quoted, r#""line \"one\"\nline\\two""#);
    }
}
