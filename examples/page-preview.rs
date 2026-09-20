//! `cargo run --example page-preview -- <out-dir> [root-name=<path>]` — the browse door's
//! pages, as FILES, so a person can look at them.
//!
//! # ★ Why this exists
//!
//! The browse pages cannot be opened by hand. Every browse row declares
//! `urn:cap:browse:read:*`, gonk mints that for no anonymous caller, and the only way to
//! hold it over HTTP is a passkey ceremony in a browser against a live server — which is
//! exactly what a person checking whether a page LOOKS right cannot be asked to set up, and
//! what an agent cannot do at all. So this tool issues the same requests the browser would,
//! through the same kernel `main` composes, under a capability it states in one place, and
//! writes what came back.
//!
//! ⚠ **It proves how a page looks, never that a page is authorized.** The capability here is
//! a fixture; the door's real one is `doors::http_cap`'s per-request capability, and the
//! tests in `tests/browse.rs` are what hold that. A preview that rendered with root
//! authority would still render — which is the whole reason this file says what it grants.
//!
//! # What it writes
//!
//! `index.html` (the root list), `tree.html`, `file.html` — each a whole page with the face
//! ALREADY SWAPPED into `#browse`, because htmx does that swap in a browser and a file on
//! disk has no server to fetch from — plus `gonk.css`, `gonk.js`, `htmx.min.js` and the
//! browse family's two stylesheets as local files, with every link rewritten to match.
//!
//! The leftover `hx-get` attributes are left EXACTLY as browse emitted them: serve the
//! directory with something that answers `/k…` a 403 and the page shows what a caller who
//! may not use one affordance sees, which is the other half of what this tool is for.
//!
//! ```sh
//! cargo run --example page-preview -- /tmp/preview gonk=.
//! (cd /tmp/preview && python3 -m http.server 8777)
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{browse, compose_with, doors, quic, rules, watch, web};
use ikigai_store::DurableStore;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().unwrap_or_else(|| usage("an output directory")));
    let (root, path) = match args.next() {
        Some(pair) => match pair.split_once('=') {
            Some((name, path)) => (name.to_string(), PathBuf::from(path)),
            None => usage("a root as `name=path`"),
        },
        None => ("gonk".to_string(), PathBuf::from(".")),
    };
    let path = path
        .canonicalize()
        .unwrap_or_else(|e| usage(&format!("a readable root ({e})")));

    let kernel = door(&root, &path);
    // ⚠ Stated once, here: what this preview PRETENDS to hold. The all-roots browse
    // wildcard and nothing else — no `urn:cap:annotate` (so the shell states the read-only
    // posture, which is one of the things worth looking at), no net grant (so nothing here
    // can spend inference), no exec token (so the pull-request block refuses, which is the
    // other).
    let cap = Capability::scoped([ikigai_browse::CAP_WILDCARD.to_string()]);

    std::fs::create_dir_all(&out).expect("the output directory");
    for (name, iri) in [
        ("gonk.css", "urn:iki:gonk:asset:gonk.css"),
        ("gonk.js", "urn:iki:gonk:asset:gonk.js"),
        ("htmx.min.js", "urn:iki:gonk:asset:htmx.min.js"),
        ("style.css", "urn:repo:style"),
        ("layout.css", ikigai_browse::LAYOUT_IRI),
    ] {
        write(&out, name, &read(&kernel, &cap, iri, &[]));
    }

    // The root list, with its one link pointed at the tree page written below — so the
    // preview is navigable the way the door is.
    let roots = local(&read(&kernel, &cap, "urn:iki:gonk:page:browse", &[]))
        .replace(&format!("'/browse/urn:repo:{root}:tree'"), "'tree.html'");
    write(&out, "index.html", &roots);
    for (name, start) in [
        ("tree.html", format!("urn:repo:{root}:tree")),
        ("file.html", format!("urn:repo:{root}:file:README.md")),
    ] {
        let shell = read(
            &kernel,
            &cap,
            &format!("urn:iki:gonk:page:browse:{start}"),
            &[],
        );
        let face = read(
            &kernel,
            &cap,
            "urn:iki:gonk:k",
            &[("c", &format!("source {start} as=text/html"))],
        );
        write(&out, name, &local(&swapped(&shell, &face)));
    }
    println!("wrote {} — serve it and look", out.display());
}

/// The kernel `main` composes, over one real root and an in-memory store.
fn door(root: &str, path: &Path) -> Kernel {
    let roots = vec![(root.to_string(), path.to_path_buf())];
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let (watch, _refused) = watch::RootWatch::start(&roots);
    let wired = browse::wire(roots.clone(), handle, watch.watched(), None, &graph);
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        None,
    ));
    let config = std::env::temp_dir().join("ikigai-gonk-preview");
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: roots.iter().map(|(name, _)| name.clone()).collect(),
        passkeys: Arc::new(Passkeys::new(quic::Layout::in_config_home(&config), 1060)),
        rules: rules::DEFAULT_RULES.into(),
    });
    // ⚠ The watcher is dropped with this function, so the previewed pages are a SNAPSHOT.
    // That is what a file on disk is anyway.
    doors::http_kernel(hub, web::space(face))
}

fn read(kernel: &Kernel, cap: &Capability, iri: &str, args: &[(&str, &str)]) -> String {
    let request = args.iter().fold(
        Request::new(Verb::Source, Iri::parse(iri).expect("a preview IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let answer = block_on(Kernel::issue(kernel, request, cap))
        .unwrap_or_else(|e| panic!("{iri} {args:?}: {e}"));
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

/// htmx's first swap, done here: the face's markup REPLACES the region, placeholder,
/// trigger and all.
///
/// ⚠ The trigger has to go with it. A `hx-trigger="load"` left on the region would fire in
/// the browser against a server that is not there, and the swapped face would be wiped by
/// the very error handling this preview exists to show — the page would look broken in a
/// way the real door is not. What a browser holds after the first paint is the face, and
/// that is what this writes.
fn swapped(shell: &str, face: &str) -> String {
    let open = match shell.find("<div class='browse'") {
        Some(open) => open,
        None => return shell.to_string(),
    };
    let rest = &shell[open..];
    let close = rest.find("</div>").expect("the region closes") + "</div>".len();
    format!(
        "{}<div class='browse' id='browse'>{face}</div>{}",
        &shell[..open],
        &rest[close..]
    )
}

/// Every URL a browser would fetch from the door, pointed at the file beside it. The `/k…`
/// affordances are NOT rewritten — see the module docs.
fn local(page: &str) -> String {
    let mut out = page.to_string();
    let k = ikigai_gonk::k::k_url;
    let rewrites: BTreeMap<String, &str> = [
        ("/static/gonk.css".to_string(), "gonk.css"),
        ("/static/gonk.js".to_string(), "gonk.js"),
        ("/static/htmx.min.js".to_string(), "htmx.min.js"),
        (k("source urn:repo:style"), "style.css"),
        (
            k(&format!("source {}", ikigai_browse::LAYOUT_IRI)),
            "layout.css",
        ),
        ("/browse".to_string(), "index.html"),
    ]
    .into_iter()
    .collect();
    // ⚠ Each rewrite matches a WHOLE quoted attribute value, which is what keeps `/browse`
    // from eating the `/browse/urn:repo:…` links that start with it. The rendered face
    // quotes attributes with `'` (xrust's serializer), so that is the quote matched.
    for (from, to) in rewrites.iter().rev() {
        out = out.replace(&format!("'{from}'"), &format!("'{to}'"));
    }
    out
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap_or_else(|e| panic!("writing {name}: {e}"));
}

fn usage(what: &str) -> ! {
    eprintln!(
        "page-preview: expected {what}\n  \
         usage: cargo run --example page-preview -- <out-dir> [root=<path>]"
    );
    std::process::exit(2)
}
