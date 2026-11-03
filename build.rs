//! What the binary says about itself (ledger #2972): the commit it was built from, when, and the
//! ledger and browse crates it links. `crate::system` serves them on the Dashboard's System tab.
//!
//! A build from a published package has no `.git`, so the commit comes from the
//! `.cargo_vcs_info.json` cargo packs instead, and a build with neither says `unknown`: a blank is
//! true and a guess is not. `SOURCE_DATE_EPOCH` wins over the clock, so a reproducible build stays
//! one.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (out.status.success() && !text.is_empty()).then(|| text.to_string())
}

/// `"sha1": "…"` in cargo's own record of the commit it packaged.
fn packaged_commit() -> Option<String> {
    let text = std::fs::read_to_string(".cargo_vcs_info.json").ok()?;
    let after = text.split("\"sha1\"").nth(1)?;
    Some(after.split('"').nth(1)?.to_string())
}

/// The version `Cargo.lock` resolved `name` to.
fn locked(lock: &str, name: &str) -> String {
    let wanted = format!("name = \"{name}\"");
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line == wanted {
            if let Some(version) = lines.next().and_then(|v| v.strip_prefix("version = ")) {
                return version.trim_matches('"').to_string();
            }
        }
    }
    "unknown".to_string()
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    // HEAD for a checkout of another branch, and the ref it names for a commit on this one. A
    // worktree's `.git` is a file, so git says where each of them is.
    for watched in ["HEAD", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", watched]) {
            if Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
    if let Some(path) = git(&["symbolic-ref", "-q", "HEAD"])
        .and_then(|name| git(&["rev-parse", "--git-path", &name]))
    {
        if Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let commit = git(&["rev-parse", "HEAD"]).or_else(packaged_commit);
    println!(
        "cargo:rustc-env=GONK_BUILD_COMMIT={}",
        commit.unwrap_or_else(|| "unknown".to_string())
    );
    let built = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        })
        .unwrap_or(0);
    println!("cargo:rustc-env=GONK_BUILT_AT_SECONDS={built}");

    let lock = std::fs::read_to_string("Cargo.lock").unwrap_or_default();
    for (key, name) in [
        ("GONK_LINKS_LEDGER", "ikigai-ledger"),
        ("GONK_LINKS_BROWSE", "ikigai-browse"),
    ] {
        println!("cargo:rustc-env={key}={}", locked(&lock, name));
    }
}
