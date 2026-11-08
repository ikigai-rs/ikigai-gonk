//! Ledger #3238: a name in item text links to one route, `/resolution/iri/{iri}`
//! (`urn:iki:gonk:resolution:iri:{iri}`), which decides where it goes when it is followed,
//! under the caller's capability.
//!
//! Every repository here is a HERMETIC scratch repository built in the test. The layer-entry
//! family is in `tests/layers_page_1675.rs`, beside the scratch layer host it needs.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::git_object::{self, Repository};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, doors, quic, resolution, web};
use ikigai_store::DurableStore;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Scratch",
            "-c",
            "user.email=scratch@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2026-11-07T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-11-07T12:00:00Z")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository with `src/lib.rs` at one commit: the dir, the commit, the blob.
fn scratch() -> (TempDir, String, String) {
    let tmp = tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path();
    git(dir, &["init", "-q"]);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "the first commit"]);
    let head = git(dir, &["rev-parse", "HEAD"]);
    let blob = git(dir, &["rev-parse", "HEAD:src/lib.rs"]);
    (tmp, head, blob)
}

/// The template a configured `demo` has for its working-tree files: a query with `&`, so the
/// escaping shows.
const FILE_TEMPLATE: &str = "https://code.example/blob/main/{path}?plain=1&view=code";

/// What `main` builds: `demo` a browse root over `dir`, and the configured repository map
/// `repositories`; the HTTP door's pages over the face, with `ledgers` its configured ledgers.
struct Served {
    http: Kernel,
    _watch: RootWatch,
    _config: TempDir,
}

fn served(dir: &Path, repositories: &[Repository], ledgers: &[&str]) -> Served {
    let roots = vec![("demo".to_string(), dir.to_path_buf())];
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let (watch, refused) = RootWatch::start(&roots);
    assert!(refused.is_empty(), "{refused:?}");
    let wired = browse::wire(
        roots.clone(),
        store.shared_writer().expect("the store's writer"),
        Some(&watch),
        None,
        &graph,
        None,
    );
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        vec![Arc::new(git_object::space("configured", repositories))],
        None,
        None,
        None,
    ));
    let config = tempfile::tempdir().expect("a config home");
    let layout = quic::Layout::in_config_home(config.path());
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: ledgers.iter().map(|l| l.to_string()).collect(),
        browse_roots: vec!["demo".to_string()].into(),
        passkeys: Arc::new(Passkeys::new(layout, 1062)),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        toolbox: false,
        faces: Vec::new(),
    });
    Served {
        http: doors::http_kernel(Arc::clone(&hub), web::door_space(face, None)),
        _watch: watch,
        _config: config,
    }
}

fn configured(dir: &Path, file: Option<&str>) -> Repository {
    Repository {
        name: "demo".into(),
        dir: dir.to_path_buf(),
        commit: Some("https://code.example/commit/{sha}".into()),
        blob: None,
        file: file.map(str::to_string),
        browse: true,
    }
}

/// The resolution of `iri` under `cap`: the answer's bytes, or the error.
fn resolve(served: &Served, iri: &str, cap: &Capability) -> Result<String, Error> {
    let request = Request::new(
        Verb::Source,
        Iri::parse(resolution::TEMPLATE.replace("{iri}", iri)).expect("a resolution IRI"),
    )
    .with_arg("as", ArgRef::Inline(b"text/html".to_vec()));
    block_on(Kernel::issue(&served.http, request, cap))
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
}

/// The target a resolved answer's page sends the browser on to, checked to be the SAME,
/// escaped, in the head's refresh and in the visible link.
fn target(answer: &str) -> String {
    let (head, page) = answer.split_once("\r\n\r\n").expect("an HTTP entity");
    assert!(head.contains("Content-Type: text/html"), "{head}");
    assert!(
        head.contains("Cache-Control: private"),
        "the target depends on the viewer: {head}"
    );
    let escaped = page
        .split_once("<meta http-equiv='refresh' content='0; url=")
        .and_then(|(_, rest)| rest.split_once("'>"))
        .map(|(url, _)| url.to_string())
        .unwrap_or_else(|| panic!("no refresh: {page}"));
    let (before_head, _) = page.split_once("</head>").expect("a head");
    assert!(
        before_head.contains("http-equiv='refresh'"),
        "the refresh is in the head"
    );
    assert!(
        page.contains(&format!(
            "<p>This name resolves to <a href='{escaped}'>{escaped}</a>.</p>"
        )),
        "the visible link to the same target: {page}"
    );
    escaped.replace("&amp;", "&")
}

fn ok(served: &Served, iri: &str, cap: &Capability) -> String {
    let answer = resolve(served, iri, cap).unwrap_or_else(|e| panic!("{iri}: {e}"));
    let page = answer.split_once("\r\n\r\n").unwrap().1;
    // The heading is the name as written, escaped as text.
    let shown = iri.replace('&', "&amp;").replace('\'', "&apos;");
    assert!(
        page.contains(&format!("<code class='iri'>{shown}</code>")),
        "{page}"
    );
    target(&answer)
}

/// The one 404's sentence, the name replaced, so two names' answers compare byte for byte.
fn not_found(served: &Served, iri: &str, cap: &Capability) -> String {
    match resolve(served, iri, cap) {
        Err(Error::NotFound(text)) => {
            assert!(text.contains(iri), "the name is shown: {text}");
            text.replace(iri, "NAME")
        }
        other => panic!("{iri}: expected the one 404, got {other:?}"),
    }
}

fn ledger_reader(ledger: &str) -> Vec<String> {
    ikigai_gonk::grants::grants_for(ledger, ikigai_gonk::grants::Authority::Read).unwrap()
}

#[test]
fn a_commit_and_a_blob_under_a_readable_root_resolve() {
    let (dir, head, blob) = scratch();
    let served = served(dir.path(), &[configured(dir.path(), None)], &["default"]);
    let root = Capability::root();
    // The configured clone's commit template comes first.
    assert_eq!(
        ok(&served, &format!("urn:git:commit:{head}"), &root),
        format!("https://code.example/commit/{head}")
    );
    // No blob template: the configured clone is also the browse root, so its local object view.
    assert_eq!(
        ok(&served, &format!("urn:git:blob:{blob}"), &root),
        format!(
            "/browse/{}",
            git_object::object_iri("configured", "demo", &blob)
        )
    );
    // The same grants the about links need: the root and urn:cap:exec:git.
    let reader = Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:exec:git"]);
    assert_eq!(
        ok(&served, &format!("urn:git:commit:{head}"), &reader),
        format!("https://code.example/commit/{head}")
    );
}

#[test]
fn a_file_resolves_to_browse_or_to_its_template() {
    let (dir, _, _) = scratch();
    let file = "urn:repo:file:demo/src/lib.rs";
    let template = served(
        dir.path(),
        &[configured(dir.path(), Some(FILE_TEMPLATE))],
        &["default"],
    );
    let bare = served(dir.path(), &[configured(dir.path(), None)], &["default"]);
    // A reader of the root goes to browse's current file, template or not.
    let reader = Capability::scoped(["urn:cap:browse:read:demo"]);
    for served in [&template, &bare] {
        assert_eq!(
            ok(served, file, &reader),
            "/browse/urn:repo:demo:file:src/lib.rs"
        );
    }
    // A viewer who may not read the root gets the template, escaped on the page.
    let other = Capability::scoped(ledger_reader("default"));
    assert_eq!(
        ok(&template, file, &other),
        "https://code.example/blob/main/src/lib.rs?plain=1&view=code"
    );
    let answer = resolve(&template, file, &other).unwrap();
    assert!(
        answer.contains("url=https://code.example/blob/main/src/lib.rs?plain=1&amp;view=code'"),
        "{answer}"
    );
    assert!(!answer.contains("?plain=1&view"), "{answer}");
    // Each path segment, as the IRI spells it percent-encoded, encoded once for the template.
    assert_eq!(
        ok(&template, "urn:repo:file:demo/a%20b/it's.rs", &other),
        "https://code.example/blob/main/a%20b/it%27s.rs?plain=1&view=code"
    );
    // A segment that decodes to a dot segment names no file.
    not_found(&template, "urn:repo:file:demo/%2E%2E/x", &other);
    // With no template and no grant: the one 404.
    not_found(&bare, file, &other);
    // A root that is not a browse root has no browse page, even for the all-roots grant.
    let all = Capability::scoped([ikigai_browse::CAP_WILDCARD]);
    not_found(&bare, "urn:repo:file:elsewhere/src/lib.rs", &all);
}

#[test]
fn a_ledger_item_resolves_only_when_its_ledger_is_readable() {
    let (dir, _, _) = scratch();
    let served = served(dir.path(), &[], &["default", "secret"]);
    let reader = Capability::scoped(ledger_reader("default"));
    assert_eq!(
        ok(&served, "urn:iki:ledger:default:item:3238", &reader),
        "/l/default/item/3238"
    );
    assert_eq!(
        ok(&served, "urn:iki:ledger:secret:item:1", &Capability::root()),
        "/l/secret/item/1"
    );
    // An unreadable ledger and one this server does not hold: the same answer.
    assert_eq!(
        not_found(&served, "urn:iki:ledger:secret:item:1", &reader),
        not_found(&served, "urn:iki:ledger:absent:item:1", &reader)
    );
}

#[test]
fn an_unreadable_name_and_a_nonexistent_one_answer_the_same() {
    let (dir, head, _) = scratch();
    let served = served(dir.path(), &[configured(dir.path(), None)], &["default"]);
    let reader = Capability::scoped(ledger_reader("default"));
    let unreadable = [
        format!("urn:git:commit:{head}"),
        "urn:repo:file:demo/src/lib.rs".to_string(),
        "urn:iki:ledger:other:item:1".to_string(),
    ];
    let nonexistent = [
        format!("urn:git:commit:{}", "f".repeat(40)),
        "urn:repo:file:nope/src/lib.rs".to_string(),
        "urn:isbn:0451450523".to_string(),
        "tag:ikigai.example,2026-11-07:note".to_string(),
    ];
    let mut answers = Vec::new();
    for iri in unreadable.iter().chain(nonexistent.iter()) {
        answers.push(not_found(&served, iri, &reader));
    }
    assert!(
        answers.windows(2).all(|w| w[0] == w[1]),
        "one answer: {answers:#?}"
    );
    assert_eq!(
        answers[0],
        "No resolver here knows the name NAME, or it is not readable to you."
    );
}

#[test]
fn a_github_alias_resolves_to_its_issue_page() {
    let (dir, _, _) = scratch();
    let served = served(dir.path(), &[], &["default"]);
    let anyone = Capability::scoped(Vec::<String>::new());
    assert_eq!(
        ok(&served, "gh:ikigai-rs/ikigai-gonk#319", &anyone),
        "https://github.com/ikigai-rs/ikigai-gonk/issues/319"
    );
    for bad in [
        "gh:-o/r#1",
        "gh:o/..#2",
        "gh:o/r#12x",
        "gh:o/r",
        "gh:o/r/x#1",
    ] {
        not_found(&served, bad, &anyone);
    }
}

/// ★ No name becomes a URL it carries: the route is not an open redirect.
#[test]
fn a_name_that_carries_an_absolute_url_never_becomes_the_target() {
    let (dir, _, _) = scratch();
    let served = served(
        dir.path(),
        &[configured(dir.path(), Some(FILE_TEMPLATE))],
        &["default"],
    );
    let root = Capability::root();
    for iri in [
        "https://evil.example/",
        "http://evil.example/x",
        "urn:redirect:https://evil.example/",
        "gh:evil.example/x#1",
        "urn:repo:file:demo//evil.example/x",
        "urn:repo:file:demo/../../evil.example",
        "urn:iki:ledger:default:item://evil.example",
        "urn:layer:talk:entry://evil.example",
        "tag:evil.example,2026:https://evil.example/",
    ] {
        match resolve(&served, iri, &root) {
            Err(Error::NotFound(_)) => {}
            Ok(answer) => {
                let target = target(&answer);
                assert!(
                    !target.contains("evil.example") || target.starts_with("https://code.example/"),
                    "{iri} became {target}"
                );
                assert!(!target.starts_with("//"), "{iri} became {target}");
            }
            Err(other) => panic!("{iri}: {other:?}"),
        }
    }
    // The file template keeps its own host whatever the path says.
    assert_eq!(
        ok(
            &served,
            "urn:repo:file:demo/https:/evil.example",
            &Capability::scoped(ledger_reader("default"))
        ),
        "https://code.example/blob/main/https%3A/evil.example?plain=1&view=code"
    );
}

/// The route reaches the resource for every name the lexer links, through the door's own
/// route table: the whole name is ONE encoded segment.
#[test]
fn the_route_is_one_raw_segment_carrying_the_whole_name() {
    let table = doors::edge_config(doors::HttpDoor {
        anonymous: Vec::new(),
        bind: "127.0.0.1:1062".parse().unwrap(),
        passkeys: None,
        public: Vec::new(),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    })
    .routes;
    assert!(
        table
            .routes
            .iter()
            .any(|r| r.pattern == resolution::ROUTE.0 && r.iri_template == resolution::ROUTE.1),
        "the route is in the door's table"
    );
    assert!(ikigai_gonk::doors::is_gonk_path(
        "/resolution/iri/urn%3Aisbn%3A1"
    ));
    assert!(resolution::routable("gh:o/r#7"));
    assert!(!resolution::routable("urn:repo:file:core/src/a b.rs"));
    assert!(!resolution::routable("urn:x:100%"));
}
