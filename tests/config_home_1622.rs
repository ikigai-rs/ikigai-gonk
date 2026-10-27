use ikigai_gonk::config::parse_args;

#[test]
fn every_grants_form_accepts_home_overrides() {
    for subject in [
        "acme",
        "acme read",
        "--browse-graph",
        "--browse-graph read",
        "--ledger-graph acme",
        "--browse read",
    ] {
        for flag in ["--config", "--config-home", "--data-home"] {
            let line = format!("grants {subject} {flag} /scratch");
            assert!(
                parse_args(line.split_whitespace().map(String::from)).is_ok(),
                "{line}"
            );
        }
    }
}

#[test]
fn backfill_accepts_home_overrides() {
    for flag in ["--config-home", "--data-home"] {
        let line = format!("ledger backfill-keys {flag} /scratch");
        assert!(
            parse_args(line.split_whitespace().map(String::from)).is_ok(),
            "{line}"
        );
    }
}

#[test]
fn grants_validate_home_values_and_authority() {
    for line in [
        "grants acme --config-home",
        "grants --ledger-graph acme --data-home",
        "grants --browse-graph --config",
        "grants acme --config-home /scratch admin",
        "grants acme read write",
        "grants --browse-graph --config-home /scratch worker",
        "grants --ledger-graph acme --bogus /scratch",
    ] {
        assert!(
            parse_args(line.split_whitespace().map(String::from)).is_err(),
            "{line}"
        );
    }
    assert!(matches!(
        parse_args(
            "grants acme --config-home /scratch read --data-home /data"
                .split_whitespace()
                .map(String::from)
        )
        .unwrap(),
        ikigai_gonk::config::Command::Grants {
            authority: ikigai_gonk::grants::Authority::Read,
            ..
        }
    ));
}

#[test]
fn home_overrides_do_not_change_config_independent_grant_tokens() {
    let dir = tempfile::tempdir().unwrap();
    for subject in [
        vec!["acme", "read"],
        vec!["--browse-graph", "read"],
        vec!["--ledger-graph", "acme"],
    ] {
        let run = |overrides: bool| {
            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"));
            command.arg("grants").args(&subject);
            if overrides {
                command
                    .arg("--config-home")
                    .arg(dir.path())
                    .arg("--data-home")
                    .arg(dir.path())
                    .arg("--config")
                    .arg(dir.path().join("absent.toml"));
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        assert_eq!(run(false), run(true));
    }
}

#[test]
fn backfill_reads_the_selected_config_home() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("selected.sock");
    std::fs::write(
        dir.path().join("config.toml"),
        format!("gonk.socket = \"{}\"\n", socket.display()),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["ledger", "backfill-keys", "--dry-run", "--config-home"])
        .arg(dir.path())
        .arg("--data-home")
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot reach gonk's socket"), "{stderr}");
    assert!(stderr.contains(socket.to_str().unwrap()), "{stderr}");
}
