//! The environment every `git` gonk runs gets (ledger #3489).
//!
//! gonk runs `git -C <dir>` in its browse roots, its configured repositories and its managed
//! checkouts, and a root may be a SUBDIRECTORY of a clone. Tracked content there that spells a
//! bare repository (`HEAD`, `config`, `objects/`, `refs/`) is one git ADOPTS when it runs in
//! that directory: it reads that `config`, so a committed `core.fsmonitor` runs on `git
//! status`, a committed `commondir` reads a repository outside every root, and a committed
//! `remote.origin.url` names whatever repository its author likes. So is an inherited
//! `GIT_DIR`-family variable, which replaces the directory gonk named with another.
//!
//! [`harden`] closes both, as ikigai-repo PR 31 (ledger #3487) did for its own git:
//! `safe.bareRepository=explicit` through `GIT_CONFIG_COUNT` (command-line scope, which git
//! honors for that key where a repository's own config is not), and the repository-locating
//! variables removed. A checkout's own `.git` is still found: `explicit` refuses only a bare
//! repository git DISCOVERED. ikigai-repo does not export its helper, so this mirrors it; one
//! shared helper should exist.

use std::process::Command;

/// The variables that point git at a repository other than the directory it runs in, or add
/// configuration to it: `git rev-parse --local-env-vars`, and `GIT_NAMESPACE`.
const LOCATING: [&str; 16] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

/// The GitHub tokens a local read has no use for, kept from git and whatever it runs.
const TOKENS: [&str; 4] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

/// Refuse a discovered bare repository and drop the inherited repository-locating variables.
/// For git that may reach the network (a clone or fetch): its credentials are left alone.
pub fn harden(command: &mut Command) -> &mut Command {
    for name in LOCATING {
        command.env_remove(name);
    }
    command
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "safe.bareRepository")
        .env("GIT_CONFIG_VALUE_0", "explicit")
}

/// [`harden`], and the GitHub tokens removed: for a read of the local repository only.
pub fn harden_local(command: &mut Command) -> &mut Command {
    for name in TOKENS {
        command.env_remove(name);
    }
    harden(command)
}

/// Scratch repositories shaped as a pull request's author could shape them.
#[cfg(test)]
pub(crate) mod fixture {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// A clone whose tracked content holds two embedded bare repositories:
    /// `adopt/` with a `core.fsmonitor` that writes `marker` and a foreign `remote.origin.url`,
    /// and `escape/` whose `commondir` is `secret`, a repository outside the clone.
    pub struct Planted {
        pub _dir: tempfile::TempDir,
        pub clone: PathBuf,
        pub secret: PathBuf,
        /// The full SHA of `secret`'s one commit, whose message is `SECRET-MESSAGE`.
        pub secret_sha: String,
        pub marker: PathBuf,
    }

    impl Planted {
        pub fn adopt(&self) -> PathBuf {
            self.clone.join("adopt")
        }
        pub fn escape(&self) -> PathBuf {
            self.clone.join("escape")
        }
    }

    pub fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    pub fn planted() -> Planted {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let secret = root.join("secret");
        std::fs::create_dir(&secret).unwrap();
        git(&secret, &["init", "-q", "-b", "main"]);
        git(
            &secret,
            &["commit", "-q", "--allow-empty", "-m", "SECRET-MESSAGE"],
        );
        let secret_sha = git(&secret, &["rev-parse", "HEAD"]);
        let marker = root.join("PWNED");

        let head = root.join("head");
        std::fs::create_dir(&head).unwrap();
        git(&head, &["init", "-q", "-b", "main"]);
        write(&head.join("adopt/HEAD"), "ref: refs/heads/main\n");
        write(
            &head.join("adopt/config"),
            &format!(
                "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\tworktree = .\n\
                 \tfsmonitor = \"touch '{}'; false\"\n\
                 [remote \"origin\"]\n\turl = https://github.com/attacker/elsewhere\n",
                marker.display()
            ),
        );
        write(&head.join("adopt/objects/info/keep"), "");
        write(&head.join("adopt/refs/heads/keep"), "");
        write(&head.join("escape/HEAD"), "ref: refs/heads/main\n");
        write(
            &head.join("escape/commondir"),
            &format!("{}\n", secret.join(".git").display()),
        );
        write(&head.join("escape/config"), "[core]\n\tbare = true\n");
        write(&head.join("escape/objects/keep"), "");
        write(&head.join("escape/refs/keep"), "");
        write(&head.join("README"), "a pull request head\n");
        git(&head, &["add", "-A"]);
        git(&head, &["commit", "-q", "-m", "crafted"]);

        let clone = root.join("clone");
        git(
            &root,
            &["clone", "-q", "--", head.to_str().unwrap(), "clone"],
        );
        git(
            &clone,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/owner/repo",
            ],
        );
        Planted {
            _dir: dir,
            clone,
            secret,
            secret_sha,
            marker,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;
    use std::path::Path;

    /// Without [`harden`], git adopts the planted repositories: the fixture is the attack.
    #[test]
    fn unhardened_git_adopts_the_planted_repositories() {
        let p = planted();
        let _ = Command::new("git")
            .arg("-C")
            .arg(p.adopt())
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(p.marker.exists(), "the committed fsmonitor ran");
        let common = git(&p.escape(), &["rev-parse", "--git-common-dir"]);
        assert_eq!(Path::new(&common), p.secret.join(".git"));
    }

    #[test]
    fn hardened_git_refuses_a_discovered_bare_repository() {
        let p = planted();
        for dir in [p.adopt(), p.escape()] {
            let out = harden(Command::new("git").arg("-C").arg(&dir))
                .args(["status", "--porcelain"])
                .output()
                .unwrap();
            assert!(!out.status.success(), "{}", dir.display());
            assert!(String::from_utf8_lossy(&out.stderr).contains("safe.bareRepository"));
        }
        assert!(!p.marker.exists(), "the committed fsmonitor did not run");
        // The clone's own `.git` is still found.
        let out = harden(Command::new("git").arg("-C").arg(&p.clone))
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn an_inherited_git_dir_does_not_replace_the_directory() {
        let p = planted();
        let mut command = Command::new("git");
        command
            .env("GIT_DIR", p.secret.join(".git"))
            .env("GIT_CONFIG_PARAMETERS", "'safe.bareRepository'='all'")
            .env("GH_TOKEN", "ghp_secret");
        let out = harden_local(command.arg("-C").arg(&p.clone))
            .args(["log", "-1", "--format=%B"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains("crafted"), "{text}");
        let envs: Vec<_> = command.get_envs().collect();
        for name in ["GIT_DIR", "GIT_CONFIG_PARAMETERS", "GH_TOKEN"] {
            assert!(envs.contains(&(name.as_ref(), None)), "{name} removed");
        }
    }
}
