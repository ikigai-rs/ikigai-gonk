//! `ikigai-gonk checkout`, end to end: the real binary against scratch git repositories
//! served by `file://` URLs. Nothing here touches the network, the operator's config home or
//! `~/.ikigai`: every run gets its own `HOME`, `XDG_CONFIG_HOME`, `--dir` and `--config`, and
//! git reads no global or system configuration (so a machine that signs commits, or sets a
//! different default branch, runs these the same as CI).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch world: an upstream bare repository, a working clone that pushes to it, and the
/// directories the command under test is pointed at.
struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    upstream: PathBuf,
    pusher: PathBuf,
}

impl World {
    fn new() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let upstream = root.join("upstream/demo.git");
        let pusher = root.join("pusher");
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["init", "--quiet", "--bare", "-b", "main"]);
        git(&root, &["clone", "--quiet", &url(&upstream), "pusher"]);
        let world = World {
            _tmp: tmp,
            root,
            upstream,
            pusher,
        };
        world.commit_upstream("README.md", "one\n");
        world
    }

    /// Commit `text` into `file` upstream, through the pushing clone.
    fn commit_upstream(&self, file: &str, text: &str) {
        std::fs::write(self.pusher.join(file), text).unwrap();
        git(&self.pusher, &["add", file]);
        git(&self.pusher, &["commit", "--quiet", "-m", text.trim()]);
        git(&self.pusher, &["push", "--quiet", "origin", "HEAD:main"]);
    }

    fn url(&self) -> String {
        url(&self.upstream)
    }

    fn checkouts(&self) -> PathBuf {
        self.root.join("checkouts")
    }

    fn config(&self) -> PathBuf {
        self.root.join("xdg/ikigai/config.toml")
    }

    fn checkout_dir(&self) -> PathBuf {
        self.checkouts().join("demo")
    }

    /// Run `ikigai-gonk checkout <extra…> --dir … --config …`.
    fn run(&self, extra: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"));
        command
            .arg("checkout")
            .args(extra)
            .arg("--dir")
            .arg(self.checkouts())
            .arg("--config")
            .arg(self.config());
        isolate(&mut command, &self.root);
        command.output().expect("run ikigai-gonk")
    }

    fn head(&self) -> String {
        git(&self.checkout_dir(), &["rev-parse", "HEAD"])
    }

    fn upstream_head(&self) -> String {
        git(&self.upstream, &["rev-parse", "main"])
    }
}

fn url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// No global or system git config, a scratch HOME and config home.
fn isolate(command: &mut Command, root: &Path) {
    command
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    isolate(&mut command, dir);
    let output = command.output().expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn refused(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(1),
        "a refusal exits 1\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_first_run_clones_and_a_later_run_fast_forwards() {
    let world = World::new();
    let url = world.url();

    let first = ok(&world.run(&[&url]));
    assert!(first.contains("cloned     demo at "), "{first}");
    let line = format!(
        "gonk.browse.root = \"demo={}\"",
        world.checkout_dir().display()
    );
    assert!(first.contains(&line), "the root line: {first}");
    assert!(first.contains("not written"), "{first}");
    assert!(
        !world.config().exists(),
        "nothing written without --write-config"
    );
    assert_eq!(world.head(), world.upstream_head());

    world.commit_upstream("README.md", "two\n");
    let second = ok(&world.run(&[&url]));
    assert!(second.contains("updated    demo "), "{second}");
    assert!(second.contains("(1 commit(s), fast-forward)"), "{second}");
    assert_eq!(world.head(), world.upstream_head());
    assert_eq!(
        std::fs::read_to_string(world.checkout_dir().join("README.md")).unwrap(),
        "two\n"
    );

    let third = ok(&world.run(&[&url]));
    assert!(third.contains("current    demo at "), "{third}");
}

#[test]
fn a_checkout_with_local_changes_is_refused_and_left_alone() {
    let world = World::new();
    let url = world.url();
    ok(&world.run(&[&url]));
    let before = world.head();
    std::fs::write(world.checkout_dir().join("README.md"), "edited here\n").unwrap();
    world.commit_upstream("README.md", "two\n");

    let out = refused(&world.run(&[&url]));
    assert!(out.contains("REFUSED    demo"), "{out}");
    assert!(out.contains("local changes"), "{out}");
    assert!(
        !out.contains("browse roots:"),
        "no root line for a refusal: {out}"
    );
    assert_eq!(world.head(), before, "nothing moved");
    assert_eq!(
        std::fs::read_to_string(world.checkout_dir().join("README.md")).unwrap(),
        "edited here\n",
        "the local edit survives"
    );
}

#[test]
fn a_diverged_checkout_is_refused_and_left_alone() {
    let world = World::new();
    let url = world.url();
    ok(&world.run(&[&url]));
    let dir = world.checkout_dir();
    std::fs::write(dir.join("LOCAL.md"), "mine\n").unwrap();
    git(&dir, &["add", "LOCAL.md"]);
    git(&dir, &["commit", "--quiet", "-m", "local"]);
    let local = world.head();
    world.commit_upstream("README.md", "two\n");

    let out = refused(&world.run(&[&url]));
    assert!(out.contains("DIVERGED"), "{out}");
    assert_eq!(world.head(), local, "nothing reset or merged");

    // A checkout on another branch is refused the same way.
    git(&dir, &["checkout", "--quiet", "-b", "side"]);
    let out = refused(&world.run(&[&url]));
    assert!(out.contains("not the default branch main"), "{out}");
}

#[test]
fn a_different_repository_under_the_same_name_is_refused() {
    let world = World::new();
    ok(&world.run(&[&world.url()]));
    let other = world.root.join("elsewhere/demo.git");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "--quiet", "--bare", "-b", "main"]);
    let out = refused(&world.run(&[&url(&other)]));
    assert!(out.contains("a different repository"), "{out}");
}

#[test]
fn an_unreachable_url_fails_without_waiting_and_the_rest_still_run() {
    let world = World::new();
    let missing = url(&world.root.join("nowhere/gone.git"));
    let out = refused(&world.run(&[&missing, &world.url()]));
    assert!(out.contains("FAILED     gone"), "{out}");
    assert!(
        out.contains("cloned     demo"),
        "the other URL still ran: {out}"
    );
}

#[test]
fn write_config_appends_once_backs_up_and_never_edits_another_line() {
    let world = World::new();
    let url = world.url();
    let config = world.config();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let original =
        "# mine\ngonk.http.ledger = \"default\"\ngonk.browse.root = \"other=/srv/other\"";
    std::fs::write(&config, original).unwrap();

    let first = ok(&world.run(&[&url, "--write-config"]));
    assert!(first.contains("added      gonk.browse.root"), "{first}");
    assert!(
        first.contains("launchctl kickstart -k"),
        "the restart: {first}"
    );
    let line = format!(
        "gonk.browse.root = \"demo={}\"",
        world.checkout_dir().display()
    );
    let text = std::fs::read_to_string(&config).unwrap();
    assert_eq!(
        text,
        format!("{original}\n{line}\n"),
        "appended, nothing else touched"
    );
    let backups: Vec<_> = std::fs::read_dir(config.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".bak"))
        .collect();
    assert_eq!(backups.len(), 1, "one backup beside it");
    assert_eq!(
        std::fs::read_to_string(backups[0].path()).unwrap(),
        original
    );

    let second = ok(&world.run(&[&url, "--write-config"]));
    assert!(second.contains("present    gonk.browse.root"), "{second}");
    assert!(
        !second.contains("restart"),
        "nothing new to restart for: {second}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        text,
        "idempotent"
    );

    // The same name configured for another directory is a conflict: no line added, the
    // existing one left as it is, and the run says so with exit 1.
    let clash = format!("{original}\ngonk.browse.root = \"demo=/srv/demo\"\n");
    std::fs::write(&config, &clash).unwrap();
    let third = refused(&world.run(&[&url, "--write-config"]));
    assert!(third.contains("CONFLICT   `demo`"), "{third}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), clash);
}

#[test]
fn a_name_is_given_with_name_equals() {
    let world = World::new();
    let spec = format!("mine={}", world.url());
    let out = ok(&world.run(&[&spec]));
    assert!(out.contains("cloned     mine"), "{out}");
    assert!(world.checkouts().join("mine/README.md").exists());
}

/// A second upstream beside the world's `demo`, with its own pushing clone.
struct Second {
    upstream: PathBuf,
    pusher: PathBuf,
}

impl Second {
    fn new(world: &World, name: &str) -> Second {
        let upstream = world.root.join(format!("upstream/{name}.git"));
        let pusher = world.root.join(format!("pusher-{name}"));
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["init", "--quiet", "--bare", "-b", "main"]);
        git(
            &world.root,
            &[
                "clone",
                "--quiet",
                &url(&upstream),
                pusher.to_str().unwrap(),
            ],
        );
        let second = Second { upstream, pusher };
        second.commit_upstream("README.md", "first\n");
        second
    }

    fn commit_upstream(&self, file: &str, text: &str) {
        std::fs::write(self.pusher.join(file), text).unwrap();
        git(&self.pusher, &["add", file]);
        git(&self.pusher, &["commit", "--quiet", "-m", text.trim()]);
        git(&self.pusher, &["push", "--quiet", "origin", "HEAD:main"]);
    }

    fn url(&self) -> String {
        url(&self.upstream)
    }

    fn upstream_head(&self) -> String {
        git(&self.upstream, &["rev-parse", "main"])
    }
}

#[test]
fn all_fast_forwards_every_checkout_from_its_own_origin() {
    let world = World::new();
    let tools = Second::new(&world, "tools");
    ok(&world.run(&[&world.url(), &tools.url()]));
    world.commit_upstream("README.md", "two\n");
    tools.commit_upstream("README.md", "second\n");
    tools.commit_upstream("NOTES.md", "third\n");

    let out = ok(&world.run(&["--all"]));
    assert!(
        out.contains("updated    demo ") && out.contains("(1 commit(s), fast-forward)"),
        "{out}"
    );
    assert!(
        out.contains("updated    tools ") && out.contains("(2 commit(s), fast-forward)"),
        "{out}"
    );
    assert_eq!(world.head(), world.upstream_head());
    assert_eq!(
        git(&world.checkouts().join("tools"), &["rev-parse", "HEAD"]),
        tools.upstream_head()
    );
    assert!(
        !out.contains("browse roots:"),
        "--all adds no root, so prints no root lines: {out}"
    );
    assert!(!world.config().exists(), "--all never writes config");

    let again = ok(&world.run(&["--all"]));
    assert!(again.contains("current    demo at "), "{again}");
    assert!(again.contains("current    tools at "), "{again}");
}

#[test]
fn all_refuses_a_dirty_checkout_and_still_updates_the_rest() {
    let world = World::new();
    let tools = Second::new(&world, "tools");
    ok(&world.run(&[&world.url(), &tools.url()]));
    let before = world.head();
    // Untracked counts: a file the operator dropped in is still theirs.
    std::fs::write(world.checkout_dir().join("SCRATCH.md"), "mine\n").unwrap();
    world.commit_upstream("README.md", "two\n");
    tools.commit_upstream("README.md", "second\n");

    let out = refused(&world.run(&["--all"]));
    assert!(out.contains("REFUSED    demo"), "{out}");
    assert!(out.contains("local changes"), "{out}");
    assert!(
        out.contains("updated    tools "),
        "the other still ran: {out}"
    );
    assert_eq!(world.head(), before, "nothing moved in the dirty one");
    assert_eq!(
        git(&world.checkouts().join("tools"), &["rev-parse", "HEAD"]),
        tools.upstream_head()
    );
    assert!(world.checkout_dir().join("SCRATCH.md").exists());
}

#[test]
fn all_reports_coverage_without_changing_the_config() {
    let world = World::new();
    let tools = Second::new(&world, "tools");
    ok(&world.run(&[&world.url(), &tools.url()]));
    // A dot-directory is not a checkout; a plain directory is refused as one.
    std::fs::create_dir_all(world.checkouts().join(".cache")).unwrap();
    std::fs::create_dir_all(world.checkouts().join("stray")).unwrap();
    let config = world.config();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let text = format!(
        "gonk.browse.root = \"demo={demo}/crates\"\n\
         gonk.browse.root = \"gone={gone}\"\n\
         gonk.browse.root = \"elsewhere=/srv/elsewhere\"\n",
        demo = world.checkout_dir().display(),
        gone = world.checkouts().join("gone").display(),
    );
    std::fs::write(&config, &text).unwrap();

    let out = refused(&world.run(&["--all"]));
    assert!(out.contains("current    demo at "), "{out}");
    assert!(out.contains("current    tools at "), "{out}");
    assert!(out.contains("REFUSED    stray"), "{out}");
    assert!(out.contains("is not a git checkout"), "{out}");
    assert!(
        !out.contains(".cache"),
        "dot-directories are skipped: {out}"
    );
    assert!(out.contains("\ncoverage of "), "{out}");
    assert!(
        out.contains("  unused     tools  "),
        "no root points into tools: {out}"
    );
    assert!(
        !out.contains("unused     demo"),
        "a root inside a checkout uses it: {out}"
    );
    assert!(out.contains("  missing    gone  "), "{out}");
    assert!(
        !out.contains("elsewhere"),
        "a root outside the managed directory is not this report's business: {out}"
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text, "untouched");
}

#[test]
fn all_on_a_directory_that_does_not_exist_clones_and_creates_nothing() {
    let world = World::new();
    let out = world.run(&["--all"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--all only updates"), "{stderr}");
    assert!(!world.checkouts().exists(), "not created");

    let both = world.run(&["--all", &world.url()]);
    assert_eq!(both.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&both.stderr).contains("not both"));
}
