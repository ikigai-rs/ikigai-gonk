//! Exercise the handlers at the invocation boundary. Kernel argument contracts can
//! normalize references, supply defaults or reject malformed inputs before dispatch.

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, ContentId, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Resolution, Scope, Space, SystemClock, Verb,
};
use ikigai_gonk::{backfill, backup, git_object, identity, lapse, quic, trigger, web};
use ikigai_store::DurableStore;

const HARNESS: &str = r#"
@prefix h: <https://ikigai-rs.dev/ns/harness#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:test:control:check> a h:Control ; rdfs:label "Check" .
<urn:test:guide:check> a h:Guide ; rdfs:label "Check guide" .
<urn:test:incident:check> a h:Incident ; rdfs:label "Check incident" .
"#;

struct Fixture {
    hub: Arc<Kernel>,
    space: Fallback,
    _dir: tempfile::TempDir,
}

fn request(verb: Verb, iri: &str) -> Request {
    Request::new(verb, Iri::parse(iri).unwrap())
}

fn inline(value: &str) -> ArgRef {
    ArgRef::Inline(value.as_bytes().to_vec())
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (store, handle) = DurableStore::in_memory_shared().unwrap();
        let dataset = backup::Dataset::new(&store, handle);
        let fixtures = EndpointSpace::new().bind(
            Exact::new("urn:repo:harness-fixture:file:.gonk/harness/harness.ttl"),
            FnEndpoint::new("harness-fixture", |_| {
                Ok(Representation::new(
                    ReprType::new("text/turtle"),
                    HARNESS.as_bytes(),
                ))
            }),
        );
        let hub = Arc::new(ikigai_gonk::compose_with(
            store,
            None,
            vec![Arc::new(fixtures)],
            vec![],
            None,
            None,
            None,
        ));
        let web = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: vec!["harness-fixture".to_string()].into(),
            passkeys: Arc::new(identity::Passkeys::new(
                quic::Layout::in_config_home(dir.path()),
                1062,
            )),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            display: ikigai_gonk::display::Rules::parse(
                r#"@prefix d: <urn:iki:gonk:display#> .
                <urn:test:group> a d:Rule; d:prefix "repo:"; d:group true ."#,
            )
            .unwrap(),
            queue: Default::default(),
            epochs: None,
            toolbox: false,
            faces: vec![],
            holder_aliases: Default::default(),
            page_assets: Default::default(),
            projects: Default::default(),
            inquiry: None,
        });
        let pages = ikigai_gonk::layers_view::bind(web::space(Arc::clone(&web)), &web, None);
        let depth = EndpointSpace::new().bind(
            Exact::new(trigger::DEPTH),
            trigger::DepthEndpoint {
                trigger: None,
                activity: Arc::default(),
                armed: false,
                policy: Default::default(),
            },
        );
        let git = git_object::space(
            "test",
            &[git_object::Repository {
                name: "fixture".into(),
                dir: dir.path().into(),
                commit: None,
                blob: None,
                file: Some("https://example.org/tree/{path}".into()),
                browse: false,
            }],
        );
        let backfill = Arc::new(backfill::Backfill::new(
            Vec::<String>::new(),
            Default::default(),
            None,
            vec![],
            Arc::default(),
            None,
        ));
        let backups = backup::space(backup::Backups {
            settings: Arc::new(backup::Settings {
                dir: dir.path().join("backups"),
                keep: 1,
                every: None,
                store_path: dir.path().join("store"),
                scripts: None,
            }),
            jobs: None,
            next: Default::default(),
            dataset,
        });
        Self {
            hub,
            space: Fallback::new(vec![
                Arc::new(pages),
                Arc::new(depth),
                Arc::new(git),
                Arc::new(backups),
                Arc::new(lapse::space(lapse::Lapses::new(None, Default::default()))),
                backfill.space(),
            ]),
            _dir: dir,
        }
    }

    fn invoke(&self, request: &Request) -> Result<Representation, Error> {
        self.invoke_through(request, &self.hub)
    }

    fn invoke_through(&self, request: &Request, hub: &Kernel) -> Result<Representation, Error> {
        let Resolution::Hit(route) = self.space.resolve(request, &Scope::empty()) else {
            panic!("no route for {}", request.target);
        };
        let cap = Capability::root();
        let inv = Invocation::with_issuer(request, &route.bindings, &cap, hub)
            .with_clock(Arc::new(SystemClock));
        block_on(route.endpoint.invoke(&inv))
    }

    fn refuses_unreadable(&self, request: &Request, name: &str) {
        for (arg, wrong) in [
            (ArgRef::Inline(vec![0xff]), "UTF-8"),
            (
                ArgRef::Reference(Iri::parse("urn:test:value").unwrap()),
                "inline",
            ),
            (ArgRef::Content(ContentId::of(b"a stored body")), "content"),
        ] {
            let request = request.clone().with_arg(name, arg);
            let result = self.invoke(&request).map(|rep| rep.repr_type.media_type);
            assert!(
                matches!(&result, Err(Error::InvalidArgument { name: actual, detail })
                    if actual == name && detail.contains(wrong)),
                "{} must explain why {name} was refused ({wrong}), got {result:?}",
                request.target
            );
        }
    }
}

#[test]
fn status_resources_default_to_text_and_refuse_malformed_faces() {
    let f = Fixture::new();
    for iri in [
        backfill::BACKFILL,
        backup::STATUS,
        lapse::STATUS,
        trigger::DEPTH,
    ] {
        let req = request(Verb::Source, iri);
        assert!(f
            .invoke(&req)
            .unwrap()
            .repr_type
            .to_string()
            .starts_with("text/plain"));
        assert!(f
            .invoke(&req.clone().with_arg("as", inline("application/json")))
            .unwrap()
            .repr_type
            .to_string()
            .starts_with("application/json"));
        f.refuses_unreadable(&req, "as");
        let unsupported = f.invoke(&req.with_arg("as", inline("image/png")));
        assert!(matches!(unsupported, Err(Error::InvalidArgument { name, .. }) if name == "as"));
    }
}

#[test]
fn ledger_queue_and_facet_filters_refuse_unreadable_values() {
    let f = Fixture::new();
    for iri in [
        "urn:iki:gonk:page:home",
        "urn:iki:gonk:page:ledger:default",
        "urn:iki:gonk:fragment:items:default",
    ] {
        let req = request(Verb::Source, iri);
        assert!(f.invoke(&req).is_ok(), "{iri}");
        assert!(
            f.invoke(&req.clone().with_arg("status", inline("closed")))
                .is_ok(),
            "{iri}"
        );
        for name in [
            "as", "status", "text", "labels", "without", "limit", "offset", "ledgers", "projects",
            "priority", "sort",
        ] {
            f.refuses_unreadable(&req, name);
        }
    }
    for iri in [ikigai_gonk::queue::QUEUE_IRI, ikigai_gonk::queue::ROWS_IRI] {
        let req = request(Verb::Source, iri);
        assert!(f.invoke(&req).is_ok(), "{iri}");
        assert!(
            f.invoke(&req.clone().with_arg("severity", inline("all")))
                .is_ok(),
            "{iri}"
        );
        for name in [
            "as", "state", "repo", "limit", "severity", "group", "summary",
        ]
        .into_iter()
        .chain(ikigai_gonk::verdict::refuted_arg())
        {
            f.refuses_unreadable(&req, name);
        }
    }
    let facet = request(Verb::Source, "urn:iki:gonk:fragment:facet:ledger:default");
    assert!(f.invoke(&facet).is_ok());
    assert!(f
        .invoke(&facet.clone().with_arg("as", inline("application/json")))
        .is_ok());
    for name in [
        "as", "status", "text", "labels", "without", "limit", "ledgers", "projects", "priority",
        "sort", "page",
    ] {
        f.refuses_unreadable(&facet, name);
    }
    let group = request(
        Verb::Source,
        &format!(
            "urn:iki:gonk:page:group:ledger:default:label:{}",
            ikigai_gonk::display::group_key("repo:demo"),
        ),
    );
    assert!(f.invoke(&group).is_ok());
    assert!(f
        .invoke(&group.clone().with_arg("status", inline("open")))
        .is_ok());
    for name in ["status", "limit", "offset"] {
        f.refuses_unreadable(&group, name);
    }
}

#[test]
fn sparql_refuses_unreadable_arguments_before_choosing_query_or_face() {
    let f = Fixture::new();
    for iri in ["urn:iki:gonk:sparql", "urn:iki:gonk:fragment:sparql"] {
        let req = request(Verb::Source, iri);
        if iri.ends_with("fragment:sparql") {
            assert!(matches!(f.invoke(&req), Err(Error::MissingArgument(name)) if name == "query"));
        } else {
            assert!(f.invoke(&req).is_ok(), "{iri}");
        }
        assert!(
            f.invoke(&req.clone().with_arg("query", inline("SELECT * WHERE {}")))
                .is_ok(),
            "{iri}"
        );
        for name in ["ledger", "query", "as", "graph"] {
            f.refuses_unreadable(&req, name);
        }
    }
    let unsupported =
        f.invoke(&request(Verb::Source, "urn:iki:gonk:sparql").with_arg("as", inline("image/png")));
    assert!(matches!(unsupported, Err(Error::InvalidArgument { name, .. }) if name == "as"));
    let post = request(Verb::Sink, "urn:iki:gonk:sparql")
        .with_arg("content-type", inline("application/sparql-query"));
    f.refuses_unreadable(&post, "content");
    f.refuses_unreadable(&post, "content-type");
    assert!(f
        .invoke(&post.with_arg("content", inline("SELECT * WHERE {}")))
        .is_ok());
}

#[test]
fn agents_do_not_turn_bad_windows_into_the_default_window() {
    let f = Fixture::new();
    let agents = request(Verb::Source, ikigai_gonk::agents::AGENTS_IRI);
    assert!(f.invoke(&agents).is_ok());
    assert!(f
        .invoke(&agents.clone().with_arg("as", inline("text/csv")))
        .is_ok());
    f.refuses_unreadable(&agents, "as");
    for iri in [
        "urn:iki:gonk:agent:question:name:completed-24h",
        "urn:iki:gonk:agent:holder:alice:question:name:completed-24h",
    ] {
        let req = request(Verb::Source, iri).with_arg("ledger", inline("default"));
        assert!(f.invoke(&req).is_ok(), "{iri}");
        assert!(
            f.invoke(
                &req.clone()
                    .with_arg("since", inline("2026-11-01T00:00:00Z"))
            )
            .is_ok(),
            "{iri}"
        );
        f.refuses_unreadable(&req, "since");
        f.refuses_unreadable(&req, "as");
    }
}

#[test]
fn git_file_link_refuses_an_unreadable_path_instead_of_answering_empty() {
    let f = Fixture::new();
    let req = request(Verb::Source, &git_object::file_iri("test", "fixture"));
    assert!(f.invoke(&req).unwrap().bytes.is_empty());
    assert_eq!(
        f.invoke(&req.clone().with_arg("path", inline("src/lib.rs")))
            .unwrap()
            .bytes,
        b"https://example.org/tree/src/lib.rs"
    );
    f.refuses_unreadable(&req, "path");
}

#[test]
fn layers_count_checks_the_seen_cursor_and_other_query_arguments() {
    let f = Fixture::new();
    let req = request(Verb::Source, ikigai_gonk::layers_view::COUNT_IRI);
    assert!(f.invoke(&req).is_ok());
    assert!(f.invoke(&req.clone().with_arg("seen", inline("5"))).is_ok());
    assert!(f.invoke(&req.clone().with_arg("seen", inline("-"))).is_ok());
    for name in ["seen", "open", "quiet", "kinds"] {
        f.refuses_unreadable(&req, name);
    }
    let wrong = f.invoke(&req.with_arg("seen", inline("five")));
    assert!(matches!(wrong, Err(Error::InvalidArgument { name, detail })
        if name == "seen" && detail.contains("non-negative integer")));
}

#[test]
fn rules_and_passkey_endpoints_distinguish_an_unreadable_arg_from_no_arg() {
    let f = Fixture::new();
    let rules = request(Verb::Source, ikigai_gonk::rules::RULES_IRI);
    assert!(f.invoke(&rules).is_ok());
    assert!(f
        .invoke(&rules.clone().with_arg("as", inline("text/plain")))
        .is_ok());
    f.refuses_unreadable(&rules, "as");

    let session = request(Verb::Source, "urn:iki:gonk:passkey:session");
    assert!(f.invoke(&session).is_ok());
    assert!(f
        .invoke(&session.clone().with_arg("content", inline("")))
        .is_ok());
    f.refuses_unreadable(&session, "content");

    let register = request(Verb::Sink, "urn:iki:gonk:passkey:register-options");
    assert!(f.invoke(&register).is_ok());
    assert!(f
        .invoke(&register.clone().with_arg("content", inline("{}")))
        .is_ok());
    f.refuses_unreadable(&register, "content");
    for value in ["{", r#"{"label": 42}"#] {
        let result = f.invoke(&register.clone().with_arg("content", inline(value)));
        assert!(
            matches!(result, Err(Error::InvalidArgument { name, detail })
            if name == "content" && detail.contains("JSON"))
        );
    }
}

#[test]
fn k_sink_does_not_drop_a_body_it_cannot_read_inline() {
    let f = Fixture::new();
    let req =
        request(Verb::Sink, ikigai_gonk::k::K_IRI).with_arg("c", inline("sink urn:iki:annotation"));
    f.refuses_unreadable(&req, "content");
    f.refuses_unreadable(&req, "content-type");

    // The downstream endpoint echoes what the adapter forwards.
    use ikigai_core::{ActionSpec, ArgSpec, Description};
    let annotation = FnEndpoint::new("annotation", |inv| {
        let bytes = inv.inline_arg("content").unwrap_or_default().to_vec();
        Ok(Representation::new(ReprType::new("text/plain"), bytes))
    })
    .with_description(
        Description::new("annotation")
            .action(ActionSpec::new(Verb::Sink).input(ArgSpec::new("content").optional())),
    );
    let hub = Kernel::new(Arc::new(
        EndpointSpace::new().bind(Exact::new("urn:iki:annotation"), annotation),
    ));
    assert!(f.invoke_through(&req, &hub).unwrap().bytes.is_empty());
    let req = req.with_arg("content", inline("a note"));
    assert_eq!(f.invoke_through(&req, &hub).unwrap().bytes, b"a note");
}

#[test]
fn harness_resources_do_not_default_unreadable_roots_filters_or_faces() {
    let f = Fixture::new();
    for iri in [
        "urn:iki:harness:control:id:check",
        "urn:iki:harness:guide:id:check",
        "urn:iki:harness:incident:id:check",
        "urn:iki:harness:coverage:query:firings-by-day",
        "urn:iki:harness:firing:date:2026-11-01",
        "urn:repo:harness-fixture:description",
    ] {
        let req = request(Verb::Source, iri);
        let absent = f.invoke(&req);
        assert!(absent.is_ok(), "{iri}: {absent:?}");
        let valid = f.invoke(&req.clone().with_arg("as", inline("application/json")));
        assert!(valid.is_ok(), "{iri}: {valid:?}");
        f.refuses_unreadable(&req, "as");
        if !iri.ends_with(":description") {
            f.refuses_unreadable(&req, "root");
        }
    }
    let page = request(Verb::Source, ikigai_gonk::harness::PAGE_IRI);
    let absent = f.invoke(&page);
    assert!(absent.is_ok(), "{absent:?}");
    let chosen = page
        .clone()
        .with_arg("root", inline("harness-fixture"))
        .with_arg("q", inline("check"));
    assert!(f.invoke(&chosen).is_ok());
    for name in ["root", "q", "direction"] {
        f.refuses_unreadable(&page, name);
    }
}

#[test]
fn form_adapter_does_not_drop_unreadable_transport_principal() {
    let f = Fixture::new();
    let req = request(Verb::Sink, "urn:iki:gonk:act").with_arg(
        "content",
        inline("_ledger=default&_action=append&content=Sample"),
    );
    f.refuses_unreadable(&req, ikigai_gonk::admit::PRINCIPAL_ARG);
    assert!(f.invoke(&req).is_ok());
    assert!(f
        .invoke(&req.with_arg(
            ikigai_gonk::admit::PRINCIPAL_ARG,
            inline("urn:test:principal"),
        ))
        .is_ok());
}
