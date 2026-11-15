//! Ledger [#3678](http://localhost:1062/l/default/item/3678): the Dashboard's Agents panel has a
//! deadline, and past it draws what is done with a sentence saying the rest did not finish.
//!
//! ⚠ A binary of its own: [`ikigai_gonk::agents::deadline_passes_after`] is process-wide, so the
//! tests here run one after another under a lock.

use std::sync::{Arc, Mutex};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::agents::deadline_passes_after;
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::DurableStore;

/// The hook is the process's: one test at a time.
static HOOK: Mutex<()> = Mutex::new(());

struct Server {
    hub: Arc<Kernel>,
    http: Arc<Kernel>,
    _config: tempfile::TempDir,
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

impl Server {
    fn new() -> Server {
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new().into(),
            passkeys: Arc::new(Passkeys::new(layout, 1062)),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            holder_aliases: Default::default(),
            page_assets: Default::default(),
            projects: Default::default(),
            toolbox: false,
            faces: Vec::new(),
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        Server {
            hub,
            http,
            _config: config,
        }
    }

    fn act(&self, action: &str, args: &[(&str, &str)]) {
        let iri = format!("{}{action}", Ledger::parse("default").unwrap().prefix());
        futures::executor::block_on(Kernel::issue(
            &self.hub,
            request(Verb::Sink, &iri, args),
            &Capability::root(),
        ))
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
    }

    /// The Agents tab's panel for `default`, on a stack the size the door gives a request.
    fn panel(&self) -> String {
        let names = vec!["default".to_string()];
        let reader = Capability::scoped(grants_for_all(&names, Authority::Read).unwrap());
        let http = Arc::clone(&self.http);
        let read = request(Verb::Source, "urn:iki:gonk:page:dashboard:agents", &[]);
        let bytes = std::thread::Builder::new()
            .stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
            .spawn(move || futures::executor::block_on(http.issue(read, &reader)))
            .unwrap()
            .join()
            .unwrap()
            .expect("the Dashboard renders")
            .bytes;
        // The writer quotes attributes singly; the names here have no quote of their own.
        let page = String::from_utf8(bytes).unwrap().replace('\'', "\"");
        let start = page
            .find("id=\"agents-default-title\"")
            .unwrap_or_else(|| panic!("no panel: {page}"));
        let rest = &page[start..];
        rest[..rest.find("</section>").unwrap()].to_string()
    }
}

/// A ledger with two agents, each holding an item.
fn two_agents() -> Server {
    let server = Server::new();
    for (n, holder) in ["ann", "bob"].iter().enumerate() {
        server.act("append", &[("content", "sample item")]);
        server.act(
            "claim",
            &[("item", &(n + 1).to_string()), ("content", holder)],
        );
    }
    server
}

/// The visible numbers of `holder`'s row.
fn shown(panel: &str, holder: &str) -> Vec<String> {
    let row = panel
        .split("<tr>")
        .find(|row| row.starts_with(&format!("<th scope=\"row\">{holder}</th>")))
        .unwrap_or_else(|| panic!("no row for {holder}: {panel}"));
    row.split("<td>")
        .skip(1)
        .map(|cell| {
            let after = cell.split("</span>").nth(1).unwrap();
            after.split("<span").next().unwrap().to_string()
        })
        .collect()
}

/// ★ Past its deadline the panel is drawn with what finished, and a sentence says the rest did not.
/// The hook stops it after the list of agents, "In flight" and "Worked on": those numbers are
/// there, the other columns are unavailable, and the panel says why, never a spinner.
#[test]
fn a_deadline_draws_a_partial_panel_with_its_sentence() {
    let _one = HOOK.lock().unwrap_or_else(|e| e.into_inner());
    let server = two_agents();
    deadline_passes_after(3);
    let panel = server.panel();
    deadline_passes_after(u64::MAX);
    let numbers = shown(&panel, "ann");
    assert_eq!(&numbers[..2], ["1", "1"], "{panel}");
    assert!(
        numbers[2..].iter().all(|n| n == "unavailable"),
        "{numbers:?}"
    );
    assert!(
        panel.contains("<p class=\"note\">Some numbers did not finish in time"),
        "{panel}"
    );
    assert!(panel.contains(">bob</th>"), "{panel}");
    assert!(panel.contains("All agents"), "{panel}");

    // With time to finish, the same panel is whole and says nothing of a deadline.
    let panel = server.panel();
    assert!(!panel.contains("did not finish"), "{panel}");
    assert!(
        shown(&panel, "ann").iter().all(|n| n != "unavailable"),
        "{panel}"
    );
}

/// A deadline that passes before even the list of agents is read still draws the panel, saying so.
#[test]
fn a_deadline_before_the_agents_draws_a_sentence() {
    let _one = HOOK.lock().unwrap_or_else(|e| e.into_inner());
    let server = two_agents();
    deadline_passes_after(0);
    let panel = server.panel();
    deadline_passes_after(u64::MAX);
    assert!(
        panel.contains("The agents did not finish in time; the next refresh asks again."),
        "{panel}"
    );
}
