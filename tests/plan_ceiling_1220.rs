//! Ledger [#1220](http://localhost:1060/l/default/item/1220): a plan script the OWNER publishes
//! and runs (root, over the socket) whose step reads a ledger's `next` must run.
//!
//! A step's contract declares the FAMILY (`urn:cap:ledger:read:*`,
//! `urn:cap:store:read:graph:*`), never the member, and root holds no exact grant to pick one
//! from, so `ikigai-script` keeps only what the host's ceiling names. gonk's ceiling used to name
//! only the two families, and the run was refused: `capability does not grant
//! urn:cap:store:read:graph:* (declared by urn:iki:ledger:library:next)`. The ceiling now also
//! names the EXACT members for every ledger this server holds ([`ikigai_gonk::script`]'s
//! `KnownLedgers`), and stays read-only.
//!
//! Each test runs the binary in a child process over a scratch config home, data home, store and
//! socket; nothing touches a live gonk.

use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_resolve::Resolver;

mod scratch_gonk;

use scratch_gonk::Gonk;

/// One step: Source `target`, its answer the plan's.
fn one_step(name: &str, target: &str) -> String {
    format!(
        "# What the ledger offers next.\n\
         @prefix ik: <https://ikigai-rs.dev/ns#> .\n\
         <urn:plan:{name}> a ik:Process ;\n    \
             ik:step <urn:plan:{name}:step:1> ;\n    \
             ik:result <urn:plan:{name}:step:1> .\n\
         <urn:plan:{name}:step:1> a ik:Step ;\n    \
             ik:verb \"Source\" ;\n    \
             ik:resolves <{target}> .\n"
    )
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn text(answer: &Representation) -> String {
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

impl Gonk {
    fn over_socket(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }

    fn file(&self, ledger: Option<&str>, title: &str) {
        let append = match ledger {
            Some(name) => format!("urn:iki:ledger:{name}:append"),
            None => "urn:iki:ledger:append".to_string(),
        };
        self.over_socket(request(Verb::Sink, &append, &[("content", title)]))
            .unwrap_or_else(|e| panic!("append {title}: {e}"));
    }

    fn publish_plan(&self, name: &str, plan: &str) {
        self.over_socket(request(
            Verb::Sink,
            &format!("urn:script:{name}"),
            &[("language", "plan"), ("content", plan)],
        ))
        .unwrap_or_else(|e| panic!("root publishes {name}: {e}"));
    }

    fn result(&self, name: &str) -> Result<String, Error> {
        self.over_socket(request(
            Verb::Source,
            &format!("urn:script:{name}:result"),
            &[],
        ))
        .map(|answer| text(&answer))
    }
}

/// ★ The reproduction: root publishes and runs a one-step plan over a NAMED ledger's `next`.
#[test]
fn a_root_published_root_run_plan_over_a_named_ledgers_next_runs() {
    let gonk = Gonk::start("");
    gonk.file(Some("library"), "Shelve the new arrivals");
    gonk.publish_plan(
        "library-next",
        &one_step("library-next", "urn:iki:ledger:library:next"),
    );
    let answer = gonk
        .result("library-next")
        .unwrap_or_else(|e| panic!("root/root over urn:iki:ledger:library:next: {e}"));
    assert!(answer.contains("Shelve the new arrivals"), "{answer}");
}

/// The default ledger's bare form, and a ledger that comes into being AFTER the server started
/// and after the first run: the list of members is the ledgers held now, not at startup.
#[test]
fn the_default_ledger_and_a_ledger_filed_after_startup_run_too() {
    let gonk = Gonk::start("");
    gonk.file(None, "Fix the widget");
    gonk.publish_plan(
        "default-next",
        &one_step("default-next", "urn:iki:ledger:next"),
    );
    let answer = gonk.result("default-next").expect("root/root over next");
    assert!(answer.contains("Fix the widget"), "{answer}");

    gonk.publish_plan(
        "later-next",
        &one_step("later-next", "urn:iki:ledger:later:next"),
    );
    gonk.file(Some("later"), "Filed after the first run");
    let answer = gonk
        .result("later-next")
        .expect("root/root over a ledger filed after startup");
    assert!(answer.contains("Filed after the first run"), "{answer}");
}

/// ★ Still read-only: the exact members are READ grants, so a plan that writes is refused at
/// publish as before (the ceiling's own bounds are pinned by `script::ceiling_over`'s doctest).
#[test]
fn the_ceiling_stays_read_only() {
    let gonk = Gonk::start("");
    gonk.file(Some("library"), "Shelve the new arrivals");
    let writes = one_step("writes", "urn:iki:ledger:library:append").replace("Source", "Sink");
    match gonk.over_socket(request(
        Verb::Sink,
        "urn:script:writes",
        &[("language", "plan"), ("content", &writes)],
    )) {
        Err(Error::Denied(why)) => assert!(why.contains("READ-ONLY"), "{why}"),
        other => panic!("a plan that writes, published: {other:?}"),
    }
}
