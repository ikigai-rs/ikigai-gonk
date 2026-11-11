//! The log gonk's stderr is written to is owner-only once gonk has started (ledger #3159),
//! even when launchd created it `0644` before: every access line names its caller.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_pre_existing_world_readable_log_is_0600_after_serve_starts() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).unwrap();
    let log = logs.join("gonk.log");
    std::fs::write(&log, "an earlier run's line\n").unwrap();
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(mode(&log), 0o644);

    // As launchd does: stdout and stderr both appended to the one file.
    let file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("c"))
        .args(["--port", "0", "--socket", "g.sock"])
        .stdin(Stdio::null())
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .spawn()
        .unwrap();

    // Started: the banner's `mount` line is in the log.
    let deadline = Instant::now() + Duration::from_secs(60);
    let started = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.lines().any(|l| l.trim_start().starts_with("mount")) {
            break true;
        }
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let observed = mode(&log);
    let _ = child.kill();
    let _ = child.wait();
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(started, "the server printed no banner:\n{text}");
    assert_eq!(
        observed, 0o600,
        "the log is owner-only after start:\n{text}"
    );
    assert!(
        text.starts_with("an earlier run's line\n"),
        "a same-day start appends, it does not rotate"
    );
}
