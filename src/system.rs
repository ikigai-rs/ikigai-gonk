//! What the operator reads about this server: its disks, its store, itself (ledger
//! [#2972](http://localhost:1062/l/default/item/2972)). The Dashboard's System tab draws these
//! ([`crate::operator`]); each answers JSON to anyone holding [`CAP_READ`], at every door.
//!
//! ```text
//! urn:iki:gonk:system:disk     Source  free and total for the store's and the backups' volumes
//! urn:iki:gonk:system:store    Source  quads, size on disk, the last backup and its age
//! urn:iki:gonk:system:gonk     Source  version, commit, build time, uptime, doors, linked crates
//! ```
//!
//! `urn:iki:gonk:system:workers` is the fourth row. It reads ledgers, so it lives where the ledgers'
//! names do: [`crate::operator::Workers`].
//!
//! ★ **[`CAP_READ`] is a real grant, and an operator's alone.** It says where the store and the
//! backups are on disk and how large the store is. A graph's name is information too (a private
//! ledger's names a project), so [`STORE`] names only the graphs its caller may read and counts the
//! rest ([`readable_graphs`], ledger #3039). An anonymous caller never holds it (`grants::anonymous_http_grants` mints
//! ledger tokens only), and a page another site navigated to loses it
//! (`admit::FOREIGN_PAGE_KEEPS` is an allowlist).
//!
//! ★ **This module reads the store and the backup status as ITSELF, and only those two.** Their
//! tokens (`urn:cap:store:read`, `urn:cap:gonk:backup`) read or export every graph, so no grant may
//! hold them and a viewer's capability could never reach them. So [`STORE`] answers from them under
//! [`store_scopes`], after the kernel has checked [`CAP_READ`], and returns counts and times, never
//! a quad. `urn:iki:store:info` carries one total, not a count per graph, so the per-graph counts are
//! the ones the newest backup recorded, labeled as of that backup: a live count per graph is a scan
//! of the dataset, on every poll of an open tab.
//!
//! ⚠ **The hub's readouts are the ones that count.** A door kernel caches nothing, so
//! `urn:kernel:cache` at a door is empty; see [`crate::operator`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    Capability, Description, Endpoint, EndpointSpace, Error, Exact, Expiry, Invocation, Iri,
    Kernel, ReprType, Representation, Request, Result, Verb,
};
use ikigai_ledger::Ledger;
use serde_json::{json, Value};

/// What every System resource requires, and the System tab with them.
pub const CAP_READ: &str = "urn:cap:gonk:system:read";

/// Free and total for the volumes this server writes.
pub const DISK: &str = "urn:iki:gonk:system:disk";
/// The store's size and the last backup.
pub const STORE: &str = "urn:iki:gonk:system:store";
/// This binary and this process.
pub const GONK: &str = "urn:iki:gonk:system:gonk";

const JSON: &str = "application/json";

/// The free space under which a volume needs attention: 30 GiB, the workers' own `[disk] floor_gb`
/// default, so the tab and a worker holding back agree. A constant for the reason
/// `dashboard::POLL_EVERY` is one: nothing has asked to tune it.
pub const DISK_FLOOR_BYTES: u64 = 30 * (1 << 30);

/// How far past its cadence a backup may be before it needs attention: the fire is phased to the
/// newest archive and then takes its own time, so the moment a cadence ends is not yet late.
const BACKUP_GRACE: Duration = Duration::from_secs(15 * 60);

/// A door this process has open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Door {
    /// `socket`, `QUIC` or `HTTP`.
    pub name: &'static str,
    /// Where it listens.
    pub at: String,
}

/// What the family is built from. `main` builds one before the hub, hands [`System::space`] to it,
/// then [attaches](System::attach) the hub and records each [door](System::listening) as it binds.
pub struct System {
    store_path: PathBuf,
    backup_dir: PathBuf,
    backup_every: Option<Duration>,
    started: Instant,
    doors: Mutex<Vec<Door>>,
    hub: OnceLock<Weak<Kernel>>,
}

impl System {
    /// A family for the store at `store_path` and the backups in `backup_dir`, every
    /// `backup_every` (`None` when none is scheduled). Uptime counts from here.
    pub fn new(
        store_path: PathBuf,
        backup_dir: PathBuf,
        backup_every: Option<Duration>,
    ) -> Arc<System> {
        Arc::new(System {
            store_path,
            backup_dir,
            backup_every,
            started: Instant::now(),
            doors: Mutex::new(Vec::new()),
            hub: OnceLock::new(),
        })
    }

    /// Hand the family the hub it is bound in. Called once, after the hub is built; a second call
    /// is a no-op, and until the first, [`STORE`] says it cannot read yet.
    pub fn attach(&self, hub: &Arc<Kernel>) {
        let _ = self.hub.set(Arc::downgrade(hub));
    }

    /// Record a door once it is bound, so this never names one that failed to open.
    pub fn listening(&self, name: &'static str, at: impl Into<String>) {
        self.doors.lock().expect("the doors lock").push(Door {
            name,
            at: at.into(),
        });
    }

    /// The three resources.
    pub fn space(self: &Arc<Self>) -> EndpointSpace {
        EndpointSpace::new()
            .bind(Exact::new(DISK), DiskEndpoint(Arc::clone(self)))
            .bind(Exact::new(STORE), StoreEndpoint(Arc::clone(self)))
            .bind(Exact::new(GONK), GonkEndpoint(Arc::clone(self)))
    }

    fn doors(&self) -> Vec<Door> {
        self.doors.lock().expect("the doors lock").clone()
    }

    /// `(what, how)` for a read of `target` under `scopes`.
    async fn read_as(&self, target: &str, scopes: &[&str], face: Option<&str>) -> Result<Vec<u8>> {
        let hub = self
            .hub
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| Error::Unavailable("the hub is not attached yet".to_string()))?;
        let iri = Iri::parse(target).map_err(|e| Error::Endpoint(e.to_string()))?;
        let mut read = Request::new(Verb::Source, iri);
        if let Some(face) = face {
            read = crate::web::with(read, "as", face);
        }
        let capability = Capability::scoped(scopes.iter().copied());
        Ok(hub.issue(read, &capability).await?.bytes)
    }
}

/// The internal scopes [`STORE`] reads under: the store's total and the backup status, nothing else.
pub fn store_scopes() -> [&'static str; 2] {
    [ikigai_store::CAP_READ, crate::backup::CAP_BACKUP]
}

/// The tab's gate, in the one place the page and the resources share: this resource is enforced
/// here as well as declared, so an embedding that reached the endpoint directly gets the same
/// answer as a door.
pub(crate) fn require(capability: &Capability) -> Result<()> {
    if capability.allows(CAP_READ) {
        Ok(())
    } else {
        Err(Error::Denied(format!(
            "reading this server's health needs {CAP_READ} in the grant"
        )))
    }
}

/// Whether `capability` may read the graph called `graph`, as a key of a backup's count table.
///
/// A ledger's graph answers [`crate::web::require_read`], the test every ledger page and
/// `urn:iki:gonk:agent` use. Any other graph answers its own store token alone, and so does a
/// graveyard on top of its ledger's test: it is a read tier of its own (`audit`), and how much a
/// ledger has deleted is what this row would say about it.
fn may_read_graph(capability: &Capability, graph: &str) -> bool {
    let named = graph.strip_prefix(crate::grants::LEDGER_GRAPH_PREFIX);
    let ledger = named
        .map(|name| name.strip_suffix(":deleted").unwrap_or(name))
        .and_then(|name| Ledger::parse(name).ok());
    capability.allows(&ikigai_store::cap_read_graph(graph))
        && ledger.is_none_or(|ledger| crate::web::require_read(capability, &ledger).is_ok())
}

/// Of a backup's `graphs` table, the rows `capability` may read and how many it may not. Nothing
/// of a hidden row leaves here but that it was one.
pub fn readable_graphs(graphs: &Value, capability: &Capability) -> (Value, usize) {
    let Some(table) = graphs.as_object() else {
        return (Value::Null, 0);
    };
    let readable: serde_json::Map<String, Value> = table
        .iter()
        .filter(|(graph, _)| may_read_graph(capability, graph))
        .map(|(graph, quads)| (graph.clone(), quads.clone()))
        .collect();
    let hidden = table.len() - readable.len();
    (Value::Object(readable), hidden)
}

/// A live answer: never cached, since time alone moves every number in it.
fn live(value: Value) -> Representation {
    Representation::new(
        ReprType::new(JSON).with_param("charset", "utf-8"),
        value.to_string().into_bytes(),
    )
    .with_expiry(Expiry::Always)
}

fn described(name: &str, title: &str, summary: &str) -> Description {
    Description::new(name)
        .title(title)
        .summary(summary)
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_READ)
        .output(JSON)
}

/// `(free to this process, total)` in bytes of the volume holding `path`, or of the nearest
/// ancestor that exists: the backup directory is not made until the first backup.
#[cfg(unix)]
fn volume(path: &Path) -> std::io::Result<(u64, u64)> {
    let existing = path
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .unwrap_or(path);
    let stats = rustix::fs::statvfs(existing)?;
    Ok((
        stats.f_bavail.saturating_mul(stats.f_frsize),
        stats.f_blocks.saturating_mul(stats.f_frsize),
    ))
}

#[cfg(not(unix))]
fn volume(_path: &Path) -> std::io::Result<(u64, u64)> {
    Err(std::io::Error::other("this platform has no statvfs"))
}

/// One volume's row: what [`DISK`] answers and the tab draws.
pub fn volume_row(name: &str, path: &Path, read: std::io::Result<(u64, u64)>, floor: u64) -> Value {
    let path = path.display().to_string();
    match read {
        Ok((free, total)) => json!({
            "name": name,
            "path": path,
            "free_bytes": free,
            "total_bytes": total,
            "floor_bytes": floor,
            "attention": free < floor,
        }),
        Err(error) => json!({
            "name": name,
            "path": path,
            "error": error.to_string(),
            "floor_bytes": floor,
            "attention": true,
        }),
    }
}

struct DiskEndpoint(Arc<System>);

#[async_trait]
impl Endpoint for DiskEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        require(inv.capability)?;
        let row = |name, path: &Path| volume_row(name, path, volume(path), DISK_FLOOR_BYTES);
        Ok(live(json!({
            "volumes": [
                row("Store", &self.0.store_path),
                row("Backups", &self.0.backup_dir),
            ],
        })))
    }

    fn name(&self) -> &str {
        "gonk-system-disk"
    }

    fn describe(&self) -> Description {
        described(
            self.name(),
            "Free and total space on this server's volumes",
            "For the volume holding the store and the volume holding the backups: their paths, \
             the bytes free to this process, the total, the floor under which a volume needs \
             attention, and whether it does.",
        )
    }
}

/// The bytes of the regular files under `path`. Symbolic links are not followed, and a file that
/// goes while this walks (RocksDB compacts) is simply not counted.
fn size_on_disk(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => size_on_disk(&entry.path()),
            Ok(meta) if meta.is_file() => meta.len(),
            _ => 0,
        })
        .sum()
}

/// Whether the newest backup needs attention, given when it was taken and the cadence: there is
/// none, or the schedule should have replaced it. Time since a backup with nothing scheduled is
/// a fact, not a fault.
pub fn backup_late(age: Option<Duration>, every: Option<Duration>) -> bool {
    match (age, every) {
        (None, _) => true,
        (Some(age), Some(every)) => age > every + BACKUP_GRACE,
        (Some(_), None) => false,
    }
}

/// `backing: …`/`quads: …` lines, as `urn:iki:store:info` writes them.
fn info_line<'a>(info: &'a str, key: &str) -> Option<&'a str> {
    info.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(": "))
}

struct StoreEndpoint(Arc<System>);

impl StoreEndpoint {
    async fn answer(&self, capability: &Capability, now_ms: u64) -> Value {
        let system = &self.0;
        let info = system
            .read_as("urn:iki:store:info", &store_scopes(), None)
            .await
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        let status: Option<Value> = system
            .read_as(crate::backup::STATUS, &store_scopes(), Some(JSON))
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let newest = status.as_ref().map(|s| &s["newest"]);
        let taken = newest.and_then(|n| n["taken_at_ms"].as_u64());
        let age = taken.map(|at| Duration::from_millis(now_ms.saturating_sub(at)));
        let failures = status
            .as_ref()
            .and_then(|s| s["timer"]["failures_in_a_row"].as_u64())
            .unwrap_or(0);
        let (quads, backing, unread) = match &info {
            Ok(text) => (
                info_line(text, "quads").and_then(|n| n.parse::<u64>().ok()),
                info_line(text, "backing").map(str::to_string),
                None,
            ),
            // Said by class: what an error carries is written for a log.
            Err(error) => (None, None, Some(error.class().to_string())),
        };
        let (graphs, hidden) = newest.map_or((Value::Null, 0), |n| {
            readable_graphs(&n["graphs"], capability)
        });
        json!({
            "quads": quads,
            "backing": backing,
            "unread": unread,
            "size_bytes": size_on_disk(&system.store_path),
            "graphs": graphs,
            "graphs_hidden": hidden,
            "backup": {
                "family_bound": status.is_some(),
                "taken_at_ms": taken,
                "age_seconds": age.map(|a| a.as_secs()),
                "every_seconds": system.backup_every.map(|e| e.as_secs()),
                "failures_in_a_row": failures,
                "attention": backup_late(age, system.backup_every) || failures > 0,
            },
        })
    }
}

#[async_trait]
impl Endpoint for StoreEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        require(inv.capability)?;
        let now_ms = crate::agents::now_ms(inv)?;
        Ok(live(self.answer(inv.capability, now_ms).await))
    }

    fn name(&self) -> &str {
        "gonk-system-store"
    }

    fn describe(&self) -> Description {
        described(
            self.name(),
            "How large the store is, and how old its last backup",
            "The dataset's total quads and backing from urn:iki:store:info, the store's bytes on \
             disk, the quads per graph the newest backup recorded for each graph this \
             capability may read and how many more it may not, and that backup's time and age \
             with whether it is older than its schedule allows. Counts and times, never a quad.",
        )
    }
}

struct GonkEndpoint(Arc<System>);

#[async_trait]
impl Endpoint for GonkEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        require(inv.capability)?;
        let doors: Vec<Value> = self
            .0
            .doors()
            .into_iter()
            .map(|door| json!({ "name": door.name, "at": door.at }))
            .collect();
        Ok(live(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "commit": env!("GONK_BUILD_COMMIT"),
            "built_at_seconds": env!("GONK_BUILT_AT_SECONDS").parse::<u64>().unwrap_or(0),
            "uptime_seconds": self.0.started.elapsed().as_secs(),
            "doors": doors,
            "ledger_version": env!("GONK_LINKS_LEDGER"),
            "browse_version": env!("GONK_LINKS_BROWSE"),
        })))
    }

    fn name(&self) -> &str {
        "gonk-system-gonk"
    }

    fn describe(&self) -> Description {
        described(
            self.name(),
            "This binary and this process",
            "gonk's version, the commit it was built from and when, how long this process has \
             been up, the doors it has open, and the ledger and browse crate versions it links.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn a_volume_under_the_floor_needs_attention_and_says_its_numbers() {
        let path = Path::new("/sample");
        let row = volume_row("Store", path, Ok((9, 100)), 10);
        assert_eq!(row["attention"], true);
        assert_eq!(row["free_bytes"], 9);
        assert_eq!(row["total_bytes"], 100);
        assert_eq!(
            volume_row("Store", path, Ok((10, 100)), 10)["attention"],
            false
        );
        // Not knowing is not being fine.
        let unread = volume_row("Store", path, Err(std::io::Error::other("no")), 10);
        assert_eq!(unread["attention"], true);
        assert_eq!(unread["error"], "no");
    }

    #[test]
    fn a_backup_is_late_past_its_schedule_and_grace() {
        let day = 24 * HOUR;
        assert!(backup_late(None, Some(day)), "none at all");
        assert!(backup_late(None, None), "none at all, none scheduled");
        assert!(!backup_late(Some(day), Some(day)));
        assert!(!backup_late(Some(day + BACKUP_GRACE), Some(day)));
        assert!(backup_late(Some(day + BACKUP_GRACE + HOUR), Some(day)));
        assert!(!backup_late(Some(30 * day), None), "nothing promised");
    }

    /// Ledger #3039: which rows a grant sees, against the ledger's own graph names.
    #[test]
    fn a_grant_sees_only_the_graphs_it_may_read() {
        let ledger = Ledger::parse("sample").unwrap();
        let other = Ledger::parse("sample-other").unwrap();
        let graphs = json!({
            ledger.graph(): 1,
            ledger.deleted_graph(): 2,
            other.graph(): 3,
            other.deleted_graph(): 4,
            "urn:iki:browse:graph:sample": 5,
            "urn:iki:ledger:graph:Not-A-Ledger": 6,
            "": 7,
        });
        let names = |capability: &Capability| {
            let (table, hidden) = readable_graphs(&graphs, capability);
            let names: Vec<String> = table.as_object().unwrap().keys().cloned().collect();
            (names, hidden)
        };
        let graph_token = |graph: &str| ikigai_store::cap_read_graph(graph);

        let nothing = Capability::scoped([CAP_READ]);
        assert_eq!(names(&nothing), (vec![], 7));

        // A ledger takes both of its tokens, and neither alone is enough.
        let reader = Capability::scoped([ledger.cap_read(), graph_token(&ledger.graph())]);
        assert_eq!(names(&reader), (vec![ledger.graph()], 6));
        assert_eq!(names(&Capability::scoped([ledger.cap_read()])), (vec![], 7));
        assert_eq!(
            names(&Capability::scoped([graph_token(&ledger.graph())])),
            (vec![], 7)
        );

        // A graveyard is its own read, on top of its ledger's.
        let auditor = Capability::scoped([
            ledger.cap_read(),
            graph_token(&ledger.graph()),
            graph_token(&ledger.deleted_graph()),
        ]);
        let (seen, hidden) = names(&auditor);
        assert!(seen.contains(&ledger.deleted_graph()), "{seen:?}");
        assert_eq!(hidden, 5);
        let graveyard_alone = Capability::scoped([graph_token(&ledger.deleted_graph())]);
        assert_eq!(names(&graveyard_alone), (vec![], 7));

        // A graph that is no ledger's answers its own token.
        let browser = Capability::scoped([graph_token("urn:iki:browse:graph:sample")]);
        assert_eq!(
            names(&browser),
            (vec!["urn:iki:browse:graph:sample".to_string()], 6)
        );

        assert_eq!(names(&Capability::root()).1, 0);
        // No backup is no table, not a table of nothing.
        assert_eq!(readable_graphs(&Value::Null, &nothing), (Value::Null, 0));
    }

    #[test]
    fn the_store_lines_are_read_by_name() {
        let info = "backing: memory\nquads: 12\ncovered: true\n";
        assert_eq!(info_line(info, "quads"), Some("12"));
        assert_eq!(info_line(info, "backing"), Some("memory"));
        assert_eq!(info_line(info, "nothing"), None);
    }

    #[test]
    fn a_walk_adds_up_the_files_under_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), [0; 10]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b"), [0; 5]).unwrap();
        assert_eq!(size_on_disk(dir.path()), 15);
        assert_eq!(size_on_disk(&dir.path().join("missing")), 0);
    }

    #[test]
    fn this_binary_knows_what_it_links() {
        let lock = include_str!("../Cargo.lock");
        for (name, version) in [
            ("ikigai-ledger", env!("GONK_LINKS_LEDGER")),
            ("ikigai-browse", env!("GONK_LINKS_BROWSE")),
        ] {
            let wanted = format!("name = \"{name}\"\nversion = \"{version}\"");
            assert!(lock.contains(&wanted), "{name} {version}");
        }
        assert!(env!("GONK_BUILT_AT_SECONDS").parse::<u64>().unwrap() > 0);
    }
}
