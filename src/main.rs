//! `ikigai-gonk` — the process: flags, the store open, three doors, the banner.
//!
//! The composition lives in the library ([`ikigai_gonk::compose`]) so the conformance walk
//! holds the kernel this `main` serves; the doors and what each grants are
//! [`ikigai_gonk::doors`].

use std::sync::Arc;

use ikigai_gonk::config::{self, Command, Homes};
use ikigai_gonk::grants::{self, Authority};
use ikigai_gonk::{compose, doors, quic};
use ikigai_store::{DurableStore, StoreConfig};

fn main() {
    let command = config::parse_args(std::env::args().skip(1))
        .unwrap_or_else(|e| fail(&format!("{e}\n\n{}", config::USAGE)));
    match command {
        Command::Help => print!("{}", config::USAGE),
        Command::Grants { ledger, authority } => {
            let tokens = grants::grants_for(&ledger, authority).unwrap_or_else(|e| fail(&e));
            println!(
                "{}",
                serde_json::to_string_pretty(&tokens).expect("strings serialize")
            );
        }
        Command::ClientAdd {
            name,
            cert,
            ledgers,
            force,
        } => client_add(&name, cert.as_deref(), &ledgers, force),
        Command::Serve(flags) => serve(&flags),
    }
}

fn serve(flags: &config::Flags) -> ! {
    let homes = Homes::from_process().unwrap_or_else(|e| fail(&e));
    let (config_path, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    // Not prefixed with the config path: a setting may have come from a flag, and a message
    // that names the file sends the operator to edit a line that is not there.
    let settings = config::settings(flags, &text, &homes).unwrap_or_else(|e| {
        fail(&format!(
            "{e}\n  (config read from {})",
            config_path.display()
        ))
    });
    let http_grants = grants::grants_for_all(&settings.http_ledgers, Authority::Write)
        .unwrap_or_else(|e| fail(&e));

    // The QUIC posture is decided before the store opens: a broken authority file stops the
    // server instead of degrading it. The door opens only once `clients.json` exists.
    let layout = quic::Layout::in_config_home(&homes.config);
    let quic_door = settings.quic.and_then(|addr| {
        let enrolment = quic::read_enrolment(&layout.clients_json()).unwrap_or_else(|e| fail(&e))?;
        let grants = quic::read_grants(&layout.grants_json()).unwrap_or_else(|e| fail(&e));
        quic::check_grants(&grants).unwrap_or_else(|e| fail(&e));
        let (identity, _) = quic::server_identity(&layout).unwrap_or_else(|e| fail(&e));
        let trusted: Vec<String> = quic::trusted_client_certs(&layout)
            .unwrap_or_else(|e| fail(&e))
            .into_iter()
            .map(|(_, pem)| pem)
            .collect();
        if trusted.is_empty() {
            fail(&format!(
                "{} exists but no client certificate is trusted — add one with `ikigai-gonk client add <name>`, or start with --no-quic",
                layout.clients_json().display()
            ));
        }
        Some((addr, identity, trusted, enrolment.len()))
    });

    // Hold the store. A second holder is refused by RocksDB — and the fix is topology.
    let store_path = StoreConfig::load(Some("gonk"))
        .unwrap_or_else(|e| fail(&e.to_string()))
        .path;
    let store = DurableStore::open(&store_path).unwrap_or_else(|e| {
        fail(&format!(
            "cannot hold the store at {}: {e}\n  gonk is the one process on a machine that opens the dataset. If \
             the holder is another gonk, this one has nothing to add — reach that one through its socket. If it \
             is anything else (an `ikigai` with `store = true`), stop it and give it these two lines, naming the \
             socket this gonk serves on:\n    \
             mount = \"prefer urn:iki:store:={sock}\"\n    mount = \"prefer urn:iki:ledger:={sock}\"",
            store_path.display(),
            sock = settings.socket.display()
        ))
    });
    let hub = Arc::new(compose(store));

    // The socket door. Bound only after the store is held, so it can never replace the
    // socket of a gonk that is still running.
    if let Some(parent) = settings.socket.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| fail(&format!("creating {}: {e}", parent.display())));
    }
    let (socket, door) = (
        settings.socket.clone(),
        doors::door_kernel(Arc::clone(&hub)),
    );
    std::thread::spawn(move || {
        let error = ikigai_ipc::serve(door, &socket).err();
        fail(&format!(
            "the socket door at {} stopped: {error:?}",
            socket.display()
        ))
    });

    let quic_line = match quic_door {
        None if settings.quic.is_none() => "off (--no-quic)".to_string(),
        None => format!(
            "off — no {} (enrol a client with `ikigai-gonk client add`)",
            layout.clients_json().display()
        ),
        Some((addr, identity, trusted, enrolled)) => {
            let (door, minter) = (
                doors::door_kernel(Arc::clone(&hub)),
                quic::minter(layout.clone()),
            );
            let line = format!(
                "udp {addr} — {} trusted certificate(s), {enrolled} enrolled",
                trusted.len()
            );
            std::thread::spawn(move || {
                let error = ikigai_quic::serve(door, addr, &identity, &trusted, minter).err();
                fail(&format!("the QUIC door on {addr} stopped: {error:?}"))
            });
            line
        }
    };

    let runtime =
        tokio::runtime::Runtime::new().unwrap_or_else(|e| fail(&format!("tokio runtime: {e}")));
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(settings.http)
            .await
            .unwrap_or_else(|e| fail(&format!("cannot bind http {}: {e}", settings.http)));
        eprintln!(
            "ikigai-gonk {} — holding the store at {}",
            env!("CARGO_PKG_VERSION"),
            store_path.display()
        );
        eprintln!(
            "  http    http://{} — loopback; ledgers read+write: {}",
            settings.http,
            settings.http_ledgers.join(", ")
        );
        eprintln!("  socket  {} — owner only", settings.socket.display());
        eprintln!("  quic    {quic_line}");
        eprintln!(
            "  mount   mount = \"prefer urn:iki:ledger:={}\"  (and the same for urn:iki:store:)",
            settings.socket.display()
        );
        let error = ikigai_web::serve_with_listener(
            hub,
            doors::http_cap(http_grants),
            listener,
            doors::edge_config(),
        )
        .await;
        fail(&format!("the http door stopped: {error:?}"))
    })
}

fn client_add(
    name: &str,
    cert: Option<&std::path::Path>,
    ledgers: &[(String, Authority)],
    force: bool,
) {
    let homes = Homes::from_process().unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    // Every ledger name is checked before anything is written.
    let mut scopes: Vec<String> = Vec::new();
    for (ledger, authority) in ledgers {
        for token in grants::grants_for(ledger, *authority).unwrap_or_else(|e| fail(&e)) {
            if !scopes.contains(&token) {
                scopes.push(token);
            }
        }
    }
    let bundle = quic::add_client(&layout, name, cert, force).unwrap_or_else(|e| fail(&e));
    println!("client `{name}`  {}", bundle.dir.display());
    println!("  fingerprint  {}", bundle.fingerprint);
    if scopes.is_empty() {
        println!("  NOT enrolled — a trusted certificate with no grant is refused. Enrol it:");
        println!("    ikigai-gonk client add {name} --ledger default=write");
    } else {
        quic::enrol(&layout, name, &bundle.fingerprint, &scopes, force)
            .unwrap_or_else(|e| fail(&e));
        println!(
            "  enrolled     grant `{name}` ({} scopes) in {}",
            scopes.len(),
            layout.grants_json().display()
        );
    }
    if bundle.key_minted {
        println!("  this bundle holds the client's PRIVATE key; the server never reads it — move the directory to the client");
    }
    println!(
        "  restart ikigai-gonk (trusted certificates are read at startup), then from the client:"
    );
    println!("    ikigai --connect quic://<gonk host>:1060 --cert-dir <this directory>");
}

fn fail(message: &str) -> ! {
    eprintln!("ikigai-gonk: {message}");
    std::process::exit(1);
}
