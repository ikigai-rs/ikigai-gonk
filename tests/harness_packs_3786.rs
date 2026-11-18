//! Ledger #3786: harness packs, applied to browse roots by the shape of their description (and by
//! explicit adoption), with firings read from a directory outside any tree.
//!
//! Four scratch roots and a scratch config home:
//! - `alpha`: on GitHub as `ikigai-rs/alpha`, a `Cargo.toml` and `ACCESSIBILITY.md`, declared a
//!   Linux library for the public; it also holds the pack's file.
//! - `beta`: on GitHub as `ikigai-rs/beta`, declared a Windows web app for internal use.
//! - `gamma`: not on GitHub, no harness of its own.
//! - `delta`: not on GitHub, its own harness adopts the pack.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::packs::Config;
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_store::DurableStore;
use serde_json::Value;
use tempfile::TempDir;

const OPERATOR: &str = r#"@prefix browse: <https://ikigai-rs.dev/ns/browse#> .
@prefix skos:   <http://www.w3.org/2004/02/skos/core#> .
@prefix foaf:   <http://xmlns.com/foaf/0.1/> .
@prefix rdfs:   <http://www.w3.org/2000/01/rdf-schema#> .
@prefix os:     <urn:iki:os:> .
@prefix class:  <urn:iki:class:> .
@prefix type:   <urn:iki:type:> .
@prefix cust:   <urn:iki:customer:> .

<urn:iki:team:core> a foaf:Group ; rdfs:label "Core" .
<urn:iki:team:web>  a foaf:Group ; rdfs:label "Web" .

<urn:iki:scheme:class> a skos:ConceptScheme .
class:module a skos:Concept ; skos:inScheme <urn:iki:scheme:class> ; skos:prefLabel "module" .
class:site   a skos:Concept ; skos:inScheme <urn:iki:scheme:class> ; skos:prefLabel "site" .

<urn:iki:scheme:os> a skos:ConceptScheme .
os:unix    a skos:Concept ; skos:inScheme <urn:iki:scheme:os> ; skos:prefLabel "Unix" .
os:linux   a skos:Concept ; skos:inScheme <urn:iki:scheme:os> ; skos:prefLabel "Linux" ; skos:broader os:unix .
os:macos   a skos:Concept ; skos:inScheme <urn:iki:scheme:os> ; skos:prefLabel "macOS" ; skos:broader os:unix .
os:windows a skos:Concept ; skos:inScheme <urn:iki:scheme:os> ; skos:prefLabel "Windows" .

<urn:iki:scheme:type> a skos:ConceptScheme .
type:library a skos:Concept ; skos:inScheme <urn:iki:scheme:type> ; skos:prefLabel "library" .
type:web-app a skos:Concept ; skos:inScheme <urn:iki:scheme:type> ; skos:prefLabel "web app" .

<urn:iki:scheme:customer> a skos:ConceptScheme .
cust:public   a skos:Concept ; skos:inScheme <urn:iki:scheme:customer> ; skos:prefLabel "public" .
cust:internal a skos:Concept ; skos:inScheme <urn:iki:scheme:customer> ; skos:prefLabel "internal" .

browse:rootClass      browse:valuesFrom <urn:iki:scheme:class> .
browse:osTarget       browse:valuesFrom <urn:iki:scheme:os> .
browse:targetType     browse:valuesFrom <urn:iki:scheme:type> .
browse:targetCustomer browse:valuesFrom <urn:iki:scheme:customer> .

<urn:repo:alpha:description>
    browse:team <urn:iki:team:core> ; browse:rootClass class:module ;
    browse:osTarget os:linux ; browse:targetType type:library ; browse:targetCustomer cust:public .

<urn:repo:beta:description>
    browse:team <urn:iki:team:web> ; browse:rootClass class:site ;
    browse:osTarget os:windows ; browse:targetType type:web-app ; browse:targetCustomer cust:internal .
"#;

const PACK: &str = r#"@prefix harness: <https://ikigai-rs.dev/ns/harness#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl:     <http://www.w3.org/2002/07/owl#> .
@prefix sh:      <http://www.w3.org/ns/shacl#> .
@prefix browse:  <https://ikigai-rs.dev/ns/browse#> .
@prefix ctl:     <urn:iki:harness:control:> .
@prefix guide:   <urn:iki:harness:guide:> .

<urn:iki:harness:pack:name:iki> a harness:Pack ;
    rdfs:label "iki rules" ; owl:versionInfo "1.4" ;
    harness:appliesTo <urn:iki:harness:shape:ikigai-rs> .

<urn:iki:harness:shape:ikigai-rs> a sh:NodeShape ;
    rdfs:label "are owned by ikigai-rs on GitHub" ;
    sh:property [ sh:path browse:githubOwner ; sh:hasValue "ikigai-rs" ] .

<urn:iki:harness:shape:rust> a sh:NodeShape ;
    rdfs:label "hold a Cargo.toml" ;
    sh:property [ sh:path browse:topLevelFile ; sh:hasValue "Cargo.toml" ] .

<urn:iki:harness:shape:unix> a sh:NodeShape ;
    rdfs:label "target a Unix" ;
    sh:property [ sh:path browse:osTarget ; sh:hasValue <urn:iki:os:unix> ] .

guide:one-gate a harness:Guide ; rdfs:label "one gate per command" .

ctl:newline-is-not-a-gate a harness:Control ;
    rdfs:label "hook: a newline is not a gate" ;
    harness:direction harness:feedback ; harness:outcome harness:warn ;
    harness:statedBy guide:one-gate .

ctl:rust-ci a harness:Control ;
    rdfs:label "gate: Rust CI is green" ;
    harness:direction harness:feedback ; harness:outcome harness:refuse ;
    harness:appliesTo <urn:iki:harness:shape:rust> .

ctl:unix-paths a harness:Control ;
    rdfs:label "gate: Unix paths only" ;
    harness:direction harness:feedback ; harness:outcome harness:refuse ;
    harness:appliesTo <urn:iki:harness:shape:unix> .
"#;

/// A pack whose shape the subset validator refuses (`sh:or`).
const REFUSED_PACK: &str = r#"@prefix harness: <https://ikigai-rs.dev/ns/harness#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
@prefix sh:      <http://www.w3.org/ns/shacl#> .
@prefix browse:  <https://ikigai-rs.dev/ns/browse#> .

<urn:iki:harness:pack:name:odd> a harness:Pack ; rdfs:label "odd rules" ;
    harness:appliesTo <urn:odd:shape> .
<urn:odd:shape> a sh:NodeShape ;
    sh:or ( [ sh:path browse:rootName ; sh:hasValue "alpha" ] [ sh:path browse:rootName ; sh:hasValue "beta" ] ) .
<urn:odd:control:x> a harness:Control ; rdfs:label "odd" .
"#;

const DELTA_HARNESS: &str = r#"@prefix harness: <https://ikigai-rs.dev/ns/harness#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
<> harness:adopts <urn:iki:harness:pack:name:iki>, <urn:iki:harness:pack:name:nobody> .
<urn:delta:control:own> a harness:Control ; rdfs:label "delta's own control" ;
    harness:direction harness:feedback .
"#;

/// A firing line in the starter's form.
fn line(at: &str, class: &str, id: &str, seq: u32, session: &str, repo: &str) -> String {
    format!(
        "{at} harness:{class} urn:iki:harness:control:{id} seq={seq} \
         session=urn:agents:session:{session} tool=Bash repo={repo} cmd=sha256:abc{seq}\n"
    )
}

fn segment(name: &str, body: &str) -> String {
    format!(
        "# ikigai-log v1\n@prefix harness: <https://ikigai-rs.dev/ns/harness#> .\n\
         @prefix log:     <https://ikigai-rs.dev/ns/log#> .\n@name     urn:log:harness:{name}\n\
         @instance urn:iki:harness:{name}\n@level    log:info\n@started  2026-10-30T00:00:00.000Z\n\
         @prev     genesis\n\n{body}"
    )
}

struct Scene {
    _dirs: Vec<TempDir>,
    roots: Vec<(String, PathBuf)>,
    config: TempDir,
    firings: PathBuf,
}

fn write(root: &Path, path: &str, text: &str) {
    let at = root.join(path);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(at, text).unwrap();
}

fn scene() -> Scene {
    let mut dirs = Vec::new();
    let mut roots = Vec::new();
    for name in ["alpha", "beta", "gamma", "delta"] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "README.md", "# a root\n");
        roots.push((name.to_string(), dir.path().to_path_buf()));
        dirs.push(dir);
    }
    let alpha = &roots[0].1;
    write(alpha, "Cargo.toml", "[package]\nname = \"alpha\"\n");
    write(alpha, "ACCESSIBILITY.md", "# Accessibility\n");
    write(alpha, "claude/harness/harness.ttl", PACK);
    write(alpha, "claude/harness/odd.ttl", REFUSED_PACK);
    // The repository's own declared facts: a customer the operator's file overrides, and a
    // second OS target, which is not single-valued and so is kept.
    write(
        alpha,
        ".gonk/root.ttl",
        "@prefix browse: <https://ikigai-rs.dev/ns/browse#> .\n\
         <> browse:targetCustomer <urn:iki:customer:internal> ; browse:osTarget <urn:iki:os:macos> .\n",
    );
    write(&roots[1].1, "package.json", "{}\n");
    write(&roots[2].1, "pyproject.toml", "[project]\n");
    write(&roots[3].1, ".gonk/harness/harness.ttl", DELTA_HARNESS);
    let config = tempfile::tempdir().unwrap();
    write(config.path(), "roots.ttl", OPERATOR);
    let firings = config.path().join("firings");
    std::fs::create_dir_all(&firings).unwrap();
    let s1 = line(
        "2026-10-30T14:46:09.417Z",
        "Warning",
        "newline-is-not-a-gate",
        1,
        "s1",
        "alpha",
    ) + &line(
        "2026-10-30T14:47:09.417Z",
        "Warning",
        "newline-is-not-a-gate",
        2,
        "s1",
        "beta",
    ) + &line(
        "2026-10-30T14:48:09.417Z",
        "Refusal",
        "rust-ci",
        3,
        "s1",
        "-",
    );
    let s1_rotated = line(
        "2026-10-31T09:00:00.000Z",
        "Refusal",
        "rust-ci",
        4,
        "s1",
        "alpha",
    ) + &line(
        "2026-10-31T09:01:00.000Z",
        "Warning",
        "newline-is-not-a-gate",
        5,
        "s1",
        "alpha",
    );
    let s2 = line(
        "2026-10-31T10:00:00.000Z",
        "Warning",
        "newline-is-not-a-gate",
        1,
        "s2",
        "beta",
    ) + &line(
        "2026-10-31T10:01:00.000Z",
        "Warning",
        "newline-is-not-a-gate",
        2,
        "s2",
        "-",
    ) + &line(
        "2026-10-31T10:02:00.000Z",
        "Warning",
        "newline-is-not-a-gate",
        3,
        "s2",
        "elsewhere",
    );
    std::fs::write(firings.join("s1.log"), segment("s1", &s1)).unwrap();
    std::fs::write(firings.join("s1.1.log"), segment("s1-1", &s1_rotated)).unwrap();
    std::fs::write(firings.join("s2.log"), segment("s2", &s2)).unwrap();
    // Not read: a hidden name, another extension, a subdirectory.
    std::fs::write(firings.join(".s3.log"), segment("s3", &s2)).unwrap();
    std::fs::write(firings.join("notes.txt"), "not a segment").unwrap();
    std::fs::create_dir_all(firings.join("old")).unwrap();
    std::fs::write(firings.join("old/s4.log"), segment("s4", &s2)).unwrap();
    Scene {
        _dirs: dirs,
        roots,
        config,
        firings,
    }
}

fn github() -> Vec<(String, String)> {
    vec![
        ("alpha".into(), "https://github.com/ikigai-rs/alpha".into()),
        ("beta".into(), "https://github.com/ikigai-rs/beta".into()),
    ]
}

impl Scene {
    fn config(&self, packs: &[&str]) -> Result<Config, String> {
        Config::parse(
            &[self.config.path().join("roots.ttl").display().to_string()],
            &packs.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            &[format!("iki {}", self.firings.display())],
            &self.roots,
            &github(),
            self.config.path(),
        )
    }
}

struct Served {
    http: Arc<Kernel>,
    _watch: RootWatch,
}

fn served(scene: &Scene, packs: Config) -> Served {
    let roots = scene.roots.clone();
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes()).unwrap();
    let (watch, refused) = RootWatch::start(&roots);
    assert!(refused.is_empty(), "{refused:?}");
    let wired = browse::wire(
        roots.clone(),
        store.shared_writer().unwrap(),
        Some(&watch),
        None,
        &graph,
        None,
    );
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
    ));
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let names: Vec<String> = roots.iter().map(|(n, _)| n.clone()).collect();
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: names.into(),
        passkeys: Arc::new(Passkeys::new(layout, 1062)),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        display: Default::default(),
        page_assets: Default::default(),
        projects: Default::default(),
        toolbox: false,
        faces: Vec::new(),
        inquiry: None,
    });
    Served {
        http: Arc::new(doors::http_kernel(
            Arc::clone(&hub),
            web::space_with(face, Arc::new(packs)),
        )),
        _watch: watch,
    }
}

impl Served {
    fn read(
        &self,
        iri: &str,
        root: Option<&str>,
        face: &str,
        cap: &Capability,
    ) -> Result<String, Error> {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap())
            .with_arg("as", ArgRef::Inline(face.as_bytes().to_vec()));
        if let Some(root) = root {
            request = request.with_arg("root", ArgRef::Inline(root.as_bytes().to_vec()));
        }
        let (http, cap) = (&*self.http, cap.clone());
        std::thread::scope(|s| {
            std::thread::Builder::new()
                .stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
                .spawn_scoped(s, move || block_on(Kernel::issue(http, request, &cap)))
                .unwrap()
                .join()
                .unwrap()
        })
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
    }

    fn page(&self, root: &str, cap: &Capability) -> String {
        self.read("urn:iki:gonk:page:harness", Some(root), "text/html", cap)
            .unwrap()
            .replace('\'', "\"")
    }

    fn json(&self, iri: &str, root: Option<&str>, cap: &Capability) -> Value {
        let text = self.read(iri, root, "application/json", cap).unwrap();
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
    }
}

/// Reads these roots and the ledger.
fn reading(roots: &[&str]) -> Capability {
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    for root in roots {
        scopes.push(format!("{}{root}", ikigai_browse::CAP_PREFIX));
    }
    Capability::scoped(scopes)
}

fn everyone() -> Capability {
    reading(&["alpha", "beta", "gamma", "delta"])
}

const IKI: &str = "iki alpha:claude/harness/harness.ttl";

/// ★ A description carries the derived facts and the declared ones, each concept's broader
/// ancestors materialized, the operator's value kept over the repository's, and never a path.
#[test]
fn a_root_description_carries_derived_and_declared_facts_and_no_path() {
    let scene = scene();
    let s = served(&scene, scene.config(&[IKI]).unwrap());
    let d = s.json("urn:repo:alpha:description", None, &everyone());
    assert_eq!(d["iri"], "urn:repo:alpha:description", "{d}");
    assert_eq!(d["derived"]["name"], "alpha");
    assert_eq!(d["derived"]["github"]["owner"], "ikigai-rs", "{d}");
    assert_eq!(d["derived"]["github"]["repository"], "alpha", "{d}");
    let files: Vec<&str> = d["derived"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert_eq!(files, ["Cargo.toml", "ACCESSIBILITY.md", ".gonk/"], "{d}");
    let declared = &d["declared"];
    assert_eq!(declared["team"][0], "urn:iki:team:core", "{d}");
    assert_eq!(declared["rootClass"][0], "urn:iki:class:module", "{d}");
    assert_eq!(declared["targetType"][0], "urn:iki:type:library", "{d}");
    // The operator's customer wins over the repository's, and the description says so.
    assert_eq!(
        declared["targetCustomer"],
        serde_json::json!(["urn:iki:customer:public"]),
        "{d}"
    );
    let note = d["notes"][0].as_str().unwrap();
    assert!(
        note.contains("The operator's file gives browse:targetCustomer"),
        "{note}"
    );
    assert!(
        note.contains("<urn:iki:customer:internal>, which is set aside"),
        "{note}"
    );
    // Two OS targets, the operator's and the repository's, and their ancestor.
    let mut os: Vec<&str> = declared["osTarget"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    os.sort();
    assert_eq!(os, ["urn:iki:os:linux", "urn:iki:os:macos"], "{d}");
    assert_eq!(
        d["under"]["osTarget"],
        serde_json::json!(["urn:iki:os:unix"]),
        "{d}"
    );
    let turtle = s
        .read(
            "urn:repo:alpha:description",
            None,
            "text/turtle",
            &everyone(),
        )
        .unwrap();
    for needle in [
        "browse:githubOwner \"ikigai-rs\"",
        "browse:topLevelFile",
        "<urn:iki:os:unix>",
        "skos:broader <urn:iki:os:unix>",
        "browse:override",
    ] {
        assert!(turtle.contains(needle), "`{needle}` is not in:\n{turtle}");
    }
    for (_, dir) in &scene.roots {
        let path = dir.display().to_string();
        assert!(
            !turtle.contains(&path) && !d.to_string().contains(&path),
            "a path: {turtle}"
        );
    }
    // A root with nothing declared still has its derived facts.
    let gamma = s.json("urn:repo:gamma:description", None, &everyone());
    assert_eq!(
        gamma["derived"]["files"],
        serde_json::json!(["pyproject.toml"]),
        "{gamma}"
    );
    assert!(gamma["derived"]["github"].is_null(), "{gamma}");
}

/// ★ A declared value that names no concept of its scheme, a file that breaks the closed shape,
/// and every bad config line, are refused at start, in words.
#[test]
fn bad_declared_facts_and_bad_config_lines_are_refused_at_start() {
    let scene = scene();
    let cases = [
        (
            OPERATOR.replace("browse:osTarget os:linux", "browse:osTarget os:plan9"),
            "gives browse:osTarget the value <urn:iki:os:plan9>, which names no concept of the scheme <urn:iki:scheme:os>",
        ),
        (
            OPERATOR.replace(
                "browse:targetType type:library",
                "browse:targetType type:library, type:web-app",
            ),
            "gives more than one target type; a root has one",
        ),
        (
            OPERATOR.replace(
                "browse:team <urn:iki:team:core> ;",
                "browse:team <urn:iki:team:core> ; browse:mascot <urn:iki:mascot:crab> ;",
            ),
            "gives browse:mascot, which is not a property a root description takes",
        ),
        (
            OPERATOR.replace("browse:team <urn:iki:team:core>", "browse:team <urn:iki:os:unix>"),
            "which is not a foaf:Group",
        ),
        (
            OPERATOR.replace("<urn:repo:beta:description>", "<urn:repo:omega:description>"),
            "it describes `omega`",
        ),
    ];
    for (text, needle) in cases {
        write(scene.config.path(), "roots.ttl", &text);
        let err = scene.config(&[IKI]).unwrap_err();
        assert!(err.starts_with("gonk.browse.description = \""), "{err}");
        assert!(err.contains(needle), "`{needle}` is not in: {err}");
    }
    write(scene.config.path(), "roots.ttl", OPERATOR);
    assert!(scene.config(&[IKI]).is_ok());
    for (pack, needle) in [
        (
            "iki omega:claude/harness/harness.ttl",
            "`omega` is not a root this server browses",
        ),
        (
            "iki alpha:claude/none.ttl",
            "the root `alpha` has no file `claude/none.ttl`",
        ),
        ("iki alpha:../x.ttl", "is not a path inside the root"),
        ("Iki alpha:claude/harness/harness.ttl", "is not a pack name"),
        ("iki", "is not `<pack> <root>:<path>`"),
    ] {
        let err = scene.config(&[pack]).unwrap_err();
        assert!(err.contains(needle), "{pack}: `{needle}` is not in: {err}");
    }
    let unbound = Config::parse(
        &[],
        &[],
        &[format!("iki {}", scene.firings.display())],
        &scene.roots,
        &[],
        scene.config.path(),
    )
    .unwrap_err();
    assert!(
        unbound.contains("no gonk.harness.pack line binds the pack `iki`"),
        "{unbound}"
    );
    let missing = Config::parse(
        &[],
        &[IKI.to_string()],
        &["iki /no/such/directory".to_string()],
        &scene.roots,
        &[],
        scene.config.path(),
    )
    .unwrap_err();
    assert!(missing.contains("the directory is not there"), "{missing}");
    // The same lines, through the config parser `serve` uses: the keys are known.
    let homes = ikigai_gonk::config::Homes {
        home: scene.config.path().to_path_buf(),
        config: scene.config.path().to_path_buf(),
        data: scene.config.path().join("data"),
    };
    let text = format!(
        "gonk.browse.root = \"alpha={}\"\ngonk.harness.pack = \"iki alpha:claude/nothing.ttl\"\n",
        scene.roots[0].1.display()
    );
    let err = ikigai_gonk::config::settings(&Default::default(), &text, &homes)
        .err()
        .unwrap();
    assert!(err.contains("has no file `claude/nothing.ttl`"), "{err}");
}

/// ★ The pack's shape (GitHub owner `ikigai-rs`) matches alpha and beta and not gamma; a rule's
/// own narrower shape keeps it to the roots that also match that (a Cargo.toml; any Unix OS
/// target, which alpha declares as Linux); each rule says its pack and why it applies.
#[test]
fn a_pack_applies_by_shape_and_a_rule_narrows_further() {
    let scene = scene();
    let s = served(&scene, scene.config(&[IKI]).unwrap());
    let alpha = s.page("alpha", &everyone());
    for needle in [
        "<h2 id=\"harness-packs\">Packs</h2>",
        "<li>\u{201c}iki rules\u{201d} 1.4<p class=\"harness-facts\">applies to roots that are owned by ikigai-rs on GitHub",
        "hook: a newline is not a gate",
        "From the pack \u{201c}iki rules\u{201d} 1.4, which applies to roots that are owned by ikigai-rs on GitHub",
        "gate: Rust CI is green",
        "From the pack \u{201c}iki rules\u{201d} 1.4, which applies to roots that hold a Cargo.toml",
        "gate: Unix paths only",
        "which applies to roots that target a Unix",
        "<h3 id=\"harness-guide-one-gate\">",
    ] {
        assert!(alpha.contains(needle), "`{needle}` is not on alpha's page:\n{alpha}");
    }
    let beta = s.page("beta", &everyone());
    assert!(beta.contains("hook: a newline is not a gate"), "{beta}");
    for absent in ["gate: Rust CI is green", "gate: Unix paths only"] {
        assert!(
            !beta.contains(absent),
            "`{absent}` is on beta's page:\n{beta}"
        );
    }
    // gamma has no harness of its own and no pack reaches it.
    let gamma = s.page("gamma", &everyone());
    assert!(!gamma.contains("iki rules"), "{gamma}");
    assert!(gamma.contains("carries a harness"), "{gamma}");
    // The rule's resource names its pack.
    let control = s.json(
        "urn:iki:harness:control:id:rust-ci",
        Some("alpha"),
        &everyone(),
    );
    assert_eq!(control["pack"]["name"], "iki", "{control}");
    assert_eq!(control["pack"]["version"], "1.4", "{control}");
    assert_eq!(
        control["pack"]["appliesTo"], "hold a Cargo.toml",
        "{control}"
    );
    assert!(s
        .read(
            "urn:iki:harness:control:id:rust-ci",
            Some("beta"),
            "application/json",
            &everyone()
        )
        .is_err());
    let detail = s
        .read(
            "urn:iki:harness:control:id:rust-ci",
            Some("alpha"),
            "text/html",
            &everyone(),
        )
        .unwrap();
    assert!(
        detail.contains("From the pack \u{201c}iki rules\u{201d} 1.4"),
        "{detail}"
    );
}

/// ★ A root adopts a pack its shape misses; adopting a pack no line binds is stated in words.
#[test]
fn explicit_adoption_brings_a_pack_and_unbound_adoption_is_stated() {
    let scene = scene();
    let s = served(&scene, scene.config(&[IKI]).unwrap());
    let delta = s.page("delta", &everyone());
    for needle in [
        "delta&#39;s own control",
        "hook: a newline is not a gate",
        "<li>\u{201c}iki rules\u{201d} 1.4<p class=\"harness-facts\">adopted by this repository",
        "From the pack \u{201c}iki rules\u{201d} 1.4, adopted by this repository",
        "This repository\u{2019}s harness adopts the pack \u{201c}nobody\u{201d}, which no gonk.harness.pack line binds, so it is not loaded.",
    ] {
        let found = delta.contains(needle) || delta.contains(&needle.replace("&#39;", "'"))
            || delta.contains(&needle.replace('\u{2019}', "'"))
            || delta.contains(&needle.replace('\u{2019}', "&#39;"));
        assert!(found, "`{needle}` is not on delta's page:\n{delta}");
    }
    // delta targets no Unix and has no Cargo.toml: the narrower rules stay away.
    assert!(!delta.contains("gate: Rust CI is green"), "{delta}");
}

/// ★ A pack's firings directory: two sessions' segments (one rotated), lines for two roots and
/// `repo=-`. Each root counts its own; the rest, and those naming a root the caller cannot read,
/// count on the pack's line. The tree's own log is unioned in; no command or session is shown.
#[test]
fn pack_firings_count_per_root_and_the_rest_on_the_pack_line() {
    let scene = scene();
    write(
        &scene.roots[0].1,
        ".gonk/state/harness/firings.log",
        &segment(
            "tree-alpha",
            &line(
                "2026-10-31T11:00:00.000Z",
                "Refusal",
                "rust-ci",
                1,
                "t",
                "alpha",
            ),
        ),
    );
    let s = served(&scene, scene.config(&[IKI]).unwrap());
    let newline = "urn:iki:harness:control:id:newline-is-not-a-gate";
    let rust = "urn:iki:harness:control:id:rust-ci";
    assert_eq!(s.json(newline, Some("alpha"), &everyone())["firings"], 2);
    // One in a rotated segment of the pack, one in the tree's own log.
    assert_eq!(s.json(rust, Some("alpha"), &everyone())["firings"], 2);
    assert_eq!(s.json(newline, Some("beta"), &everyone())["firings"], 2);
    let alpha = s.page("alpha", &everyone());
    // `repo=-` twice, a root gonk does not browse once.
    assert!(
        alpha.contains("3 firings outside any repository you may read"),
        "{alpha}"
    );
    for absent in ["sha256:", "urn:agents:session", "s1.log"] {
        assert!(
            !alpha.contains(absent),
            "`{absent}` is on the page:\n{alpha}"
        );
    }
    let day = s.json(
        "urn:iki:harness:firing:date:2026-10-31",
        Some("alpha"),
        &everyone(),
    );
    assert_eq!(day["rows"].as_array().unwrap().len(), 2, "{day}");
    // A caller who reads alpha and not beta: beta's firings move to the pack's line.
    let page = s.page("alpha", &reading(&["alpha"]));
    assert!(
        page.contains("5 firings outside any repository you may read"),
        "{page}"
    );
}

/// ★ A caller without browse read on a root sees nothing of it: not its harness, its
/// description, nor its firings.
#[test]
fn a_caller_without_read_on_a_root_sees_nothing_of_it() {
    let scene = scene();
    let s = served(&scene, scene.config(&[IKI]).unwrap());
    let alpha_only = reading(&["alpha"]);
    for iri in ["urn:repo:beta:description", "urn:repo:omega:description"] {
        match s.read(iri, None, "application/json", &alpha_only) {
            Err(Error::Denied(why)) => {
                assert!(
                    !why.contains("internal") && !why.contains("windows"),
                    "{why}"
                )
            }
            other => panic!("{iri}: {other:?}"),
        }
    }
    let page = s
        .read(
            "urn:iki:gonk:page:harness",
            Some("beta"),
            "text/html",
            &alpha_only,
        )
        .unwrap_or_default();
    assert!(
        !page.contains("iki rules") && !page.contains("newline"),
        "{page}"
    );
    match s.read(
        "urn:iki:harness:control:id:newline-is-not-a-gate",
        Some("beta"),
        "application/json",
        &alpha_only,
    ) {
        Err(Error::Denied(_)) => {}
        other => panic!("{other:?}"),
    }
    // Nor does the default root move to beta: alpha's page counts none of beta's firings.
    let control = s.json(
        "urn:iki:harness:control:id:newline-is-not-a-gate",
        Some("alpha"),
        &alpha_only,
    );
    assert_eq!(control["firings"], 2, "{control}");
}

/// ★ A shape the subset validator refuses is stated on the page, in words, and its pack is not
/// loaded.
#[test]
fn a_refused_shape_is_stated_in_words() {
    let scene = scene();
    let s = served(
        &scene,
        scene
            .config(&[IKI, "odd alpha:claude/harness/odd.ttl"])
            .unwrap(),
    );
    let alpha = s.page("alpha", &everyone());
    assert!(
        alpha.contains("The pack \u{201c}odd rules\u{201d} cannot be matched against this repository: the shape &lt;urn:odd:shape&gt; is one the subset validator refuses"),
        "{alpha}"
    );
    assert!(
        !alpha.contains("<a href=\"/harness/control/x?root=alpha\">odd</a>"),
        "{alpha}"
    );
}

/// ★ Two loaded graphs giving one kind and id to different IRIs: the page is refused, naming
/// both.
#[test]
fn an_id_collision_is_refused_naming_both_iris() {
    let scene = scene();
    write(
        &scene.roots[3].1,
        ".gonk/harness/harness.ttl",
        &(DELTA_HARNESS.to_string()
            + "<urn:delta:control:newline-is-not-a-gate> a harness:Control ; rdfs:label \"mine\" .\n"),
    );
    let s = served(&scene, scene.config(&[IKI]).unwrap());
    let page = s.page("delta", &everyone());
    assert!(
        page.contains("give the control id `newline-is-not-a-gate` to different IRIs"),
        "{page}"
    );
    assert!(
        page.contains("urn:delta:control:newline-is-not-a-gate"),
        "{page}"
    );
    assert!(
        page.contains("urn:iki:harness:control:newline-is-not-a-gate"),
        "{page}"
    );
    assert!(s
        .read(
            "urn:iki:harness:control:id:own",
            Some("delta"),
            "application/json",
            &everyone()
        )
        .is_err());
}

/// The measurement the item asks for: a page read over about 3,500 firing lines in 15
/// segments, read afresh every time.
#[test]
fn a_page_over_3500_firings_in_15_segments_is_measured() {
    let scene = scene();
    for f in std::fs::read_dir(&scene.firings).unwrap() {
        let f = f.unwrap().path();
        if f.is_file() {
            std::fs::remove_file(f).unwrap();
        }
    }
    let ids = ["newline-is-not-a-gate", "rust-ci", "unix-paths"];
    let repos = ["alpha", "beta", "-", "alpha", "alpha"];
    let mut n = 0;
    for seg in 0..15 {
        let mut body = String::new();
        for i in 0..234 {
            n += 1;
            let at = format!(
                "2026-10-{:02}T{:02}:{:02}:00.000Z",
                1 + (n % 28),
                i % 24,
                seg * 3 % 60
            );
            body.push_str(&line(
                &at,
                if i % 3 == 0 { "Refusal" } else { "Warning" },
                ids[n % 3],
                i as u32 + 1,
                &format!("m{seg}"),
                repos[n % 5],
            ));
        }
        std::fs::write(
            scene.firings.join(format!("m{seg}.log")),
            segment(&format!("m{seg}"), &body),
        )
        .unwrap();
    }
    assert_eq!(n, 3510);
    let s = served(&scene, scene.config(&[IKI]).unwrap());
    s.page("alpha", &everyone());
    let started = Instant::now();
    let runs = 3;
    for _ in 0..runs {
        let page = s.page("alpha", &everyone());
        assert!(
            page.contains("firings outside any repository you may read"),
            "{page}"
        );
    }
    let each = started.elapsed() / runs;
    eprintln!("a harness page over {n} firing lines in 15 segments: {each:?} a read");
    assert!(each.as_secs() < 30, "{each:?}");
}
