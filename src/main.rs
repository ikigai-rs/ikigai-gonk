//! `ikigai-gonk` — the process: flags, the store open, three doors, the banner.
//!
//! The composition lives in the library ([`ikigai_gonk::compose`]) so the conformance walk
//! holds the kernel this `main` serves; the doors and what each grants are
//! [`ikigai_gonk::doors`], and the HTML face is [`ikigai_gonk::web`].

use std::sync::Arc;

use ikigai_gonk::config::{self, Command, Homes};
use ikigai_gonk::grants::{self, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::{compose, doors, quic, web};
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
        Command::PasskeyInvite {
            name,
            ledgers,
            force,
            minutes,
            flags,
        } => passkey_invite(&name, &ledgers, force, minutes, &flags),
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

    // Authority is decided before the store opens: a broken authority file stops the server
    // instead of degrading it. `grants.json` feeds BOTH identity doors (certificates and
    // passkeys), so its broad-token check runs whether or not QUIC opens.
    let layout = quic::Layout::in_config_home(&homes.config);
    quic::read_grants(&layout.grants_json())
        .and_then(|grants| quic::check_grants(&grants))
        .unwrap_or_else(|e| fail(&e));
    let quic_door = quic::open_door(&layout, settings.quic).unwrap_or_else(|e| fail(&e));
    // The passkey file is read here too, so an unparsable `passkeys` block stops the server
    // rather than failing every sign-in.
    identity::read_passkeys(&layout).unwrap_or_else(|e| fail(&e));

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
        quic::QuicDoor::Off(why) => why,
        quic::QuicDoor::Open {
            addr,
            identity,
            trusted,
            enrolled,
        } => {
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
        let port = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(settings.http.port());
        let passkeys = Arc::new(Passkeys::new(layout.clone(), port));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: settings.http_ledgers.clone(),
            passkeys: Arc::clone(&passkeys),
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        eprintln!(
            "ikigai-gonk {} — holding the store at {}",
            env!("CARGO_PKG_VERSION"),
            store_path.display()
        );
        // ★ The `localhost` form, never the bound IP: WebAuthn refuses an IP address as a
        // relying party, so a page opened at 127.0.0.1 cannot use a passkey at all.
        eprintln!(
            "  http    http://localhost:{port}/ — loopback ({}); anonymous read+write: {}; {} passkey(s)",
            settings.http,
            settings.http_ledgers.join(", "),
            passkeys.enrolled_count()
        );
        eprintln!("  socket  {} — owner only", settings.socket.display());
        eprintln!("  quic    {quic_line}");
        eprintln!(
            "  mount   mount = \"prefer urn:iki:ledger:={}\"  (and the same for urn:iki:store:)",
            settings.socket.display()
        );
        let error = ikigai_web::serve_with_listener(
            http,
            doors::http_cap(doors::HttpDoor {
                anonymous: http_grants,
                port,
                passkeys: Some(passkeys),
            }),
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
    println!("  restart ikigai-gonk: trusted certificates are read at startup");
    if cert.is_some() {
        // An imported certificate's bundle holds no private key: the client already has one.
        println!(
            "  copy {} into the client's own identity directory (beside its client.crt and \
             client.key), then from the client:",
            bundle.dir.join("server.crt").display()
        );
        println!("    ikigai --connect quic://<gonk host>:1060 --cert-dir <that directory>");
    } else {
        println!(
            "  this bundle holds the client's PRIVATE key and the server never reads it — move \
             the directory to the client, then from the client:"
        );
        println!("    ikigai --connect quic://<gonk host>:1060 --cert-dir <the moved directory>");
    }
}

fn passkey_invite(
    name: &str,
    ledgers: &[(String, Authority)],
    force: bool,
    minutes: u64,
    flags: &config::Flags,
) {
    let homes = Homes::from_process().unwrap_or_else(|e| fail(&e));
    let (_, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    let settings = config::settings(flags, &text, &homes).unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    if ledgers.is_empty() {
        fail(&format!(
            "an invite needs a grant: `ikigai-gonk passkey invite {name} --ledger default=delete`"
        ));
    }
    let mut scopes: Vec<String> = Vec::new();
    for (ledger, authority) in ledgers {
        for token in grants::grants_for(ledger, *authority).unwrap_or_else(|e| fail(&e)) {
            if !scopes.contains(&token) {
                scopes.push(token);
            }
        }
    }
    // ★ An identity must be STRICTLY stronger than an anonymous loopback caller, or signing
    // in would be a ceremony that changes nothing — and a grant that looked like it limited
    // someone would not.
    let anonymous = grants::grants_for_all(&settings.http_ledgers, Authority::Write)
        .unwrap_or_else(|e| fail(&e));
    if scopes.iter().all(|scope| anonymous.contains(scope)) {
        fail(&format!(
            "that grant adds nothing: an anonymous loopback caller already reads and writes {}. \
             Invite with more — `--ledger {}=delete`, or a ledger outside `gonk.http.ledger`",
            settings.http_ledgers.join(", "),
            settings
                .http_ledgers
                .first()
                .map(String::as_str)
                .unwrap_or("default")
        ));
    }
    let code = identity::invite(
        &layout,
        name,
        &scopes,
        force,
        minutes,
        identity::now_seconds(),
    )
    .unwrap_or_else(|e| fail(&e));
    println!(
        "passkey invite for `{name}` — grant `{name}` ({} scopes) in {}",
        scopes.len(),
        layout.grants_json().display()
    );
    println!("  valid for {minutes} minutes, once");
    println!(
        "  open  http://localhost:{}/#invite={code}",
        settings.http.port()
    );
    println!(
        "  in a browser on this machine (localhost, not 127.0.0.1); the passkey is enrolled in {}",
        layout.clients_json().display()
    );
}

fn fail(message: &str) -> ! {
    eprintln!("ikigai-gonk: {message}");
    std::process::exit(1);
}
