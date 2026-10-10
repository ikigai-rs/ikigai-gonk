//! Ledger [#1093](http://localhost:1060/l/default/item/1093), item 1: `passkey invite` has no
//! `--socket` flag and never opens the socket, but it read the settings `serve` reads, and
//! those refused a socket path over `sun_path`'s 104 bytes. So an invite under a long
//! `--data-home` (the socket's default directory) was refused with
//! `socket path … is 193 bytes`. Reproduced before the fix with the real binary.
//!
//! The real binary against a scratch config home: no server is started, nothing touches the
//! operator's config home.

use std::process::Command;

#[test]
fn an_invite_under_a_long_data_home_is_not_refused_over_a_socket_it_never_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let config_home = root.join("cfg");
    let data_home = root.join(format!("data-{}", "d".repeat(120)));
    for dir in [&root.join("home"), &config_home, &data_home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["passkey", "invite", "brian", "--ledger", "default=delete"])
        .arg("--config-home")
        .arg(&config_home)
        .arg("--data-home")
        .arg(&data_home)
        .env("HOME", root.join("home"))
        .output()
        .expect("run ikigai-gonk");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(!stderr.contains("socket path"), "{stderr}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("#invite="),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // `serve` still refuses it, before the store is opened.
    let out = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["serve", "--port", "0", "--no-backup"])
        .arg("--config-home")
        .arg(&config_home)
        .arg("--data-home")
        .arg(&data_home)
        .env("HOME", root.join("home"))
        .output()
        .expect("run ikigai-gonk");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "serve must refuse: {stderr}");
    assert!(
        stderr.contains("a Unix socket path must be shorter than"),
        "{stderr}"
    );
    assert!(
        std::fs::read_dir(&data_home).unwrap().next().is_none(),
        "refused before the store is opened: nothing is written under the data home"
    );
}
