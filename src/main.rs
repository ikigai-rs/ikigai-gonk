//! `ikigai-gonk` — the process: flags, the store open, three doors, the banner.
//!
//! The composition lives in the library ([`ikigai_gonk::compose`]) so the conformance walk
//! holds the kernel this `main` serves; the doors and what each grants are
//! [`ikigai_gonk::doors`], and the HTML face is [`ikigai_gonk::web`].

use std::sync::Arc;

use ikigai_gonk::backfill;
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::config::{self, BrowseGrant, Command, Homes};
use ikigai_gonk::grants::{self, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::watch::Watched;
use ikigai_gonk::{
    access, browse, compose_serving, doors, layer, mount, queue, quic, roots, trigger, verdict,
    watch, web,
};
// ★ No `SharerWrites` here any more, and that absence is the shape of ledger #282's fix: the
// promise is not a type this file names, it is `browse::Graph` read as a declaration.
use ikigai_store::{DurableStore, StoreConfig};
use ikigai_time::JobRegistry;

fn main() {
    // First, before any thread exists: the stack every request-serving thread gets, including
    // the ones the socket and QUIC transports spawn themselves (ledger #915, `stack`).
    ikigai_gonk::stack::enlarge_default();
    let command = config::parse_args(std::env::args().skip(1))
        .unwrap_or_else(|e| fail(&format!("{e}\n\n{}", config::USAGE)));
    match command {
        Command::Help => print!("{}", config::USAGE),
        Command::Grants { ledger, authority } => {
            let tokens = match &ledger {
                Some(ledger) => grants::grants_for(ledger, authority),
                None => grants::browse_graph_grants(authority),
            }
            .unwrap_or_else(|e| fail(&e));
            println!(
                "{}",
                serde_json::to_string_pretty(&tokens).expect("strings serialize")
            );
        }
        Command::GrantsLedgerGraph { ledger } => {
            let tokens = grants::ledger_graph_grants(&ledger).unwrap_or_else(|e| fail(&e));
            println!(
                "{}",
                serde_json::to_string_pretty(&tokens).expect("strings serialize")
            );
        }
        Command::GrantsBrowse { browse, flags } => {
            let settings = read_settings(&flags);
            let tokens = browse_tokens(Some(&browse), &settings);
            println!(
                "{}",
                serde_json::to_string_pretty(&tokens).expect("strings serialize")
            );
        }
        Command::ReviewRequest { repo, path, flags } => review_request(&repo, &path, &flags),
        Command::LayerTake { offer, flags } => layer_take(&offer, &flags),
        Command::Browse { op, flags } => browse_command(&op, &flags),
        Command::ClientAdd {
            name,
            cert,
            ledgers,
            browse_graph,
            ledger_graphs,
            browse,
            scripts,
            nl_draft,
            force,
            rotate,
            flags,
        } => {
            // The full settings are read only when a role needs this server's roots and
            // mount, so a ledger-only enrollment works exactly as before on a host with no
            // config.toml; the QUIC port for the printed line is read either way.
            let browse_scopes = if browse.is_some() || nl_draft {
                role_tokens(browse.as_ref(), nl_draft, &read_settings(&flags))
            } else {
                Vec::new()
            };
            client_add(
                &name,
                cert.as_deref(),
                &scopes_for(
                    &Subjects {
                        ledgers: &ledgers,
                        browse_graph,
                        ledger_graphs: &ledger_graphs,
                        scripts: &scripts,
                    },
                    &browse_scopes,
                ),
                Rewrite { force, rotate },
                &flags,
            )
        }
        Command::ClientList { flags } => client_list(&flags),
        Command::ClientRemove {
            key,
            by_name,
            flags,
        } => client_remove(&key, by_name, &flags),
        Command::PasskeyList { flags } => passkey_list(&flags),
        Command::PasskeyRevoke {
            key,
            by_label,
            flags,
        } => passkey_revoke(&key, by_label, &flags),
        Command::PasskeyLogoutAll {
            key,
            by_label,
            flags,
        } => passkey_logout_all(&key, by_label, &flags),
        Command::PasskeyInvite {
            name,
            ledgers,
            browse_graph,
            ledger_graphs,
            browse,
            scripts,
            nl_draft,
            force,
            minutes,
            flags,
        } => passkey_invite(
            &name,
            &Subjects {
                ledgers: &ledgers,
                browse_graph,
                ledger_graphs: &ledger_graphs,
                scripts: &scripts,
            },
            browse.as_ref(),
            nl_draft,
            force,
            minutes,
            &flags,
        ),
        Command::Onboard(args) => onboard_command(&args),
        Command::RoborevFile(args) => {
            let door = bridge_door(args.quic.clone(), &args.flags);
            ikigai_gonk::roborev::run(&args, &door, &mut std::io::stdout())
                .unwrap_or_else(|e| fail(&e));
        }
        Command::Checkout(args) => {
            // The same two homes `serve` takes (ledger #918), resolved by the same function.
            let homes = Homes::resolve(&config::Flags {
                config_home: args.config_home.clone(),
                data_home: args.data_home.clone(),
                ..config::Flags::default()
            })
            .unwrap_or_else(|e| fail(&e));
            let all_ok = ikigai_gonk::checkout::run(&args, &homes, &mut std::io::stdout())
                .unwrap_or_else(|e| fail(&e));
            if !all_ok {
                std::process::exit(1);
            }
        }
        Command::KataImport(args) => {
            let door = bridge_door(args.quic.clone(), &args.flags);
            ikigai_gonk::kata::run(&args, &door, &mut std::io::stdout())
                .unwrap_or_else(|e| fail(&e));
        }
        Command::BackfillKeys(args) => backfill_keys(&args),
        Command::FindingsFile(args) => findings_file(&args),
        Command::Serve(flags) => serve(&flags),
    }
}

/// The door a bridge files through: `--quic` when given, else the socket the config names
/// (ledger #2075: `--quic` wins when both are configured).
fn bridge_door(
    quic: Option<ikigai_gonk::bridge::Door>,
    flags: &config::Flags,
) -> ikigai_gonk::bridge::Door {
    quic.unwrap_or_else(|| ikigai_gonk::bridge::Door::Socket(read_settings(flags).socket))
}

fn serve(flags: &config::Flags) -> ! {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let (config_path, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    // Not prefixed with the config path: a setting may have come from a flag, and a message
    // that names the file sends the operator to edit a line that is not there.
    let settings = config::settings(flags, &text, &homes).unwrap_or_else(|e| {
        fail(&format!(
            "{e}\n  (config read from {})",
            config_path.display()
        ))
    });
    // Before the store is opened: a socket the kernel cannot bind stops the server here.
    config::check_socket_path(&settings.socket).unwrap_or_else(|e| fail(&e));
    // The anonymous loopback caller: its ledgers' READ tokens (ledger #1687), and the right to run
    // and read a PUBLIC script (ledger #939, `script::ANONYMOUS`).
    // And, when `gonk.toolbox.dist` is set, a read on exactly the toolbox's files (ledger #1933,
    // `grants::toolbox_read_grants`): the toolbox reads its build under its caller's capability.
    let mut http_grants =
        grants::anonymous_http_grants(&settings.http_ledgers).unwrap_or_else(|e| fail(&e));
    if let Some(toolbox) = &settings.toolbox {
        http_grants.extend(toolbox.grants());
    }

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
    // And the render rules, for the same reason: a rule table this server cannot obey is a
    // setting that would look exactly like one in effect. Read once, checked once, then
    // SERVED — `urn:iki:gonk:render-rules` is what the renderer resolves, so what a page
    // acted on is what a person can read back.
    let render_rules = read_render_rules(&layout).unwrap_or_else(|e| fail(&e));

    // The browse roots' watchers start BEFORE the space is built, and the set they actually
    // got decides which roots' reads may be cached. Fail closed, in that order: a root whose
    // platform watcher refuses is served live, exactly as `ikigai-browse` declares it.
    let (root_watch, unwatched) = watch::RootWatch::start(&settings.browse_roots);
    for root in &unwatched {
        eprintln!(
            "ikigai-gonk: browse root `{}` will not be watched ({}) — its reads are served \
             live and uncached",
            root.name, root.reason
        );
    }

    // Hold the store. A second holder is refused by RocksDB — and the fix is topology.
    // `--store` outright, else `store.toml` read from THIS run's homes — `load_in`, so a
    // `--config-home` / `--data-home` moves the dataset with everything else (ledger #799).
    let store_path = match &flags.store {
        Some(path) => path.clone(),
        None => {
            StoreConfig::load_in(&homes.config, &homes.data, Some("gonk"))
                .unwrap_or_else(|e| fail(&e.to_string()))
                .path
        }
    };
    // ★ The handle leaves this crate ONLY when something else in this process needs it. The
    // browse annotation family takes an `Arc<Store>`, which is how its quads land in the
    // dataset the ledger lives in — the whole point of composing them here.
    //
    // ★★ And when it leaves, gonk SAYS WHERE THAT HOLDER WRITES. `SharerWrites` is a promise
    // to `ikigai-store`, which answers it per read: a scoped read of a graph outside the
    // promise is cacheable again, and every read that can see the whole dataset stays
    // `Expiry::Always`. Without the promise the store must assume the worst and make EVERY
    // read `Expiry::Always` — and expiry propagates, so that would de-cache every ledger read
    // as a side effect of adding a browse face (`tests/browse.rs` prints both numbers).
    //
    // ★ The promise is DERIVED, not transcribed. `browse::Graph` is gonk's choice of where
    // the family's quads land, made once, on the line below; `sharer_writes()` is that choice
    // read as a declaration and `browse::wire` is the same choice read as a mount. Until
    // `ikigai-browse` 0.4.0's `Mount::graph` this line said `only_the_default_graph()` because
    // a person had gone and read browse's three writers — a fact about a dependency's
    // internals, re-typed here, which is what ledger #282 filed. ⚠ A FALSE promise is silent,
    // unbounded staleness (reads of a graph the sharer does write, cached against threads its
    // write never cuts), and `tests/browse.rs` fingerprints the reserved graphs around a real
    // browse WRITE and a real browse READ to keep the derivation honest at both ends.
    //
    // ★ And the handle leaves ALWAYS since ledger #1699, because the backup reads through it: it
    // streams every quad from one snapshot into its archive (`backup::Dataset`) instead of
    // holding the whole dataset as a SPARQL answer. The backup writes nothing, so the promise
    // is the same derived one with or without a browse face; without one it promises graphs
    // nobody writes, which costs only the caching of reads that can see the default graph (the
    // ledgers live in named graphs, so their scoped reads stay cached).
    let browse_graph = browse::Graph::chosen();
    let browses = !settings.browse_roots.is_empty();
    let opened = DurableStore::open_shared_declaring(&store_path, browse_graph.sharer_writes());
    let (store, handle) = opened.unwrap_or_else(|e| {
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
    let explains = settings.explains();
    // ★★ Obligation 1, checked before a single door opens. The choice above confines browse's
    // reads to one graph; quads it wrote into another are still on disk and no longer visible
    // to any of them, and NOTHING anywhere reports that — an empty annotation list and an
    // empty archive are legitimate answers. So this server counts them and says so, in
    // browse's own numbers.
    if browses {
        warn_unmigrated(&handle, &browse_graph, &store_path, &settings.socket);
    }
    // What the backup reads, and under which of the store's bounds (ledger #1699).
    let dataset = backup::Dataset::new(&store, Arc::clone(&handle));
    // ★ The store's OWN writer (ledger #1252): browse's writes commit on the thread and
    // under the lock every store door uses, and carry the declaration made above. Kept, so a
    // re-arrangement of the roots (ledger #2154) builds over the same one.
    let writer = store
        .shared_writer()
        .expect("a store opened shared hands out its writer");
    let browse = browses.then(|| {
        browse::wire(
            settings.browse_roots.clone(),
            writer.clone(),
            Some(&root_watch),
            explains.then_some(&settings.explain),
            // The SAME value the declaration above was derived from — not a second
            // `Graph::chosen()`, which would be a second decision.
            &browse_graph,
            Some(homes.config.clone()),
        )
    });
    let browse_line = browse_line(&settings.browse_roots, root_watch.watched(), &browse_graph);
    // What this process spends on review passes — the queue's slot, and every review in
    // flight through the browse family whoever started it (the page's Review button too), so
    // the judge backfill can yield to all of them (ledger #702 item 4).
    let activity = Arc::new(trigger::Activity::default());
    let (browse_space, style) = match browse {
        Some(wired) => (
            Some(Arc::new(
                wired
                    .space
                    .observing_reviews(Arc::clone(&activity))
                    // "On GitHub" with the operator's `gonk.browse.github` lines (ledger #1907).
                    .with_github(Arc::new(ikigai_gonk::github::space(
                        &settings.browse_roots,
                        &settings.browse_github,
                    ))),
            )),
            Some(wired.style),
        ),
        None => (None, None),
    };
    // ★ The roots as live data (ledger #2154, `roots`): a root added to or removed from
    // config.toml mounts or unmounts with no restart. Live only when the roots came from the
    // file and the family is composed; otherwise the status says why not.
    let roots_source = if !flags.browse_roots.is_empty() {
        roots::Source::Flags
    } else {
        match &browse_space {
            Some(browse) => {
                let explain = explains.then(|| settings.explain.clone());
                let (graph, config_home, writer) =
                    (browse_graph.clone(), homes.config.clone(), writer.clone());
                let build: roots::Build = Box::new(move |roots, watch, github| {
                    browse::wire(
                        roots.to_vec(),
                        writer.clone(),
                        Some(watch),
                        explain.as_ref(),
                        &graph,
                        Some(config_home.clone()),
                    )
                    .space
                    .with_github(Arc::new(ikigai_gonk::github::space(roots, github)))
                });
                roots::Source::Config {
                    path: config_path.clone(),
                    home: homes.home.clone(),
                    live: Box::new(roots::Live {
                        browse: Arc::clone(browse),
                        epochs: root_watch.epochs(),
                        build,
                    }),
                }
            }
            None => roots::Source::NotComposed {
                path: config_path.clone(),
                home: homes.home.clone(),
            },
        }
    };
    let live_roots = roots::LiveRoots::new(
        roots_source,
        settings.browse_roots.clone(),
        settings.browse_github.clone(),
        unwatched.clone(),
        None,
    );
    // The mounts are built before the kernel and dialled by neither: `crate::mount` dials on
    // first use, so a peer that is down costs this server nothing at startup and the ledger
    // is served whether or not a model can be reached.
    // ★ ONE model-call window for every mount line (ledger #308): spend is counted where it
    // leaves for the model, whoever and whatever is spending it (`budget::model_calls`).
    let model_calls = ikigai_gonk::budget::model_calls(settings.model_calls_per_hour);
    let mut mounted: Vec<Arc<dyn ikigai_core::Space>> = settings
        .mounts
        .iter()
        .map(|m| mount::space(m, &model_calls))
        .collect();
    // ★ The layer bindings' mounts (ledger #1190): `urn:layer:{layer}` per `gonk.layer` line,
    // then `urn:layers:` and `urn:collab:`, on one door nothing dials until it is used.
    let layer_mounts = layer::mount(&settings.layers);
    mounted.extend(layer_mounts.spaces.iter().cloned());
    let mount_line = mount_line(&settings, explains);

    // The backup rotation, and the timer that fires it. Both are built before the kernel
    // because the kernel binds the family; the timer is handed the kernel afterwards.
    let backup_settings = Arc::new(backup::Settings {
        dir: settings.backup.dir.clone(),
        keep: settings.backup.keep,
        every: settings.backup.every,
        store_path: store_path.clone(),
        scripts: Some(settings.script_dir.clone()),
    });
    // ★ The backup fires under exactly `backup::JOB_SCOPES` and nothing else, so a scheduled
    // backup has the authority to read every graph and write the rotation — and no authority
    // to write a single quad. The list also carries the store's budget grant
    // (`backup::JOB_BUDGET`, ledger #979): scoped callers get the store's 5 s base, and the
    // whole-dataset query takes several times that — and, since store 0.2.9, both of the
    // store's ANSWER grants (`backup::JOB_ANSWER_ROWS` and `JOB_ANSWER_BYTES`, ledger #993):
    // scoped callers' answers are bounded at 100,000 rows and 16 MiB, and the backup's is
    // every quad in the dataset. Since ikigai-time 0.4.0 (ledger #79) the registry's capability
    // is a CEILING and each job records its own; a tick fires under `ceiling.clamp(job)`.
    // Both are `JOB_SCOPES` here, so the clamp is exactly `JOB_SCOPES`. `Capability::root()`
    // is still the registry's default ceiling and would be silently broader than anything this
    // server hands any caller, which is why the ceiling is set as well as the job's own.
    // ★ Ledger #1087: the registry's timer is phased to the ROTATION, so its first tick comes
    // when the newest archive is one cadence old rather than one cadence after this start —
    // see `backup::first_delay`.
    let backup_next = backup::NextRun::default();
    let (jobs, first_backup) = match settings.backup.every {
        Some(every) => {
            let (jobs, first) = backup::registry(&backup_settings, every, &backup_next);
            (Some(jobs), Some(first))
        }
        None => (None, None),
    };
    // The review queue, when one is configured. `prepare` creates the tree 0700 up front for
    // the reason `ikigai-intray`'s own watcher does: a directory that appears later races
    // whatever writes into it, and `review request` writes with no server running.
    //
    // ★ The reviewer's SCOPES are resolved here, before the kernel exists, because a grant
    // that `grants.json` cannot honor must stop this server rather than arm a reactor that
    // dead-letters everything. The reactor itself needs the kernel, so it is installed after
    // the hub below — the two halves of arming, in the only order they can happen.
    let reviewer = match &settings.review {
        Some(t) => resolve_reviewer(t, &layout),
        None => None,
    };
    let mut trigger_spaces = match &settings.review {
        Some(t) => {
            trigger::prepare(t).unwrap_or_else(|e| fail(&e));
            trigger::space(
                t,
                Arc::clone(&activity),
                reviewer.is_some(),
                settings.queue.clone(),
            )
        }
        None => Vec::new(),
    };
    // ★ The judge backfill (ledger #696), bound only where judge-finding is — beside the
    // explain families, so only with a peer to judge on. It runs under exactly the browse read
    // and a net grant naming the mounted peer's host: what judge-finding declares, and nothing
    // a store write, an annotation or `gh` would need — plus the store's host reserve (ledger
    // #1598), a priority rather than an authority; see `backfill::scopes`.
    let backfill = explains.then(|| {
        Arc::new(backfill::Backfill::new(
            live_roots.names(),
            settings.queue.clone(),
            settings.explain.judge.clone(),
            backfill::scopes(&mount_host(&settings)),
            Arc::clone(&activity),
            settings.review.clone(),
        ))
    });
    if let Some(backfill) = &backfill {
        trigger_spaces.push(backfill.space());
    }
    // ★ The script family (ledger #939): its atoms in `gonk.script.dir`, opened before the hub
    // because the hub binds it, and attached after, so a publish re-describes the catalog.
    // ★ One anonymous run window (ledger #1210), shared by the script family (a script's run, an
    // ad hoc plan) and the natural-language family (a draft's dry run).
    let anonymous_runs =
        ikigai_gonk::budget::AnonymousRuns::per_minute(settings.anonymous_runs_per_minute);
    let scripts = ikigai_gonk::script::Scripts::open(&settings.script_dir, anonymous_runs.clone())
        .unwrap_or_else(|e| fail(&e));
    // ★ Natural language to a draft query (ledger #1138): `urn:nl:*`, drafting through the
    // mounted model under the caller's own net grant, and no drafter at all without a mount.
    let nl = ikigai_gonk::nl::Nl::new(
        &settings.nl_ask,
        settings.mount_host().as_deref(),
        anonymous_runs,
    );
    // ★ The onboarding controller (ledger #2009, `onboard`), only with a `gonk.onboard` line:
    // its resources beside the review family's, its key in the keystore, its door its own.
    let onboard = settings.onboard.as_ref().map(|config| {
        ikigai_gonk::onboard::Onboard::from_config(config, layout.clone())
            .unwrap_or_else(|e| fail(&e))
    });
    if let Some(onboard) = &onboard {
        trigger_spaces.push(Arc::new(onboard.space()));
    }
    trigger_spaces.push(Arc::new(live_roots.space()));
    // ★ The named views (ledger #2006, `views`): core's dynamic space LAST in the hub, rebuilt
    // from the declaration graph whenever its own Sink writes it.
    let views = ikigai_gonk::views::Views::new();
    let hub = Arc::new(compose_serving(
        store,
        browse_space,
        mounted,
        trigger_spaces,
        Some(Backups {
            settings: Arc::clone(&backup_settings),
            jobs: jobs.clone(),
            next: backup_next.clone(),
            dataset,
        }),
        Some(&scripts),
        Some(&nl),
        Some(&views),
    ));
    scripts.attach(&hub);
    let onboard_line = match (&onboard, &settings.onboard) {
        (Some(onboard), Some(config)) => {
            onboard.attach(&hub);
            onboard.start();
            format!(
                "controller at {} (lobby `{}`, grants `{}`), key `{}` in the keystore; nodes \
                 are told gonk is at {}; applies the grants layer every {} s; writes need the \
                 owner's socket, the page {}",
                config.target,
                onboard.lobby(),
                onboard.grants(),
                config.key,
                onboard.gonk_door().describe(),
                ikigai_gonk::onboard::APPLY_EVERY.as_secs(),
                ikigai_gonk::onboard::CAP_READ
            )
        }
        _ => format!("off (no {} line)", ikigai_gonk::onboard::KEY),
    };
    // Under ROOT, for the reason on `Views::start` (ledger #2048): a narrowed listener does not
    // hear a declaration it could not cache. The views' own reads stay the caller's.
    let views_line = match views.start(&hub, ikigai_core::Capability::root()) {
        Ok(()) => format!(
            "{} under {}, rebuilt from {} (a Sink needs {}); status at {}",
            ikigai_gonk::views::SPACE,
            ikigai_gonk::views::VIEW_PREFIX,
            ikigai_gonk::views::DECLARATION,
            ikigai_gonk::views::write_grant(),
            ikigai_gonk::views::STATUS
        ),
        Err(e) => e,
    };
    let layer_line =
        layer::start(&settings.layers, &layer_mounts, &hub).unwrap_or_else(|e| fail(&e));
    // ★ The Layers page (ledger #1675): its own door under its own identity, mounted into
    // nothing, and dialled by nothing until a page is first opened.
    let layers_view = settings.layers_view.as_ref().map(|view| {
        ikigai_gonk::layers_view::LayersView::connect(
            view,
            Some(homes.data.join("layers-view").join("cursors.json")),
        )
    });
    let layers_line = match (&settings.layers_view, &layers_view) {
        (Some(_), Some(view)) => format!(
            "/layers reads and posts at {} as the identity its certificates name (asked of the              door on first use); a passkey grant needs {} (and {} to post)",
            view.origin(),
            ikigai_gonk::layers_view::CAP_READ,
            ikigai_gonk::layers_view::CAP_SAY
        ),
        _ => format!(
            "no {} line, so /layers says it is not configured",
            ikigai_gonk::layers_view::KEY
        ),
    };

    // ★ The Queue's serious set, checked against the finding contract THIS kernel binds
    // (ledger #496): a word the contract does not declare stops this server naming both
    // lists, rather than serving a queue that asks about nothing. The same shape as
    // `gonk.review.arm` without a space — a line for a queue this server does not serve is
    // a refusal, not a no-op.
    let queue_line = if settings.browse_roots.is_empty() {
        if settings.queue.configured {
            fail(
                "gonk.queue.serious is set and no gonk.browse.root is configured: there is no \
                 finding contract to check it against and no queue for it to narrow",
            );
        }
        "no gonk.browse.root, so no findings and no Queue page".to_string()
    } else {
        let declared = queue::check_serious(&hub, &settings.queue).unwrap_or_else(|e| fail(&e));
        queue_line(&settings, &declared)
    };
    // ★ The judge's verdict words, read from the findings contract THIS kernel binds (ledger
    // #702 item 1): the Queue orders, folds and hides by them and spells none of them, so a
    // browse below the floor that declares no set stops this server naming the floor, rather
    // than serving a Queue that orders every judged row as uncertain in silence.
    let verdicts = settings
        .browse_roots
        .first()
        .map(|(root, _)| verdict::adopt(&hub, root).unwrap_or_else(|e| fail(&e)));
    let judge_line = judge_line(&settings, explains, verdicts);

    // ★★ ARMING, and it is deliberately the last thing before the doors: the reviewer's
    // grant is checked against the CONTRACT of the review this kernel actually binds, and a
    // grant that is short of it — or that carries the publish token — stops this server.
    let review_line = match (&settings.review, &reviewer) {
        (Some(queue), Some(scopes)) => {
            let host = mount_host(&settings);
            let probe = settings
                .browse_roots
                .first()
                .map(|(name, _)| trigger::review_probe_iri(name))
                .unwrap_or_else(|| {
                    fail(
                        "gonk.review.arm is set and no gonk.browse.root is configured: there \
                         is no repository to review, so no pass could ever resolve",
                    )
                });
            trigger::check_reviewer(&hub, &probe, &host, scopes).unwrap_or_else(|e| fail(&e));
            let recovered =
                trigger::arm(queue, Arc::clone(&hub), scopes).unwrap_or_else(|e| fail(&e));
            format!("{}. {recovered}", armed_line(queue, scopes.len(), &host))
        }
        _ => {
            // Not armed: make sure nothing is left behind that a reactor would fire. The
            // handler file lives in the tree a dropper writes into, and an unarmed gonk that
            // left one there would be a loaded gun for the next process that is armed.
            if let Some(queue) = &settings.review {
                trigger::set_handler(queue, false).unwrap_or_else(|e| fail(&e));
            }
            review_line(&settings, &layout)
        }
    };

    // The backfill gets its kernel, and whether a waiting review request is a pass about to
    // run (armed) — it starts nothing: an operator does, with its Sink.
    if let Some(backfill) = &backfill {
        backfill.attach(&hub, reviewer.is_some());
    }

    let backup_line = start_backups(
        &hub,
        &jobs,
        first_backup,
        &backup_settings,
        &settings.socket,
    );

    // Both watchers, now that there is a kernel to cut threads on. `urn:repo:style` declares
    // a thread per `a11y.toml` candidate and browse ships the watch for them; the roots'
    // threads are this server's own (`crate::watch`), declared only for what is watched.
    if let Some(style) = style {
        if let Err(e) = style.spawn(Arc::clone(&hub)) {
            // Not fatal — the sheet still serves — but the symptom (an edit that never
            // lands) is silent, so say it once.
            eprintln!("ikigai-gonk: urn:repo:style will not follow a11y.toml edits: {e}");
        }
    }
    // The badge's epochs, taken before the watch moves onto its thread: the watch bumps a
    // root's when it cuts it, the browse overlay when a write names it (ledger #667).
    let epochs = root_watch.epochs();
    live_roots.attach(&hub);
    if live_roots.is_live() {
        // Held by the live set, which retires it when a re-arrangement replaces it.
        live_roots.hold(root_watch.spawn_held(Arc::clone(&hub)));
        if let Err(e) = live_roots.watch_config() {
            eprintln!(
                "ikigai-gonk: config.toml will not be watched ({e}); `ikigai-gonk browse \
                 add|remove` still mounts and unmounts live"
            );
        }
    } else {
        root_watch.spawn(Arc::clone(&hub));
    }

    // The socket door. Bound only after the store is held, so it can never replace the
    // socket of a gonk that is still running.
    if let Some(parent) = settings.socket.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| fail(&format!("creating {}: {e}", parent.display())));
    }
    // One access line per request at every door (ledger #739), unless `gonk.log.access`
    // turns it off — off means not wrapped at all, not wrapped and silent. gonk's polls are
    // sampled: the Queue badge and the header's layers count, which every open page polls every
    // ten seconds (ledger #767, #1700), and an open Layers page's poll (ledger #1694, #1705).
    let access = |door: access::Door| {
        settings
            .access_log
            .then(|| access::AccessLog::stderr(door).gonk_polls())
    };
    let (socket, door) = (
        settings.socket.clone(),
        doors::door_kernel_with(Arc::clone(&hub), access(access::Door::Socket)),
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
            clients,
        } => {
            let (door, minter) = (
                doors::quic_kernel_with(
                    Arc::clone(&hub),
                    access(access::Door::Quic),
                    layout.clone(),
                ),
                quic::minter(layout.clone()),
            );
            // ★ Bound HERE, before the banner (ledger #1300, the gonk half of ledger #1188), so
            // the banner names the port the kernel chose: `--quic-bind 127.0.0.1:0` used to be
            // announced as `udp 127.0.0.1:0`, a door nobody could find, which made every
            // scratch gonk pick a port by bind-and-release (field guide 9h). The IP is the one
            // configured: a wildcard binds one dual-stack `[::]` socket, and the banner keeps
            // naming what the operator wrote.
            let socket = ikigai_quic::bind(addr)
                .unwrap_or_else(|e| fail(&format!("cannot bind quic {addr}: {e}")));
            let bound = socket
                .local_addr()
                .map(|local| std::net::SocketAddr::new(addr.ip(), local.port()))
                .unwrap_or(addr);
            // The count line stays as it was (a harness reads the port from it); each client
            // follows on a line of its own, named rather than counted (ledger #427).
            let mut line = format!(
                "udp {bound} — {} trusted certificate(s), {enrolled} enrolled",
                trusted.len()
            );
            for client in &clients {
                line.push_str(&format!("\n          {client}"));
            }
            std::thread::spawn(move || {
                let error = ikigai_quic::serve_bounded_on(
                    door,
                    socket,
                    &identity,
                    &trusted,
                    minter,
                    ikigai_quic::DEFAULT_IDLE_TIMEOUT,
                    // A widened grant reaches a standing connection within this bound; a
                    // removed or narrowed one does not wait for it (ledger #1710).
                    Some(quic::MAX_CONNECTION_AGE),
                )
                .err();
                fail(&format!("the QUIC door on {bound} stopped: {error:?}"))
            });
            line
        }
    };

    // The HTTP door's workers and blocking pool, sized explicitly rather than through the
    // default `stack::enlarge_default` set: a request recurses on these threads (ledger #915).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
        .build()
        .unwrap_or_else(|e| fail(&format!("tokio runtime: {e}")));
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(settings.http)
            .await
            .unwrap_or_else(|e| fail(&format!("cannot bind http {}: {e}", settings.http)));
        let port = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(settings.http.port());
        // Sign-ins and sign-outs are written whatever `gonk.log.access` says: they are rare,
        // and they are the audit of who held a session (ledger #1824).
        let passkeys = Arc::new(
            Passkeys::new(layout.clone(), port)
                .with_idle_seconds(settings.session_idle_seconds)
                .auditing(ikigai_gonk::access::stderr_sink()),
        );
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: settings.http_ledgers.clone(),
            // The NAMES of the configured roots, for the header's entry point and the page
            // behind it. The paths went to the mount above; a page never needs one.
            browse_roots: live_roots.names(),
            passkeys: Arc::clone(&passkeys),
            rules: Arc::clone(&render_rules),
            queue: settings.queue.clone(),
            epochs: Some(Arc::clone(&epochs)),
            // The header offers the RDF Toolbox only when `gonk.toolbox.dist` names a build
            // (ledger #2021).
            toolbox: settings.toolbox.is_some(),
        });
        let pages = ikigai_gonk::layers_view::bind(
            web::space(Arc::clone(&face)),
            &face,
            layers_view,
        );
        let pages = ikigai_gonk::onboard::bind_page(pages, &face);
        // The RDF Toolbox (ledger #1933), in this door alone and only when configured.
        let faces = settings
            .toolbox
            .as_ref()
            .map(|toolbox| toolbox.spaces())
            .unwrap_or_default();
        let http = Arc::new(doors::http_kernel_serving(
            Arc::clone(&hub),
            pages,
            faces,
            access(access::Door::Http),
        ));
        eprintln!(
            "ikigai-gonk {} — holding the store at {}",
            env!("CARGO_PKG_VERSION"),
            store_path.display()
        );
        // ★ The URL that REACHES this bind (`doors::http_url`): `localhost` for 127.0.0.1,
        // because WebAuthn refuses an IP address as a relying party and a page opened at
        // 127.0.0.1 cannot use a passkey at all; the bound IP for any other loopback address,
        // where `localhost` would name a port nothing listens on — and then the banner says
        // passkeys cannot work there.
        let bound = std::net::SocketAddr::new(settings.http.ip(), port);
        eprintln!(
            "  http    {} — loopback ({}); anonymous read: {}; {} passkey(s), sessions {} h or {} min idle; anonymous SPARQL budget {} ms",
            doors::http_url(bound),
            settings.http,
            settings.http_ledgers.join(", "),
            passkeys.enrolled_count(),
            identity::SESSION_SECONDS / 3600,
            passkeys.idle_seconds() / 60,
            settings.anonymous_sparql_budget_ms
        );
        if let Some(caveat) = doors::passkey_caveat(bound) {
            eprintln!("          ⚠ {caveat}");
        }
        eprintln!("  browse  {browse_line}");
        eprintln!(
            "  toolbox {}",
            match &settings.toolbox {
                Some(toolbox) => format!(
                    "{}{} — the RDF Toolbox over {}, read under the caller's capability",
                    doors::http_url(bound).trim_end_matches('/'),
                    ikigai_gonk::toolbox::PATH,
                    toolbox.dist().display()
                ),
                None => format!("off (no {} line)", ikigai_gonk::toolbox::KEY),
            }
        );
        eprintln!(
            "  scripts urn:script:* in {} — sparql and plan, read-only (the ledgers' reads); \
             anonymous runs PUBLIC scripts and read-only plans, {} a minute in all (draft checks \
             too), each held to the anonymous budget; a QUIC client drafts, a person publishes; \
             backed up with the store",
            settings.script_dir.display(),
            settings.anonymous_runs_per_minute
        );
        eprintln!(
            "  nl      urn:nl:grounding, urn:nl:sparql:check — under the caller's own grant; {}",
            match settings.mount_host() {
                Some(host) => format!(
                    "urn:nl:sparql drafts through {} for a caller holding urn:cap:net:{host} \
                     (--nl draft), saving DRAFT scripts a person publishes",
                    settings.nl_ask
                ),
                None => "no urn:nl:sparql (no gonk.mount, so no model to draft with)".to_string(),
            }
        );
        eprintln!("  backup  {backup_line}");
        eprintln!("  views   {views_line}");
        eprintln!("  llm     {mount_line}");
        eprintln!("  layer   {layer_line}");
        eprintln!("  layers  {layers_line}");
        eprintln!("  onboard {onboard_line}");
        eprintln!("  review  {review_line}");
        eprintln!("  judge   {judge_line}");
        eprintln!("  queue   {queue_line}");
        eprintln!("  socket  {} — owner only", settings.socket.display());
        eprintln!("  quic    {quic_line}");
        eprintln!(
            "  log     {}",
            if settings.access_log {
                "one `gonk:Access` line per request at each door, on stderr; the Queue \
                 badge's poll only when it fails, takes a second, or is the 60th \
                 (`gonk.log.access = false` turns it off)"
            } else {
                "no access lines (`gonk.log.access = false`)"
            }
        );
        eprintln!(
            "  mount   mount = \"prefer urn:iki:ledger:={}\"  (and the same for urn:iki:store:)",
            settings.socket.display()
        );
        // One door for both seams: the capability (`http_cap`) and the principal (inside
        // `edge_config`) are computed from the same cookie by the same `HttpDoor`.
        let door = doors::HttpDoor {
            anonymous: http_grants,
            // The address actually bound: the configured IP, with the port the listener got
            // (a config may ask for 0). Its IP is one of the names the door answers to
            // (ledger #1045).
            bind: bound,
            passkeys: Some(passkeys),
            anonymous_sparql_budget_ms: settings.anonymous_sparql_budget_ms,
        };
        // Edge refusals write their own access lines (ledger #894).
        let edge = doors::edge_config_with(door.clone(), access(access::Door::Http));
        let edge = match settings.toolbox {
            Some(_) => ikigai_gonk::toolbox::route(edge),
            None => edge,
        };
        // A browser navigating to an error gets it in gonk's own page, with the passkey
        // sign-in on it when signing in would cure it (ledger #2021).
        let edge = ikigai_gonk::refusal::install(edge, door.clone(), Arc::clone(&face));
        let error = ikigai_web::serve_with_listener(
            http,
            doors::http_cap(door),
            listener,
            edge,
        )
        .await;
        fail(&format!("the http door stopped: {error:?}"))
    })
}

/// Install the kernel in the job registry and schedule the backup, returning the banner's
/// backup line.
///
/// ★★ **A recurring timer's FIRST tick is one whole interval away, and that is the trap a
/// 24-hour backup on a supervised daemon falls into.** `KeepAlive` restarts this process on
/// any exit, so a server restarted (or reinstalled) more often than daily would put its
/// backup off with every start. Until ledger #1087 startup only caught up an archive that was
/// ALREADY a cadence old; one 11 hours old at a restart waited the full day after the
/// restart, a 35-hour gap. The registry's timer is now phased to the rotation
/// (`backup::first_delay`): the first run is due one cadence after the newest archive, a
/// minute out at the soonest, so a restart costs a backup nothing and a restart loop does not
/// spam one either.
fn start_backups(
    hub: &Arc<ikigai_core::Kernel>,
    jobs: &Option<JobRegistry>,
    first: Option<std::time::Duration>,
    settings: &Arc<backup::Settings>,
    socket: &std::path::Path,
) -> String {
    let Some(every) = settings.every else {
        return format!(
            "no schedule (--no-backup or gonk.backup.every = \"off\") — {} is still bound; \
             this process is the only thing that can export the dataset",
            backup::BACKUP
        );
    };
    let (Some(jobs), Some(first)) = (jobs, first) else {
        return "no schedule — the registry was not built (this is a bug)".to_string();
    };
    if let Err(e) = backup::start(
        jobs,
        every,
        Arc::clone(hub) as Arc<dyn ikigai_resolve::Resolver>,
    ) {
        // Not fatal: the ledger is served either way, and refusing to start would take the
        // system of record offline over its backup. Loud, because a server that says it is
        // backing up and is not is the failure this whole feature exists to prevent.
        eprintln!("ikigai-gonk: THE BACKUP IS NOT SCHEDULED: {e}");
        return format!("NOT SCHEDULED: {e}");
    }
    let first_line = format!(", the first in {}", humanize(first));
    scheduled_line(every, &settings.dir, settings.keep, &first_line, socket)
}

/// The banner's backup line once the schedule is in — separate so a test can read it,
/// because **the command in it is one an operator will paste**.
///
/// ⚠ The status command names THIS server's socket. The backup family is bound only behind
/// it: no config-home `mount` claims `urn:iki:gonk:`, so a bare `ikigai -c 'source …'`
/// reaches nothing and HANGS rather than failing (ledger #381, an `ikigai-cli` defect). And
/// the path is the one this process binds, not `~/.ikigai/gonk.sock`, so a host configured
/// differently is told the truth.
fn scheduled_line(
    every: std::time::Duration,
    dir: &std::path::Path,
    keep: usize,
    first: &str,
    socket: &std::path::Path,
) -> String {
    format!(
        "every {} into {} — keep {keep}{first}. \
         `ikigai --connect {} -c 'source {}'` says when the last good one was",
        humanize(every),
        dir.display(),
        socket.display(),
        backup::STATUS,
    )
}

/// A duration as the banner reads it.
fn humanize(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        s if s < 3600 => format!("{}m", s / 60),
        s if s % 86_400 == 0 => format!("{}h", s / 3600),
        s => format!("{}h{}m", s / 3600, (s % 3600) / 60),
    }
}

/// ★ **Say, loudly, when this binary's graph choice and the store's contents disagree.**
///
/// The failure this exists for is the quietest one in the server: a gonk that names a graph,
/// run against a store whose browse quads are in another one, serves an EMPTY archive and an
/// EMPTY annotation list, with no error at any layer. Nothing else in the process can tell —
/// the reads succeed.
///
/// ⚠ It warns rather than refusing to start, and that is a judgment worth stating: the
/// ledger, the doors and the backup are unaffected by this condition, and turning a
/// browse-only data problem into a total outage would be the wrong trade for a server whose
/// job is holding the ledger. The operator sequence in the README puts the migration where it
/// belongs — with the server stopped, between two runs.
///
/// A failure to COUNT (an unreadable store) is reported the same way and is not fatal either:
/// this is a check, and a check that can stop the server is a new way to fail.
fn warn_unmigrated(
    store: &ikigai_store::Store,
    graph: &browse::Graph,
    store_path: &std::path::Path,
    socket: &std::path::Path,
) {
    match browse::unmigrated_quads(store, graph) {
        Ok(0) => {}
        Ok(stranded) => eprint!(
            "{}",
            unmigrated_warning(stranded, graph, store_path, socket)
        ),
        Err(e) => eprintln!("ikigai-gonk: could not check where browse's quads are: {e}"),
    }
}

/// The text [`warn_unmigrated`] prints — separate so a test can read it, because **the
/// commands in it are the operator's only lifeline out of this condition** and a wrong one
/// would be found at the worst moment.
///
/// ⚠ Every IRI here is `as_str()`. `NamedNode`'s `Display` brackets it, so `{iri}` inside
/// `<…>` writes `<<urn:…>>` and `--graph {iri}` hands the migration tool an argument it
/// refuses as "not an IRI". That is not hypothetical: it was written that way first, and
/// `the_migration_warning_names_a_command_that_would_run` is why it did not ship.
fn unmigrated_warning(
    stranded: u64,
    graph: &browse::Graph,
    store_path: &std::path::Path,
    socket: &std::path::Path,
) -> String {
    let Some(iri) = graph.named().map(oxigraph::model::NamedNode::as_str) else {
        // The default-graph arm: browse's quads are stranded in some NAMED graph, which only
        // a rollback produces. Say it without naming a migration command that cannot express
        // the reverse move.
        return format!(
            "ikigai-gonk: ⚠ {stranded} quad(s) ikigai-browse wrote are in a NAMED graph, and \
             this binary reads the default one — the archive and every annotation will read \
             EMPTY. This is a binary rolled back past a graph migration; roll it forward \
             again, or migrate the data back.\n"
        );
    };
    let store = store_path.display();
    let socket = socket.display();
    format!(
        "ikigai-gonk: ⚠⚠ THE BROWSE ARCHIVE IS INVISIBLE — {stranded} quad(s) are outside \
         <{iri}>\n  \
         This binary reads and writes browse's quads in that graph. The quads above are still \
         on disk and are no longer visible to ANY browse read: no annotation, no archived \
         explanation, no review finding, and no error anywhere. New writes land in <{iri}> \
         and the two sets will not join.\n  \
         Stop this server and migrate the store — it holds the RocksDB write lock, so nothing \
         else can:\n    \
         cargo install ikigai-browse --version 0.4.0 --locked --features migrate --bin \
         migrate-annotation-ns\n    \
         migrate-annotation-ns {store} --graph {iri}\n    \
         migrate-annotation-ns {store} --graph {iri} --commit\n  \
         The dry run (no --commit) reports this same count. ⚠ --commit is a one-shot \
         destructive rewrite and restarting does not undo it. Take a backup through this \
         server FIRST, while it is still up — it holds the lock, so nothing else can export \
         the dataset:\n    \
         ikigai --connect {socket} -c 'source urn:iki:gonk:backup'\n"
    )
}

/// The banner's browse line: which roots are served, which of them are watched — because
/// "watched" is exactly "its reads are cached", and an operator reading the banner should be
/// able to tell those apart without reading this source — and WHERE the family's quads land,
/// which is the one line an operator needs before writing a SPARQL query or a grant.
fn browse_line(
    roots: &[(String, std::path::PathBuf)],
    watched: &[Watched],
    graph: &browse::Graph,
) -> String {
    if roots.is_empty() {
        return "not composed — no gonk.browse.root; urn:repo:* and the git/gh facades are \
                not bound"
            .to_string();
    }
    let names: Vec<String> = roots
        .iter()
        .map(|(name, _)| {
            if watched.iter().any(|root| &root.name == name) {
                format!("{name} (watched)")
            } else {
                format!("{name} (live, unwatched)")
            }
        })
        .collect();
    let where_they_land = match graph.named() {
        // ⚠ `.as_str()`: `NamedNode`'s Display brackets the IRI itself (ledger #382).
        Some(iri) => format!("annotations and archive in <{}>", iri.as_str()),
        None => "annotations and archive in this dataset's DEFAULT graph — no token can name \
                 it, so every query over them is a root one"
            .to_string(),
    };
    format!("urn:repo:{{{}}}:* — {where_they_land}", names.join(", "))
}

/// The banner's LLM line: where `urn:llm:*` resolves, whether the explanation families are
/// bound because of it, and — when they are — what one derivation may cost.
///
/// The ceilings are on the banner because they are the per-call half of the spend gate, and
/// an operator who cannot see them has no way to know what a configured tier will spend
/// short of reading this source.
fn mount_line(settings: &config::Settings, explains: bool) -> String {
    let Some(mount) = settings.mounts.first() else {
        return "not mounted — no gonk.mount; urn:llm:* resolves nowhere, so explain, review \
                and the PR-derived layers are not bound"
            .to_string();
    };
    if !explains {
        return format!(
            "{} — mounted, but no gonk.browse.root, so nothing here derives through it",
            mount.target
        );
    }
    let tiers = &settings.explain;
    format!(
        "{} (prefer; dialed on first use) — explain/review bound; file {} @{}, dir {} @{}, \
         review {} @{}, pr {} @{} tokens; at most {} model calls an hour, every caller \
         together (gonk.mount.calls_per_hour). Deriving needs urn:cap:net:{}, which \
         `--browse derive` mints and nothing else does",
        mount.target,
        tiers.file.provider,
        tiers.file.max_tokens,
        tiers.dir.provider,
        tiers.dir.max_tokens,
        tiers.review.provider,
        tiers.review.max_tokens,
        tiers.pr.provider,
        tiers.pr.max_tokens,
        settings.model_calls_per_hour,
        mount.target.net_host(),
    )
}

/// The banner's judge line: which provider confirms or refutes each serious finding a pass
/// mints (`gonk.review.judge`), and what the Queue does with its verdict.
fn judge_line(
    settings: &config::Settings,
    explains: bool,
    verdicts: Option<&verdict::Verdicts>,
) -> String {
    if !explains {
        return "not bound — no review pass can run here, so no finding is judged; verdicts \
                already on file still order the Queue"
            .to_string();
    }
    let ordering = match verdicts {
        Some(set) => format!(
            "the Queue orders {} first, then unjudged, {}, could-not-judge, and hides the \
             undecided {} one click away (ledger #704); the words are the findings \
             contract's `{}` set",
            set.upheld().unwrap_or_default(),
            queue::join_or(set.uncertain()),
            set.refuting().unwrap_or_default(),
            verdict::VERDICT_INPUT,
        ),
        None => "no gonk.browse.root, so no Queue to order".to_string(),
    };
    match &settings.explain.judge {
        Some(provider) => format!(
            "{provider} @{} tokens — one call per serious finding a pass mints \
             (gonk.review.judge, gonk.review.judge_max_tokens; browse's defaults are \
             urn:llm:coder:ask @400); {ordering}. Backfill the older queue \
             with `sink {} content={}` on the socket (one call at a time, yielding to review \
             passes; `{}` says where it stands)",
            settings.explain.judge_max_tokens,
            backfill::BACKFILL,
            backfill::START,
            backfill::BACKFILL
        ),
        None => format!(
            "off (gonk.review.judge = \"off\") — passes mint findings with no verdict; \
             {ordering}"
        ),
    }
}

/// The render rules in effect: this deployment's `gonk/render-rules.ttl`, else the table
/// gonk ships. Parsed here so a table the renderer could not obey stops the server — the
/// house rule for configuration, and the reason [`web::Web`] carries the TEXT rather than a
/// parsed table: the text is what `urn:iki:gonk:render-rules` serves back.
fn read_render_rules(layout: &quic::Layout) -> Result<Arc<str>, String> {
    let path = layout.render_rules_ttl();
    let turtle: Arc<str> = match std::fs::read_to_string(&path) {
        Ok(text) => text.into(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ikigai_gonk::rules::DEFAULT_RULES.into())
        }
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    ikigai_gonk::rules::Rules::parse(&turtle)
        .map_err(|e| format!("{}: {e}", path.display()))
        .map(|_| turtle)
}

/// What a minting command line named besides a `--browse` role: `--ledger`s, `--browse-graph`
/// and `--ledger-graph`s — the subjects whose tokens need no server settings to compute.
struct Subjects<'a> {
    ledgers: &'a [(String, Authority)],
    browse_graph: Option<Authority>,
    ledger_graphs: &'a [String],
    /// `--script <name>=<read|run|publish>` (ledger #939).
    scripts: &'a [(String, grants::ScriptAuthority)],
}

impl Subjects<'_> {
    fn is_empty(&self) -> bool {
        self.ledgers.is_empty()
            && self.browse_graph.is_none()
            && self.ledger_graphs.is_empty()
            && self.scripts.is_empty()
    }
}

/// Every token an enrollment asks for: the ledgers' grants, plus — when `--browse-graph` was
/// given — the browse graph's store doors, plus each `--ledger-graph`'s raw store grant
/// (ledger #878), plus a `--browse` role's tokens (already computed against this server's
/// roots and mount by [`browse_tokens`]), in first-seen order.
///
/// ★ One function for both minting paths, because a grant that means different things on a
/// certificate and on a passkey would be a difference nothing in this server could justify.
/// Every name is checked here, before anything is written: a typo refused at mint time is a
/// grant that never exists, and a typo written is a silent denial at first use.
fn scopes_for(subjects: &Subjects<'_>, browse: &[String]) -> Vec<String> {
    let mut scopes: Vec<String> = Vec::new();
    let mut add = |token: String| {
        if !scopes.contains(&token) {
            scopes.push(token);
        }
    };
    for (ledger, authority) in subjects.ledgers {
        for token in grants::grants_for(ledger, *authority).unwrap_or_else(|e| fail(&e)) {
            add(token);
        }
    }
    if let Some(authority) = subjects.browse_graph {
        for token in grants::browse_graph_grants(authority).unwrap_or_else(|e| fail(&e)) {
            add(token);
        }
    }
    for ledger in subjects.ledger_graphs {
        for token in grants::ledger_graph_grants(ledger).unwrap_or_else(|e| fail(&e)) {
            add(token);
        }
    }
    for token in browse {
        add(token.clone());
    }
    for (script, authority) in subjects.scripts {
        for token in grants::script_grants(script, *authority).unwrap_or_else(|e| fail(&e)) {
            add(token);
        }
    }
    scopes
}

/// This server's settings, for a command that mints against them — the same read `serve`
/// makes, so a role is computed against the roots and the mount the server will run with.
/// `ikigai-gonk ledger backfill-keys` — over the socket of the gonk this config describes.
fn backfill_keys(args: &ikigai_gonk::keys::BackfillArgs) {
    let socket = match &args.socket {
        Some(path) => path.clone(),
        None => {
            let flags = config::Flags {
                config: args.config.clone(),
                ..config::Flags::default()
            };
            read_settings(&flags).socket
        }
    };
    let ledger =
        ikigai_ledger::Ledger::parse(&args.ledger).unwrap_or_else(|e| fail(&e.to_string()));
    let issue = ikigai_gonk::keys::over_socket(&socket).unwrap_or_else(|e| fail(&e));
    ikigai_gonk::keys::run(&issue, &ledger, args.dry_run, &mut std::io::stdout())
        .unwrap_or_else(|e| fail(&e));
}

/// `ikigai-gonk findings file` — over the socket of the gonk this config describes, under the
/// `gonk.queue.serious` set that config names (ledger #1453).
fn findings_file(args: &ikigai_gonk::findings::FileArgs) {
    let settings = read_settings(&args.flags);
    let issue = ikigai_gonk::keys::over_socket(&settings.socket).unwrap_or_else(|e| fail(&e));
    ikigai_gonk::findings::run(&issue, args, &settings.queue, &mut std::io::stdout())
        .unwrap_or_else(|e| fail(&e));
}

/// `ikigai-gonk layer take <offer>`: through the running gonk's socket (its layer mount is
/// this gonk's identity at the door, and its ledger is the one this server holds).
fn layer_take(offer: &str, flags: &config::Flags) {
    let settings = read_settings(flags);
    let layer_name = offer
        .strip_prefix("urn:layer:")
        .and_then(|rest| rest.split(':').next())
        .unwrap_or_default();
    let binding = settings
        .layers
        .iter()
        .find(|b| b.layer == layer_name && b.role == layer::Role::Take)
        .unwrap_or_else(|| {
            fail(&format!(
                "no `gonk.layer` line binds layer `{layer_name}` in the take role here"
            ))
        });
    let client: Arc<dyn ikigai_resolve::Resolver> =
        Arc::new(ikigai_ipc::connect(&settings.socket).unwrap_or_else(|e| {
            fail(&format!(
                "cannot reach gonk's socket at {}: {e} (is gonk running? `--socket` names another)",
                settings.socket.display()
            ))
        }));
    let ledger_client = Arc::clone(&client);
    let ledger: ikigai_gonk::taker::LedgerIssue = Arc::new(move |request| {
        let target = request.target.to_string();
        ledger_client
            .issue(request)
            .map(|(r, _)| r.bytes)
            .map_err(|e| format!("{target}: {e}"))
    });
    let door = Arc::new(ikigai_gonk::taker::ResolverDoor(client));
    // The item named in its own ledger, with the page that shows it (ledger #1856).
    let filed = |n: i64| match ikigai_ledger::Ledger::parse(&binding.ledger) {
        Ok(ledger) => {
            let (words, path) = ikigai_gonk::layer::number_link(&ledger, n);
            format!("{words} ({path})")
        }
        Err(_) => format!("{}#{n}", binding.ledger),
    };
    match ikigai_gonk::taker::take(binding, door, &ledger, offer) {
        Ok(ikigai_gonk::taker::Took::Filed(n)) => {
            println!("took <{offer}>; filed as {}", filed(n))
        }
        Ok(ikigai_gonk::taker::Took::Already(n)) => {
            println!(
                "<{offer}> is already taken by this server; filed as {}",
                filed(n)
            )
        }
        Err(e) => fail(&e),
    }
}

/// `ikigai-gonk browse add|remove|list` (ledger #2154): over the running gonk's socket, which
/// writes its OWN config file and mounts or unmounts with no restart. With no gonk to reach,
/// `add` and `remove` write the file here, for the next start.
fn browse_command(op: &config::BrowseOp, flags: &config::Flags) {
    use config::BrowseOp;
    use ikigai_core::{ArgRef, Iri, Request, Verb};
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let (config_path, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    // The socket alone, not the whole settings: a root whose directory is gone stops
    // `settings`, and removing that root is exactly what this command is for.
    let socket = config::socket_for(flags, &text, &homes);
    let iri = |name: &str| {
        Iri::parse(format!("{}{name}", roots::ROOT_PREFIX))
            .unwrap_or_else(|_| fail(&format!("browse: `{name}` cannot be a root name")))
    };
    let request = match op {
        BrowseOp::Add { name, path } => Request::new(Verb::Sink, iri(name))
            .with_arg("content", ArgRef::Inline(path.as_bytes().to_vec())),
        BrowseOp::Remove { name } => Request::new(Verb::Delete, iri(name)),
        BrowseOp::List => Request::new(
            Verb::Source,
            Iri::parse(roots::ROOTS_IRI).expect("a constant IRI"),
        ),
    };
    match ikigai_ipc::connect(&socket) {
        Ok(client) => {
            let (answer, _) = ikigai_resolve::Resolver::issue(&client, request)
                .unwrap_or_else(|e| fail(&e.to_string()));
            print!("{}", String::from_utf8_lossy(&answer.bytes));
        }
        Err(e) => {
            let offline = format!(
                "no gonk answers at {} ({e}), so nothing was mounted",
                socket.display()
            );
            match op {
                BrowseOp::Add { name, path } => {
                    let line = roots::write_root(&config_path, &homes.home, name, path)
                        .unwrap_or_else(|e| fail(&e));
                    let done = match line {
                        ikigai_gonk::checkout::ConfigLine::Added => "wrote",
                        _ => "already had",
                    };
                    println!(
                        "{done} `{}` in {}; {offline}. gonk mounts it at its next start.",
                        ikigai_gonk::checkout::root_line(name, path),
                        config_path.display()
                    );
                }
                BrowseOp::Remove { name } => {
                    let (count, _) = ikigai_gonk::checkout::comment_out_root(&config_path, name)
                        .unwrap_or_else(|e| fail(&e));
                    println!(
                        "commented out {count} line(s) for `{name}` in {}; {offline}.",
                        config_path.display()
                    );
                }
                BrowseOp::List => fail(&offline),
            }
        }
    }
}

/// `ikigai-gonk onboard …` (ledger #2009): `init` here, everything else over the socket.
fn onboard_command(args: &ikigai_gonk::onboard::Args) {
    use ikigai_gonk::onboard::{self as onb, CliOp};
    let settings = read_settings(&args.flags);
    let homes = Homes::resolve(&args.flags).unwrap_or_else(|e| fail(&e));
    if let CliOp::Init(flags) = &args.op {
        // ★ Everything `init` does is in `onboard_init`, driven by a test with no terminal;
        // here only the Keychain mint and the terminal are chosen (ledger #2123).
        use std::io::IsTerminal;
        let (config, text) = config::read_config(&args.flags, &homes).unwrap_or_else(|e| fail(&e));
        let env = ikigai_gonk::onboard_init::Env {
            homes: homes.clone(),
            config,
            text,
            onboard: settings.onboard.clone(),
            quic: settings.quic,
            socket: settings.socket.clone(),
            host: onb::short_host_name(),
        };
        let stdin = std::io::stdin();
        let mut prompt;
        let mut defaults = ikigai_gonk::onboard_init::Defaults;
        let ask: &mut dyn ikigai_gonk::onboard_init::Ask = if stdin.is_terminal() && !flags.yes {
            prompt = ikigai_gonk::onboard_init::Prompt {
                input: stdin.lock(),
                out: std::io::stdout(),
            };
            &mut prompt
        } else {
            &mut defaults
        };
        let mut mint = |key: &str| onb::generate_key(ikigai_secret::default_backend(), None, key);
        let complete =
            ikigai_gonk::onboard_init::run(flags, &env, ask, &mut mint, &mut std::io::stdout())
                .unwrap_or_else(|e| fail(&e));
        if !complete {
            std::process::exit(1);
        }
        return;
    }
    let request = onb::cli_request(&args.op)
        .unwrap_or_else(|| fail("onboard: that entry id is not one an IRI can carry"));
    let client = ikigai_ipc::connect(&settings.socket).unwrap_or_else(|e| {
        fail(&format!(
            "cannot reach gonk's socket at {}: {e} (is gonk running with a {} line?)",
            settings.socket.display(),
            onb::KEY
        ))
    });
    let (answer, _) =
        ikigai_resolve::Resolver::issue(&client, request).unwrap_or_else(|e| fail(&e.to_string()));
    match &args.op {
        CliOp::Invite { name, out, .. } => {
            let minted: serde_json::Value = serde_json::from_slice(&answer.bytes)
                .unwrap_or_else(|e| fail(&format!("the invite's answer is not JSON: {e}")));
            let cert = out.join(format!("{name}-door.crt"));
            std::fs::write(&cert, minted["server_cert"].as_str().unwrap_or_default())
                .unwrap_or_else(|e| fail(&format!("{}: {e}", cert.display())));
            print!("{}", onb::invite_lines(&minted, &cert));
        }
        _ => print!("{}", String::from_utf8_lossy(&answer.bytes)),
    }
}

fn read_settings(flags: &config::Flags) -> config::Settings {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let (_, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    config::settings(flags, &text, &homes).unwrap_or_else(|e| fail(&e))
}

/// The roles' tokens on this server: a `--browse` role's and `--nl draft`'s, both computed
/// against this server's mount (and roots), so a grant never names a host the mount does not.
fn role_tokens(
    browse: Option<&BrowseGrant>,
    nl_draft: bool,
    settings: &config::Settings,
) -> Vec<String> {
    let mut tokens = browse_tokens(browse, settings);
    if nl_draft {
        for token in
            grants::nl_draft_grants(settings.mount_host().as_deref()).unwrap_or_else(|e| fail(&e))
        {
            if !tokens.contains(&token) {
                tokens.push(token);
            }
        }
    }
    tokens
}

/// A `--browse` role's tokens on this server, or none without one.
fn browse_tokens(browse: Option<&BrowseGrant>, settings: &config::Settings) -> Vec<String> {
    match browse {
        Some(browse) => grants::browse_role_for(settings, browse.role, &browse.roots)
            .unwrap_or_else(|e| fail(&e)),
        None => Vec::new(),
    }
}

/// What writing `scopes` under `grant` would change — and, without `--force`, the refusal
/// when it changes an existing grant at all, naming every scope it would remove and add.
///
/// ★ Asked BEFORE anything is written (a certificate bundle, an invite), so a refused
/// rewrite leaves nothing behind. The writers refuse the same way on their own; this is the
/// early copy of that check, not a replacement for it.
fn checked_change(
    layout: &quic::Layout,
    grant: &str,
    scopes: &[String],
    force: bool,
) -> quic::GrantChange {
    let change = quic::grant_change(layout, grant, scopes).unwrap_or_else(|e| fail(&e));
    if change.changes() && !force {
        fail(&change.refusal(grant, &layout.grants_json()));
    }
    change
}

/// After a forced rewrite: what it took away and what it handed out (ledger
/// [#435](http://localhost:1060/l/default/item/435)). A
/// narrowing nobody is told about is how a feature goes dark with no signal.
fn print_change(grant: &str, change: &quic::GrantChange) {
    if !change.changes() {
        return;
    }
    println!(
        "  REPLACED     grant `{grant}` (--force){} — every identity enrolled under it holds \
         the new list from its next request or connection:",
        if change.narrows() {
            ", NARROWING it"
        } else {
            ""
        }
    );
    println!("{}", change.lines());
}

/// What `client add` may replace on an existing client: its GRANT (`--force`) and, separately,
/// its IDENTITY (`--rotate`).
struct Rewrite {
    force: bool,
    rotate: bool,
}

fn client_add(
    name: &str,
    cert: Option<&std::path::Path>,
    scopes: &[String],
    rewrite: Rewrite,
    flags: &config::Flags,
) {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    // The port the printed `--connect` line names: the one `serve` would bind with this
    // config and these flags (ledger #816 — it used to be a literal 1060).
    let (_, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    let port = config::quic_addr(flags, &text)
        .unwrap_or_else(|e| fail(&e))
        .map_or(config::DEFAULT_PORT, |addr| addr.port());
    // ★ Every refusal BEFORE anything is minted (ledger #805, part 3): the bundle name, an
    // import over an existing bundle, every token no identity may hold, a rewrite of an
    // existing grant, and a certificate already enrolled elsewhere. Only then is a key pair
    // generated, so a refused command leaves no bundle and no server identity behind.
    let planned =
        quic::planned_fingerprint(&layout, name, cert, rewrite.rotate).unwrap_or_else(|e| fail(&e));
    let change = (!scopes.is_empty()).then(|| {
        quic::enrol_refusal(&layout, name, planned.as_deref(), scopes, rewrite.force)
            .unwrap_or_else(|e| fail(&e))
    });
    let bundle = quic::add_client(&layout, name, cert, rewrite.rotate).unwrap_or_else(|e| fail(&e));
    println!("client `{name}`  {}", bundle.dir.display());
    println!("  fingerprint  {}", bundle.fingerprint);
    println!("  principal    {}", quic::client_iri(&bundle.fingerprint));
    // ★ A rotation with no scopes keeps the client's AUTHORITY and replaces only its identity
    // (ledger #918): the old certificate's enrollment moves to the new one, same grant, and
    // `grants.json` is not touched. Without this the new certificate was enrolled under
    // nothing and the client was refused at its next connection.
    let mut carried = None;
    if let (true, Some(old)) = (scopes.is_empty(), &bundle.replaced) {
        carried =
            quic::rotate_enrolment(&layout, old, &bundle.fingerprint).unwrap_or_else(|e| fail(&e));
    }
    if let Some(grant) = &carried {
        println!(
            "  enrolled     under grant `{grant}`, unchanged — the old certificate's enrollment \
             moved to this one"
        );
    } else if scopes.is_empty() {
        println!("  NOT enrolled — a trusted certificate with no grant is refused. Enroll it:");
        let existing = quic::read_grants(&layout.grants_json())
            .ok()
            .and_then(|grants| grants.get(name).map(Vec::len));
        match existing {
            // A grant of this name already exists, and a fixed `--ledger` line would REWRITE
            // it (refused without --force, a narrowing with it). Say what is there instead.
            Some(n) => println!(
                "    grant `{name}` already holds {n} scope(s) in {}: re-run with the --ledger / \
                 --browse flags that grant names (a different set is refused without --force)",
                layout.grants_json().display()
            ),
            None => println!(
                "    ikigai-gonk client add {name} --ledger <ledger>=<read|audit|worker|write|delete|purge>"
            ),
        }
    } else {
        quic::enrol(&layout, name, &bundle.fingerprint, scopes, rewrite.force)
            .unwrap_or_else(|e| fail(&e));
        println!(
            "  enrolled     grant `{name}` ({} scopes) in {}",
            scopes.len(),
            layout.grants_json().display()
        );
        if let Some(change) = &change {
            print_change(name, change);
        }
    }
    // ★ A rotation says what it REPLACED (ledger #864, R7): the old certificate is no longer
    // enrolled, so the deployed bundle stops working at its next call — on purpose.
    if let Some(old) = &bundle.replaced {
        // Already moved to the new certificate when the enrollment was carried over.
        let unenrolled =
            carried.is_some() || quic::unenrol(&layout, old).unwrap_or_else(|e| fail(&e));
        println!(
            "  ROTATED      the old certificate {old} {} — a client still holding the old \
             bundle is refused from its next call; give it this one",
            if unenrolled {
                "was unenrolled"
            } else {
                "was not enrolled"
            }
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
        println!("    ikigai --connect quic://<gonk host>:{port} --cert-dir <that directory>");
    } else if bundle.key_minted {
        println!(
            "  this bundle holds the client's PRIVATE key and the server never reads it — move \
             the directory to the client, then from the client:"
        );
        println!("    ikigai --connect quic://<gonk host>:{port} --cert-dir <the moved directory>");
    } else {
        println!(
            "  the client's identity is unchanged — a client already holding this bundle needs \
             nothing new; it connects as before:"
        );
        println!("    ikigai --connect quic://<gonk host>:{port} --cert-dir <its bundle>");
    }
}

/// `ikigai-gonk client list` — every bundle and every enrolled fingerprint, joined.
fn client_list(flags: &config::Flags) {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    let (rows, default) = quic::list_clients(&layout).unwrap_or_else(|e| fail(&e));
    if rows.is_empty() {
        println!(
            "no QUIC clients: nothing under {} and no certificate enrolled in {}",
            layout.clients_dir().display(),
            layout.clients_json().display()
        );
    }
    for row in &rows {
        println!(
            "{:<16} {}  grant {:<12} {}",
            row.name.as_deref().unwrap_or("-"),
            row.fingerprint,
            match (&row.grant, &default) {
                (Some(grant), _) => grant.clone(),
                (None, Some(default)) if row.enrolled => format!("{default} (default)"),
                _ => "-".to_string(),
            },
            row.state()
        );
    }
    if let Some(default) = default {
        println!(
            "default grant `{default}`: an ENROLLED certificate whose entry names no grant is \
             served under it; a certificate with no entry is refused whatever it says"
        );
    }
}

/// `ikigai-gonk client remove <name> | --fingerprint <fp>`.
fn client_remove(key: &str, by_name: bool, flags: &config::Flags) {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    let removed = quic::remove_client(&layout, key, by_name).unwrap_or_else(|e| fail(&e));
    if let Some(dir) = &removed.bundle {
        println!("removed  {}", dir.display());
    }
    match &removed.unenrolled {
        Some(fingerprint) => println!(
            "unenrolled {fingerprint} from {} — refused from its next call",
            layout.clients_json().display()
        ),
        None => println!("  (its certificate was not enrolled)"),
    }
    println!(
        "  grants.json is unchanged: a grant name may be shared by other certificates and \
         passkeys. Restart ikigai-gonk to stop trusting the certificate at the TLS handshake"
    );
}

/// `ikigai-gonk passkey list` — every enrolled passkey (ledger #313).
fn passkey_list(flags: &config::Flags) {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    let enrolled = identity::checked_passkeys(&layout).unwrap_or_else(|e| fail(&e));
    if enrolled.is_empty() {
        println!(
            "no passkeys enrolled in {} — `ikigai-gonk passkey invite <name> …` makes one",
            layout.clients_json().display()
        );
    }
    for entry in enrolled.values() {
        println!(
            "{:<16} grant {:<12} credential {}  {}",
            entry.label,
            entry.grant,
            entry.credential_id,
            identity::passkey_iri(&entry.credential_id)
        );
    }
}

/// `ikigai-gonk passkey revoke <label> | --credential <id>` (ledger #313).
fn passkey_revoke(key: &str, by_label: bool, flags: &config::Flags) {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    let which = if by_label {
        identity::Which::Label(key)
    } else {
        identity::Which::Credential(key)
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let revoked = identity::revoke(&layout, which, now_ms).unwrap_or_else(|e| fail(&e));
    println!(
        "revoked passkey `{}` ({}, grant `{}`) from {} — refused from its next request, and its \
         live sessions are deleted",
        revoked.label,
        identity::passkey_iri(&revoked.credential_id),
        revoked.grant,
        layout.clients_json().display()
    );
    println!(
        "  recorded in {}. grants.json is unchanged: a grant name may be shared by other \
         passkeys and certificates",
        identity::audit_log(&layout).display()
    );
}

/// `ikigai-gonk passkey logout --all <label> | --credential <id>` (ledger #1824): end every
/// session of one passkey, and leave it enrolled.
fn passkey_logout_all(key: &str, by_label: bool, flags: &config::Flags) {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    let which = if by_label {
        identity::Which::Label(key)
    } else {
        identity::Which::Credential(key)
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let ended = identity::end_sessions(&layout, which, now_ms).unwrap_or_else(|e| fail(&e));
    println!(
        "every session of passkey `{}` ({}, grant `{}`) ends at the server's next signed-in \
         request or sign-in; the passkey stays enrolled and can sign in again",
        ended.label,
        identity::passkey_iri(&ended.credential_id),
        ended.grant,
    );
    println!(
        "  recorded in {}, which the running server reads",
        identity::audit_log(&layout).display()
    );
}

fn passkey_invite(
    name: &str,
    subjects: &Subjects<'_>,
    browse: Option<&BrowseGrant>,
    nl_draft: bool,
    force: bool,
    minutes: u64,
    flags: &config::Flags,
) {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let settings = read_settings(flags);
    let layout = quic::Layout::in_config_home(&homes.config);
    // ★ Ledger #1067: the link names `localhost` (the relying party), so on a bind `localhost`
    // cannot reach the invite is refused HERE, before a grant or an invite is written.
    let (base, warning) = doors::invite_base(settings.http).unwrap_or_else(|e| fail(&e));
    if subjects.is_empty() && browse.is_none() && !nl_draft {
        fail(&format!(
            "an invite needs a grant: `ikigai-gonk passkey invite {name} --ledger default=delete`, \
             `--browse read` (or `derive`) for the repositories, or `--browse-graph read` for \
             the browse graph alone"
        ));
    }
    let scopes = scopes_for(subjects, &role_tokens(browse, nl_draft, &settings));
    // ★ An identity must be STRICTLY stronger than an anonymous loopback caller, or signing
    // in would be a ceremony that changes nothing — and a grant that looked like it limited
    // someone would not.
    let anonymous =
        grants::anonymous_http_grants(&settings.http_ledgers).unwrap_or_else(|e| fail(&e));
    if scopes
        .iter()
        .all(|scope| grants::implied_by(scope, &anonymous))
    {
        fail(&format!(
            "that grant adds nothing: an anonymous loopback caller already reads {}. \
             Invite with more — `--ledger {}=delete`, or a ledger outside `gonk.http.ledger`",
            settings.http_ledgers.join(", "),
            settings
                .http_ledgers
                .first()
                .map(String::as_str)
                .unwrap_or("default")
        ));
    }
    let change = checked_change(&layout, name, &scopes, force);
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
    print_change(name, &change);
    println!("  valid for {minutes} minutes, once");
    println!("  open  {base}#invite={code}");
    if let Some(warning) = warning {
        println!("  ⚠ {warning}");
    }
    println!(
        "  in a browser on this machine (localhost, not 127.0.0.1); the passkey is enrolled in {}",
        layout.clients_json().display()
    );
}

fn fail(message: &str) -> ! {
    eprintln!("ikigai-gonk: {message}");
    std::process::exit(1);
}

/// `ikigai-gonk review request <repo> <path>` — drop one tuple and exit.
///
/// ★ **This is the whole of what a git hook runs.** No store is opened, no door is bound,
/// nothing is dialled: the tuple is written into the queue's inbox through the space's own
/// Sink (staging write, then an atomic rename, with the id blake3 of the content) and the
/// process exits. So a `post-commit` hook can call it once per changed file, in the
/// background, and the commit never waits — on this server being up, on a peer, or on a
/// model. A tuple is a REQUEST, not an authority: dropping one spends nothing and needs no
/// grant, which is exactly why the hook can be three lines and hold nothing.
///
/// The repository is NOT validated against `gonk.browse.root` here. It could be, and it is
/// not on purpose: the hook runs in a checkout, this command runs with whatever config it
/// can find, and a request for a root this server does not serve should fail where a pass
/// is run — with the kernel's own `Unresolved` naming the IRI — rather than be silently
/// dropped at the door by a second copy of the root list.
fn review_request(repo: &str, path: &str, flags: &config::Flags) {
    let homes = Homes::resolve(flags).unwrap_or_else(|e| fail(&e));
    let (_, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    let settings = config::settings(flags, &text, &homes).unwrap_or_else(|e| fail(&e));
    let Some(queue) = settings.review else {
        fail(
            "no review queue is configured: add `gonk.review.space = \"reviews\"` to the \
             config home's config.toml. Binding the queue does not arm anything — that takes \
             `gonk.review.arm = true` AND a `gonk.review.grant` grants.json can honor; see \
             the README's \"Arming it\" section",
        )
    };
    let tuple = trigger::Tuple {
        repo: repo.to_string(),
        path: path.to_string(),
    };
    let id = trigger::drop_tuple(&queue, &tuple).unwrap_or_else(|e| fail(&e));
    println!("{id}");
}

/// The reviewer's scopes when this server is armed, `None` when it is not.
///
/// ★ **`arm` without a usable grant stops this server**, rather than serving a queue nothing
/// drains while the banner says otherwise. The two facts are independent on purpose (see
/// [`config`]), so the failure of one has to be loud.
fn resolve_reviewer(queue: &trigger::Trigger, layout: &quic::Layout) -> Option<Vec<String>> {
    if !queue.arm {
        return None;
    }
    let Some(grant) = &queue.grant else {
        fail(
            "gonk.review.arm is set and gonk.review.grant names nothing. Arming is a grant \
             plus a word, never a word alone: a pass runs under an authority an operator \
             wrote into grants.json, and there is nothing here to run one under",
        )
    };
    let scopes = quic::read_grants(&layout.grants_json())
        .and_then(|grants| trigger::reviewer_scopes(&grants, grant))
        .unwrap_or_else(|e| fail(&format!("gonk.review.arm is set, and {e}")));
    Some(scopes)
}

/// The host a `urn:cap:net:` grant must name — where the mounted peer lives
/// ([`config::Settings::mount_host`]), and `localhost` with no mount.
fn mount_host(settings: &config::Settings) -> String {
    settings
        .mount_host()
        .unwrap_or_else(|| "localhost".to_string())
}

/// The banner's review line when the trigger IS armed.
///
/// ⚠ It says what bounds a push, because that is the question an operator has the moment
/// this is on: passes are serial, one per this process, and nothing bounds the wall clock.
fn armed_line(queue: &trigger::Trigger, scopes: usize, host: &str) -> String {
    format!(
        "urn:space:{} — ARMED: a drop fires {} under grant `{}` ({scopes} scopes, \
         net {host}, no urn:cap:annotate — a pass CANNOT publish). {} waiting now. Passes \
         run ONE AT A TIME in this process and nothing bounds wall clock, so a push \
         touching forty files occupies the reviewer for forty passes; watch `{}` or the \
         Queue badge. A queue that is not empty with nothing in flight is STUCK",
        queue.space,
        trigger::PASS,
        queue.grant.as_deref().unwrap_or("?"),
        trigger::pending(queue),
        trigger::DEPTH,
    )
}

/// The banner's queue line — which severities the Queue page asks a human about, and which
/// it only counts, from the contract this kernel binds (ledger #496).
///
/// ⚠ It names the words so an operator can see the gate they are running under; it is a
/// banner, and a banner is not something a review pass can read. Nothing in this crate puts
/// the gate where a pass could see it.
fn queue_line(settings: &config::Settings, declared: &[String]) -> String {
    let others = queue::other_severities(declared, &settings.queue);
    format!(
        "asks a human about {} ({}gonk.queue.serious) and any unrated finding; {} minted, \
         counted in the badge, listed under {}?{}={} — not queued",
        settings.queue.serious.join(", "),
        if settings.queue.configured {
            ""
        } else {
            "the default; "
        },
        if others.is_empty() {
            "nothing else is".to_string()
        } else {
            format!("{} are", others.join(", "))
        },
        queue::QUEUE_PATH,
        queue::SCOPE_ARG,
        queue::SCOPE_ALL,
    )
}

/// The banner's review line — the queue, what is waiting in it, and, when a grant is named,
/// whether `grants.json` can honor it.
///
/// ⚠ **It says plainly that nothing drains this** — because with `gonk.review.arm` unset
/// nothing does, and a queue filling with nobody reading it is the quietest way for this
/// feature to be useless. A person drains it one tuple at a time over the socket; the line
/// prints how, and what the word is that changes it.
fn review_line(settings: &config::Settings, layout: &quic::Layout) -> String {
    let Some(queue) = &settings.review else {
        return "not configured — no gonk.review.space; no queue is bound and \
                urn:iki:gonk:review:pass is not offered"
            .to_string();
    };
    let pending = trigger::pending(queue);
    let grant = match &queue.grant {
        None => "no gonk.review.grant named".to_string(),
        Some(name) => match quic::read_grants(&layout.grants_json())
            .and_then(|grants| trigger::reviewer_scopes(&grants, name))
        {
            Ok(scopes) => format!("grant `{name}` ({} scopes)", scopes.len()),
            Err(e) => format!("grant `{name}` UNUSABLE: {e}"),
        },
    };
    format!(
        "urn:space:{} — {pending} request(s) waiting, {grant}. NOT ARMED (no \
         gonk.review.arm = true): nothing in this process drains this queue. Run one with \
         `{}` over the socket",
        queue.space,
        trigger::DRAIN_ONE.replace("{space}", &queue.space),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ The one message in this server that is itself a procedure: an operator reading it has
    /// an invisible archive and no other instructions. So the commands are checked — that they
    /// name THIS store, THIS graph, and spell the IRI the way a shell would have to.
    #[test]
    fn the_migration_warning_names_a_command_that_would_run() {
        let graph = browse::Graph::chosen();
        let iri = graph.named().expect("a named graph").as_str().to_string();
        let text = unmigrated_warning(
            10,
            &graph,
            std::path::Path::new("/tmp/store"),
            std::path::Path::new("/tmp/gonk.sock"),
        );

        assert!(text.contains("10 quad(s)"), "{text}");
        assert!(
            text.contains(&format!(
                "migrate-annotation-ns /tmp/store --graph {iri} --commit"
            )),
            "the command an operator will paste: {text}"
        );
        assert!(
            text.contains(&format!("migrate-annotation-ns /tmp/store --graph {iri}\n")),
            "the dry run comes first: {text}"
        );
        // ⚠ `NamedNode`'s Display brackets the IRI. A `{node}` anywhere in that message
        // produces `<<urn:…>>` in the prose and `--graph <urn:…>` in the command, which the
        // migration tool refuses as not an IRI.
        assert!(
            !text.contains("<<"),
            "an IRI was printed through Display: {text}"
        );
        assert!(text.contains(&format!("--graph {iri}")), "{text}");
        assert!(text.contains(&format!("<{iri}>")), "{text}");
        // The irreversible half is never implicit — and the backup command names the socket,
        // because the backup family is bound behind it and a bare `ikigai -c` does not reach
        // this server at all.
        assert!(
            text.contains("ikigai --connect /tmp/gonk.sock -c 'source urn:iki:gonk:backup'"),
            "{text}"
        );
        assert!(text.contains("restarting does not undo it"), "{text}");

        // The rollback arm names no command, because none of these would perform it.
        let back = unmigrated_warning(
            10,
            &browse::Graph::TheDefault,
            std::path::Path::new("/tmp/store"),
            std::path::Path::new("/tmp/gonk.sock"),
        );
        assert!(!back.contains("migrate-annotation-ns"), "{back}");
        assert!(back.contains("rolled back"), "{back}");
    }

    /// The backup line's command is one an operator pastes, so it must be the form that
    /// RETURNS: through the socket this server binds (#381), not a bare `ikigai -c`.
    #[test]
    fn the_backup_line_names_a_status_command_that_reaches_this_server() {
        let line = scheduled_line(
            std::time::Duration::from_secs(86_400),
            std::path::Path::new("/var/backups/gonk"),
            7,
            "",
            std::path::Path::new("/tmp/elsewhere/gonk.sock"),
        );
        assert!(
            line.contains(
                "`ikigai --connect /tmp/elsewhere/gonk.sock -c 'source urn:iki:gonk:backup:status'`"
            ),
            "{line}"
        );
        assert!(!line.contains("`source "), "the bare form hangs: {line}");
        assert!(
            line.contains("every 24h into /var/backups/gonk — keep 7."),
            "{line}"
        );
    }

    /// The browse line names the graph with exactly one pair of brackets (#382). "Contains
    /// the IRI" alone passes on `<<urn:…>>`, so the whole bracketed form is pinned and `<<`
    /// is refused.
    #[test]
    fn the_browse_line_brackets_the_graph_once() {
        let graph = browse::Graph::chosen();
        let iri = graph.named().expect("a named graph").as_str();
        let line = browse_line(&[("demo".to_string(), "/tmp/demo".into())], &[], &graph);
        assert!(
            line.ends_with(&format!("annotations and archive in <{iri}>")),
            "{line}"
        );
        assert!(!line.contains("<<") && !line.contains(">>"), "{line}");
    }
}
