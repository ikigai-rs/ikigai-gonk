//! Ledger #2783: browse-root adds fired at once each land in `config.toml`.
//!
//! `write_root` read the file, appended its line and renamed a staging file over it, with no
//! lock and one staging name for every writer: two adds at once both answered `Added` and the
//! later rename dropped the other's line (found by the check-then-act audit, ledger #2772).
//! Each round here releases N adds (and, in the second test, removes) together by a barrier and
//! then reads the file back.

use std::path::Path;
use std::sync::{Arc, Barrier};

use ikigai_gonk::checkout::{comment_out_root, ConfigLine};
use ikigai_gonk::roots::write_root;

const ROUNDS: usize = 30;
const ADDS: usize = 6;

/// The names configured by a line in force (not commented out) in `config`.
fn configured(config: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(config).unwrap_or_default();
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("gonk.browse.root"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .filter_map(|value| {
            let value = value.trim().trim_matches('"');
            Some(value.split_once('=')?.0.trim().to_string())
        })
        .collect()
}

#[test]
fn concurrent_root_adds_all_land_in_config_toml() {
    let mut violations = Vec::new();
    for round in 0..ROUNDS {
        let scratch = tempfile::tempdir().unwrap();
        let config = Arc::new(scratch.path().join("config.toml"));
        std::fs::write(&*config, "# browse roots\n").unwrap();
        let barrier = Arc::new(Barrier::new(ADDS));
        let adds: Vec<_> = (0..ADDS)
            .map(|i| {
                let name = format!("r{i}");
                let dir = scratch.path().join(&name);
                std::fs::create_dir(&dir).unwrap();
                let (config, barrier, home) = (
                    Arc::clone(&config),
                    Arc::clone(&barrier),
                    scratch.path().to_path_buf(),
                );
                std::thread::spawn(move || {
                    barrier.wait();
                    let answer = write_root(&config, &home, &name, &dir.display().to_string());
                    (name, answer)
                })
            })
            .collect();
        let answers: Vec<_> = adds.into_iter().map(|t| t.join().unwrap()).collect();
        let lines = configured(&config);
        for (name, answer) in &answers {
            if *answer != Ok(ConfigLine::Added) || !lines.contains(name) {
                violations.push(format!(
                    "round {round}: {name} answered {answer:?}; the file has {lines:?}"
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "{} answer(s) did not land:\n{}",
        violations.len(),
        violations.join("\n")
    );
}

#[test]
fn a_remove_at_the_same_time_as_adds_keeps_every_add() {
    let mut violations = Vec::new();
    for round in 0..ROUNDS {
        let scratch = tempfile::tempdir().unwrap();
        let config = Arc::new(scratch.path().join("config.toml"));
        let gone = scratch.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        std::fs::write(
            &*config,
            format!("gonk.browse.root = \"gone={}\"\n", gone.display()),
        )
        .unwrap();
        let barrier = Arc::new(Barrier::new(ADDS + 1));
        let mut threads: Vec<_> = (0..ADDS)
            .map(|i| {
                let name = format!("r{i}");
                let dir = scratch.path().join(&name);
                std::fs::create_dir(&dir).unwrap();
                let (config, barrier, home) = (
                    Arc::clone(&config),
                    Arc::clone(&barrier),
                    scratch.path().to_path_buf(),
                );
                std::thread::spawn(move || {
                    barrier.wait();
                    write_root(&config, &home, &name, &dir.display().to_string()).map(|_| ())
                })
            })
            .collect();
        {
            let (config, barrier) = (Arc::clone(&config), Arc::clone(&barrier));
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                comment_out_root(&config, "gone").map(|_| ())
            }));
        }
        for thread in threads {
            if let Err(e) = thread.join().unwrap() {
                violations.push(format!("round {round}: {e}"));
            }
        }
        let lines = configured(&config);
        let wanted: Vec<String> = (0..ADDS).map(|i| format!("r{i}")).collect();
        let mut sorted = lines.clone();
        sorted.sort();
        if sorted != wanted {
            violations.push(format!("round {round}: the file has {lines:?}"));
        }
    }
    assert!(
        violations.is_empty(),
        "{} round(s) broke:\n{}",
        violations.len(),
        violations.join("\n")
    );
}
