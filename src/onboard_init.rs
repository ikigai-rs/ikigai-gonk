//! `ikigai-gonk onboard init` (ledger [#2123](http://localhost:1062/l/default/item/2123), for
//! [#2009](http://localhost:1062/l/default/item/2009)): ONE command sets the onboarding
//! controller up, asking only questions that have good defaults.
//!
//! In order, each step keeping whatever is already there and saying so:
//!
//! 1. **The controller key**: minted into the keystore (Touch ID on a Mac) unless its public
//!    half's file exists, in which case nothing is minted and the file is read.
//! 2. **The kinds file**: [`crate::onboard::DEFAULT_KINDS`] (gonk's `docs/onboard/kinds.ttl`,
//!    compiled in), written only when there is no file. An existing one is never overwritten.
//! 3. **The settings**: `gonk.onboard` (the layer host's door and the controller's certificate
//!    set there) and `gonk.onboard.door` (how nodes reach this gonk), appended to `config.toml`
//!    only when missing, with a backup beside it, exactly as `checkout --write-config` does
//!    ([`crate::checkout::append_lines`]). A line already there is never rewritten.
//! 4. **The layer host's `[onboard]` table**, printed ready to paste, with the PEM.
//!
//! On a terminal it asks; with `--yes`, or with no terminal on stdin, it asks nothing and takes
//! the flags and the defaults ([`Defaults`]). A rerun mints nothing and adds nothing that is
//! there, so it is safe to run again to see the edge's table.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::config::{values_for, Homes, QuicBind, DEFAULT_PORT};
use crate::onboard::{self as onb, GonkDoor, DOOR_KEY, KEY, SOCKET_DOOR};

/// The layer host's door when no `gonk.layers.view` line names one.
pub const DEFAULT_LAYER_DOOR: &str = "ikigai-rs.dev:4434";

/// The flags that answer `init`'s questions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitFlags {
    /// `--door HOST:PORT|socket`: how nodes reach this gonk.
    pub door: Option<String>,
    /// `--layer-door HOST:PORT`: the layer host's door.
    pub layer_door: Option<String>,
    /// `--cert-dir DIR`: the controller's certificate set at that door.
    pub cert_dir: Option<String>,
    /// `--yes`: take every default, even on a terminal.
    pub yes: bool,
}

/// Asks the operator one question.
pub trait Ask {
    /// The answer to `question`: `default` for an empty one.
    ///
    /// # Errors
    ///
    /// No answer at all (the input ended).
    fn ask(&mut self, question: &str, default: &str) -> Result<String, String>;
}

/// Asks nothing: every answer is its default (no terminal on stdin, or `--yes`).
pub struct Defaults;

impl Ask for Defaults {
    fn ask(&mut self, _question: &str, default: &str) -> Result<String, String> {
        Ok(default.to_string())
    }
}

/// Asks on a terminal: the question and its default on `out`, one line from `input`.
pub struct Prompt<R, W> {
    /// Where answers come from.
    pub input: R,
    /// Where questions go.
    pub out: W,
}

impl<R: BufRead, W: Write> Ask for Prompt<R, W> {
    fn ask(&mut self, question: &str, default: &str) -> Result<String, String> {
        let _ = if default.is_empty() {
            write!(self.out, "{question}: ")
        } else {
            write!(self.out, "{question} [{default}]: ")
        };
        let _ = self.out.flush();
        let mut line = String::new();
        let read = self
            .input
            .read_line(&mut line)
            .map_err(|e| format!("onboard init: reading an answer: {e}"))?;
        if read == 0 {
            return Err("onboard init: no answer (the input ended)".to_string());
        }
        let answer = line.trim();
        Ok(if answer.is_empty() { default } else { answer }.to_string())
    }
}

/// What `init` works from: the homes, the config file and what serve would make of it.
pub struct Env {
    /// The homes.
    pub homes: Homes,
    /// The config file, which may not exist yet.
    pub config: PathBuf,
    /// Its text (empty when there is none).
    pub text: String,
    /// The onboarding controller's settings, when a `gonk.onboard` line is there.
    pub onboard: Option<onb::Config>,
    /// The QUIC door as serve resolves it.
    pub quic: QuicBind,
    /// The owner-only socket.
    pub socket: PathBuf,
    /// This machine's short host name.
    pub host: String,
}

/// Run `init`. `mint` mints the controller key under the name it is given and answers the
/// public half's PEM. Answers whether everything is in place: `false` when a line could not
/// be written for want of an answer (named in the output).
///
/// # Errors
///
/// A refused mint, a file that cannot be read or written, an answer that is not one.
pub fn run(
    flags: &InitFlags,
    env: &Env,
    ask: &mut dyn Ask,
    mint: &mut dyn FnMut(&str) -> Result<String, String>,
    out: &mut dyn Write,
) -> Result<bool, String> {
    let gonk_dir = env.homes.config.join("gonk").join("onboard");
    let (key, key_path, kinds_path, grants) = match &env.onboard {
        Some(c) => (
            c.key.clone(),
            c.controller_key.clone(),
            c.kinds.clone(),
            c.grants.clone(),
        ),
        None => (
            onb::DEFAULT_KEY.to_string(),
            gonk_dir.join("controller.pub.pem"),
            gonk_dir.join("kinds.ttl"),
            onb::DEFAULT_GRANTS.to_string(),
        ),
    };
    let shown = |p: &Path| shown(p, &env.homes.home);
    let mut complete = true;

    // 1. The controller key.
    let public = if key_path.exists() {
        let pem = std::fs::read_to_string(&key_path)
            .map_err(|e| format!("{}: {e}", key_path.display()))?;
        say(
            out,
            &format!(
                "kept      the controller key `{key}`: {} is there, so nothing was minted (if the \
                 keystore has lost `{key}`, remove that file and run init again)",
                shown(&key_path)
            ),
        );
        pem
    } else {
        let pem = mint(&key)?;
        write_new(&key_path, &pem)?;
        say(
            out,
            &format!(
                "minted    the controller key `{key}` into the keystore; its public half is {}",
                shown(&key_path)
            ),
        );
        pem
    };

    // 2. The kinds file.
    if kinds_path.exists() {
        say(
            out,
            &format!(
                "kept      the kinds file {} (init never overwrites it)",
                shown(&kinds_path)
            ),
        );
    } else {
        write_new(&kinds_path, onb::DEFAULT_KINDS)?;
        say(
            out,
            &format!(
                "wrote     the kinds file {} (worker, task-member: a new kind is an edit there)",
                shown(&kinds_path)
            ),
        );
    }

    // 3. The settings.
    let view = layers_view(&env.text);
    let mut lines: Vec<String> = Vec::new();
    let mut cert_dir: Option<PathBuf> = env.onboard.as_ref().and_then(|c| match &c.target {
        crate::mount::Target::Quic { certs, .. } => Some(certs.clone()),
        _ => None,
    });
    let onboard_line = values_for(&env.text, KEY).into_iter().next();
    if let Some(line) = &onboard_line {
        say(out, &format!("kept      {KEY} = \"{line}\""));
        if flags.layer_door.is_some() || flags.cert_dir.is_some() {
            say(
                out,
                "          (--layer-door and --cert-dir are not applied over a line that is \
                 there: edit config.toml to change it)",
            );
        }
    } else {
        let layer_default = view
            .as_ref()
            .map_or(DEFAULT_LAYER_DOOR.to_string(), |(door, _)| door.clone());
        let layer = match &flags.layer_door {
            Some(door) => door.clone(),
            None => ask.ask("the layer host's door (HOST:PORT)", &layer_default)?,
        };
        let layer = layer.strip_prefix("quic://").unwrap_or(&layer).to_string();
        onb::host_port(&layer).map_err(|why| format!("the layer door `{layer}`: {why}"))?;
        let cert_default = view.as_ref().map_or(String::new(), |(_, dir)| dir.clone());
        let dir = match &flags.cert_dir {
            Some(dir) => dir.clone(),
            None => ask.ask(
                "the controller's certificate set at that door (client.crt, client.key, \
                 server.crt)",
                &cert_default,
            )?,
        };
        let path = crate::config::expand_home(&dir, &env.homes.home);
        match cert_set_problem(&dir, &path) {
            Some(why) => {
                complete = false;
                say(
                    out,
                    &format!(
                        "NOT added {KEY}: {why}. Run init again with --cert-dir DIR (the \
                         certificate set of the participant the layer host names as \
                         `onboard.controller`)"
                    ),
                );
            }
            None => {
                lines.push(format!("{KEY} = \"quic://{layer} {dir}\""));
                cert_dir = Some(path);
            }
        }
    }
    let have_onboard = onboard_line.is_some() || !lines.is_empty();
    let door = match values_for(&env.text, DOOR_KEY).into_iter().next() {
        Some(value) => {
            say(out, &format!("kept      {DOOR_KEY} = \"{value}\""));
            if flags.door.as_deref().is_some_and(|d| d != value) {
                say(
                    out,
                    "          (--door is not applied over a line that is there: edit \
                     config.toml to change it)",
                );
            }
            onb::parse_door(&value)?
        }
        None => {
            let port = env.quic.addr().map_or(DEFAULT_PORT, |a| a.port());
            let default = format!("{}.local:{port}", env.host);
            let answer = match &flags.door {
                Some(door) => door.clone(),
                None => choose(ask, &default)?,
            };
            let door = onb::parse_door(&answer)?;
            if have_onboard {
                lines.push(format!("{DOOR_KEY} = \"{answer}\""));
            } else {
                complete = false;
                say(
                    out,
                    &format!("NOT added {DOOR_KEY} = \"{answer}\": it needs the {KEY} line"),
                );
            }
            door
        }
    };
    if !lines.is_empty() {
        let existing = env.config.exists().then_some(env.text.as_str());
        let backup = crate::checkout::append_lines(&env.config, existing, &lines)?;
        for line in &lines {
            say(out, &format!("added     {line}"));
        }
        say(out, &format!("          to {}", shown(&env.config)));
        if let Some(backup) = backup {
            say(out, &format!("backup    {}", shown(&backup)));
        }
        say(
            out,
            "gonk reads these at start: restart gonk to serve onboarding with them.",
        );
    }

    // How nodes reach gonk, and whether that door opens.
    say(out, "");
    for line in reach(&door, env) {
        say(out, &line);
    }

    // 4. The layer host's table.
    say(out, "");
    say(
        out,
        &edge_table(&grants, cert_dir.as_deref(), &key_path, &public),
    );
    Ok(complete)
}

/// The menu: locally (the socket) or from elsewhere (QUIC, at an address defaulting to
/// `default`). An answer that is itself a door (`socket`, `HOST:PORT`) is taken as one.
fn choose(ask: &mut dyn Ask, default: &str) -> Result<String, String> {
    let menu = "how do nodes reach this gonk?\n  \
                1  locally: nodes on this machine, over gonk's owner-only socket (no door is \
                advertised)\n  \
                2  from elsewhere: other machines, over gonk's QUIC door\n\
                choose 1 or 2";
    let choice = ask.ask(menu, "2")?;
    match choice.as_str() {
        "1" | "locally" | SOCKET_DOOR => Ok(SOCKET_DOOR.to_string()),
        "2" => ask.ask(
            "this gonk's address for other machines (HOST:PORT)",
            default,
        ),
        other if onb::host_port(other).is_ok() => Ok(other.to_string()),
        other => Err(format!(
            "onboard init: `{other}` is neither 1 nor 2 (nor a door: HOST:PORT or `socket`)"
        )),
    }
}

/// What the chosen door means, and for QUIC whether gonk's door will actually open.
fn reach(door: &GonkDoor, env: &Env) -> Vec<String> {
    let socket = shown(&env.socket, &env.homes.home);
    match door {
        GonkDoor::Socket => vec![
            format!(
                "nodes reach gonk locally, over its owner-only socket {socket}; no door is \
                 advertised."
            ),
            format!(
                "  on the node, in its starter checkout: python3 tools/gonk.py init --socket \
                 {socket}"
            ),
            "  ⚠ the socket carries the owner's authority: an entitlement's gonk grant does not \
             bound a node that uses it."
                .to_string(),
        ],
        _ => {
            let Some(at) = door.fallback() else {
                return vec!["no door is advertised: gonk serves no QUIC door.".to_string()];
            };
            let mut lines = vec![format!(
                "nodes reach gonk from elsewhere, at {at} (gonk's QUIC door)."
            )];
            let layout = crate::quic::Layout::in_config_home(&env.homes.config);
            let enrolled = crate::quic::read_enrolment(&layout.clients_json())
                .ok()
                .flatten()
                .is_some_and(|e| e.admits_certificates());
            match env.quic {
                QuicBind::Explicit(addr) if addr.ip().is_loopback() => lines.push(format!(
                    "  ⚠ gonk.quic.bind is {addr}, a loopback address: no other machine can \
                     reach it. Set gonk.quic.bind = \"0.0.0.0:{}\".",
                    addr.port()
                )),
                QuicBind::Explicit(addr) => lines.push(format!(
                    "  the QUIC door is configured: gonk.quic.bind = \"{addr}\"{}",
                    if enrolled {
                        "."
                    } else {
                        ", and with no client certificate enrolled gonk refuses to start until \
                         one is (`ikigai-gonk client add`)."
                    }
                )),
                QuicBind::Default(addr) if enrolled => lines.push(format!(
                    "  the QUIC door opens on udp {addr} (no gonk.quic.bind line; a client \
                     certificate is enrolled)."
                )),
                QuicBind::Default(addr) => lines.push(format!(
                    "  the QUIC door is not open yet: with no gonk.quic.bind line it opens on udp \
                     {addr} once a client certificate is enrolled. The first approval enrolls one; \
                     restart gonk after it, as `onboard status` says."
                )),
                QuicBind::Off => {}
            }
            if let (Some(bound), Some((_, port))) = (env.quic.addr(), at.rsplit_once(':')) {
                if port != bound.port().to_string() {
                    lines.push(format!(
                        "  note: the door names port {port} and gonk binds {}: right only if \
                         something forwards one to the other.",
                        bound.port()
                    ));
                }
            }
            lines
        }
    }
}

/// The layer host's `[onboard]` table, ready to paste, and the PEM to put beside it. What gonk
/// cannot know is left COMMENTED OUT and marked, so the host refuses the table (both keys are
/// required) rather than taking a guess.
pub fn edge_table(grants: &str, cert_dir: Option<&Path>, key_path: &Path, public: &str) -> String {
    let file = key_path
        .file_name()
        .map_or("controller.pub.pem".to_string(), |f| {
            f.to_string_lossy().into_owned()
        });
    let controller = match cert_dir.and_then(participant_of) {
        Some(name) => format!("controller     = \"{name}\""),
        None => format!(
            "# controller   = \"<participant>\"   # ⚠ SET THIS: the participant whose certificate \
             set is {}",
            cert_dir.map_or("the one in gonk.onboard".to_string(), |d| d
                .display()
                .to_string())
        ),
    };
    format!(
        "the layer host's [onboard] table, for its layers.toml:\n\n\
         [onboard]\n\
         grants         = \"{grants}\"\n\
         {controller}\n\
         controller_key = \"{file}\"\n\
         # trust_dir    = \"<the door's quic cert dir>/clients\"   # ⚠ SET THIS: the door's OWN client \
         directory; with any other, an approved node stays in the lobby and nothing says why\n\
         trust_reload   = \"live\"\n\n\
         and {file} beside layers.toml (the public half; the key stays in this machine's keystore):\n\n\
         {public}{}",
        if public.ends_with('\n') { "" } else { "\n" }
    )
}

/// The participant a certificate set belongs to, when its directory says: the layers client
/// keeps one set per participant at `…/layers-client/<name>`.
fn participant_of(dir: &Path) -> Option<String> {
    let parent = dir.parent()?.file_name()?.to_str()?;
    let name = dir.file_name()?.to_str()?;
    (parent == "layers-client"
        && !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'))
    .then(|| name.to_string())
}

/// Why `dir` (spelled) is not a certificate set, if it is not one.
fn cert_set_problem(spelled: &str, path: &Path) -> Option<String> {
    if spelled.is_empty() {
        return Some("no certificate set was given".to_string());
    }
    if spelled.contains('"') || spelled.contains(char::is_whitespace) {
        return Some(format!(
            "`{spelled}` holds a quote or a space, which the line cannot carry"
        ));
    }
    let missing: Vec<&str> = ["client.crt", "client.key", "server.crt"]
        .into_iter()
        .filter(|f| !path.join(f).is_file())
        .collect();
    (!missing.is_empty()).then(|| {
        format!(
            "{} is not a certificate set (no {})",
            path.display(),
            missing.join(", ")
        )
    })
}

/// The `gonk.layers.view` line's door and certificate directory, as spelled.
fn layers_view(text: &str) -> Option<(String, String)> {
    let line = values_for(text, crate::layers_view::KEY)
        .into_iter()
        .next()?;
    let mut words = line.split_whitespace();
    let door = words.next()?.strip_prefix("quic://")?.to_string();
    let dir = words.next()?.to_string();
    Some((door, dir))
}

/// Write a file that does not exist yet, its directory included.
fn write_new(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// A path as the operator reads it: `~/…` under the home directory.
fn shown(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

fn say(out: &mut dyn Write, line: &str) {
    let _ = writeln!(out, "{line}");
}
