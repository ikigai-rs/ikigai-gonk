//! Riding out a restart or a short outage (ledger [#3811](http://localhost:1062/l/default/item/3811)):
//! one setting for how long a page keeps trying, and a server-rendered read that tries again
//! before it says the store is unavailable.
//!
//! A restart of gonk, or a short outage of its store, used to show a person an error at once, in
//! three places. Each now waits, with the Layers watcher's behavior (`web/gonk.js`, ledger #1705)
//! as the model: try again with backoff, say so once, and stop on an answer no retry can fix.
//!
//! - **htmx requests** are ikigai-ui's shell's (0.5.1, ledger #3810). It reads two attributes on
//!   `<html>`, in milliseconds, which gonk writes from [`LIMIT_KEY`] and [`BASE_KEY`] on its own
//!   pages ([`crate::render::render`]) and on every framed face ([`crate::face_frame`]), so a face
//!   and a gonk page give up at the same moment.
//! - **A panel's read** ([`read`]): a few attempts inside the request, with short backoff,
//!   before the panel renders its "the store is unavailable" note.
//! - **A whole page** that is a `502`, `503` or `504` ([`crate::refusal`]): it reloads itself
//!   with the shell's backoff until the same limit, and says in `#status` that it is waiting.
//!
//! The settings are read once, at start, and installed here ([`install`]) like the process's
//! instance id ([`crate::instance`]): every full page is drawn through one function that has no
//! configuration to hand, and the value cannot change while the process runs.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Kernel, Representation,
    Request, Resolution, Result, Scope, Space,
};
use ikigai_throttle::Retry;

/// How long after the first failure a page stops retrying and shows the error.
pub const LIMIT_KEY: &str = "gonk.ui.retry.limit";
/// The first wait's ceiling, doubled on each retry (ikigai-ui caps it at 30 s).
pub const BASE_KEY: &str = "gonk.ui.retry.base";

/// [`LIMIT_KEY`]'s default: ikigai-ui's own, and the `ikigai` command line's.
pub const DEFAULT_LIMIT: Duration = Duration::from_secs(90);
/// [`BASE_KEY`]'s default: ikigai-ui's own.
pub const DEFAULT_BASE: Duration = Duration::from_secs(1);
/// The longest limit accepted: a page that waits longer than this for a restart is waiting for
/// an operator, and should say so.
pub const MAX_LIMIT: Duration = Duration::from_secs(60 * 60);

/// How long a page keeps trying ([`LIMIT_KEY`], [`BASE_KEY`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// How long after the first failure a page gives up; zero turns retrying off.
    pub limit: Duration,
    /// The first wait's ceiling.
    pub base: Duration,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            limit: DEFAULT_LIMIT,
            base: DEFAULT_BASE,
        }
    }
}

impl Settings {
    /// The two lines as spelled, `None` for a line that is absent. A duration is ikigai-time's
    /// (`90s`, `1500ms`, `2m`); the limit may also be `off`, which turns retrying off.
    ///
    /// ```
    /// use std::time::Duration;
    /// use ikigai_gonk::ui_retry::Settings;
    /// assert_eq!(Settings::parse(None, None).unwrap(), Settings::default());
    /// let s = Settings::parse(Some("2m"), Some("500ms")).unwrap();
    /// assert_eq!((s.limit, s.base), (Duration::from_secs(120), Duration::from_millis(500)));
    /// assert_eq!(Settings::parse(Some("off"), None).unwrap().limit, Duration::ZERO);
    /// assert!(Settings::parse(Some("soon"), None).is_err());
    /// assert!(Settings::parse(None, Some("0s")).is_err());
    /// assert!(Settings::parse(Some("10s"), Some("20s")).is_err());
    /// ```
    pub fn parse(limit: Option<&str>, base: Option<&str>) -> std::result::Result<Settings, String> {
        let limit = match limit.map(str::trim) {
            None => DEFAULT_LIMIT,
            Some(off) if off.eq_ignore_ascii_case("off") => Duration::ZERO,
            Some(spelled) => duration(LIMIT_KEY, spelled, "or `off` to show a failure at once")?,
        };
        if limit > MAX_LIMIT {
            return Err(format!(
                "{LIMIT_KEY}: {} s is longer than an hour; a page waiting that long for gonk \
                 is waiting for an operator",
                limit.as_secs()
            ));
        }
        let base = match base.map(str::trim) {
            None => DEFAULT_BASE,
            Some(spelled) => duration(BASE_KEY, spelled, "")?,
        };
        if !limit.is_zero() && base > limit {
            return Err(format!(
                "{BASE_KEY}: the first wait ({} ms) is longer than {LIMIT_KEY} ({} ms), so a \
                 page would give up before it tried again",
                base.as_millis(),
                limit.as_millis()
            ));
        }
        Ok(Settings { limit, base })
    }

    /// The attributes ikigai-ui's shell reads on `<html>`, each with a leading space.
    ///
    /// ```
    /// use ikigai_gonk::ui_retry::Settings;
    /// assert_eq!(
    ///     Settings::default().attributes(),
    ///     " data-ui-retry-limit='90000' data-ui-retry-base='1000'"
    /// );
    /// ```
    pub fn attributes(&self) -> String {
        format!(
            " data-ui-retry-limit='{}' data-ui-retry-base='{}'",
            self.limit.as_millis(),
            self.base.as_millis()
        )
    }
}

fn duration(key: &str, spelled: &str, or: &str) -> std::result::Result<Duration, String> {
    ikigai_time::parse_schedule(spelled)
        .map(|schedule| schedule.interval())
        .map_err(|e| {
            let or = if or.is_empty() {
                String::new()
            } else {
                format!(", {or}")
            };
            format!("{key}: `{spelled}` is not a duration: {e} — like `90s`, `1500ms` or `2m`{or}")
        })
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Make `settings` the ones every page carries. `main` calls it once, at start, before a door
/// opens; a second call changes nothing.
pub fn install(settings: Settings) {
    let _ = SETTINGS.set(settings);
}

/// The installed settings, or the defaults before [`install`] (a test's server).
pub fn settings() -> Settings {
    SETTINGS.get().copied().unwrap_or_default()
}

// ------------------------------------------------------------------- a panel's read

/// How many times a panel's read is sent in all, the first included.
pub const READ_ATTEMPTS: u32 = 4;
/// The first backoff's ceiling, and the most any one wait may be: full jitter under
/// 100 ms, 200 ms, 400 ms.
pub const READ_BACKOFF: (Duration, Duration) = (Duration::from_millis(100), Duration::from_secs(1));
/// All attempts and waits together. A panel is one part of a page a person is waiting on, so
/// this is short; the run's own deadline, when the door set one, can only lower it.
pub const READ_TIME_LIMIT: Duration = Duration::from_secs(3);

/// Where a panel's read goes: the invocation's own kernel, or one named.
#[derive(Clone)]
pub enum Via {
    /// `inv.issue(request)`: the door's kernel, under the run's capability.
    Run,
    /// The hub, under the run's capability and with the run's deadline.
    Hub(Arc<Kernel>),
}

/// Read `request` for a panel, under `inv`'s capability, trying again while the store is
/// briefly unavailable: [`ikigai_throttle::Retry`] decides, over a one-endpoint space that
/// forwards the read, so its class table, backoff with full jitter and bound on the run's
/// remaining time are the ones every ikigai retry uses. The overlay sees the panel's OWN
/// invocation, so a write is never resent (it re-sends only for an idempotent verb) and the
/// request's deadline bounds every wait.
///
/// What is tried again, decided here on purpose:
///
/// - **`Unavailable`** (the store restarting, a peer briefly gone): yes, with backoff.
/// - **`RetryAfter`**: once, when its delay fits the time left. It is the answer of a limiter,
///   a rate window or an open circuit, and ⚠ not `is_transient()` (see
///   [`crate::budget::Window::admit`]): waiting it out once honors it; asking again in a loop
///   would only spend the window. A second one is returned as it is.
/// - **`Timeout`** never, though core classes it transient: on a panel's read it is the
///   caller's own store time budget (`urn:cap:store:budget:<ms>`), and a repeat under the same
///   budget costs the same and fails the same. Throttle's table cannot say this, so the read
///   holds it back from the overlay.
/// - Everything else, a denial or a refusal included, on the first failure.
///
/// The error returned is the last one the read itself met, never the overlay's summary
/// (`GaveUp`), so a panel's note still says why in the words it always has.
pub async fn read(inv: &Invocation<'_>, request: Request, via: Via) -> Result<Representation> {
    let forward = Arc::new(Forward {
        request: request.clone(),
        via,
        seen: Mutex::new(Seen::default()),
    });
    let space = EndpointSpace::new().bind_arc(
        Exact::new(request.target.as_str()),
        Arc::clone(&forward) as Arc<dyn Endpoint>,
    );
    let retry = Retry::new(space, READ_ATTEMPTS)
        .with_backoff(READ_BACKOFF.0, READ_BACKOFF.1)
        .with_time_limit(READ_TIME_LIMIT)
        .with_max_wait(READ_TIME_LIMIT);
    let Resolution::Hit(hit) = retry.resolve(&request, &Scope::empty()) else {
        unreachable!("the space binds the request's own target");
    };
    let answer = hit.endpoint.invoke(inv).await;
    answer.map_err(|overlay| forward.last().unwrap_or(overlay))
}

#[derive(Default)]
struct Seen {
    /// The last error the read met, as it met it.
    last: Option<Error>,
    /// How many `RetryAfter`s it has met.
    retry_afters: u32,
}

/// The one endpoint [`read`]'s overlay re-invokes: the read, sent where [`Via`] says.
struct Forward {
    request: Request,
    via: Via,
    seen: Mutex<Seen>,
}

impl Forward {
    fn last(&self) -> Option<Error> {
        self.seen.lock().ok()?.last.take()
    }

    /// `error` as the overlay is shown it: unchanged when it may decide, or a permanent
    /// stand-in for a failure [`read`] does not try again (the real one is kept for the caller).
    fn shown(&self, error: Error) -> Error {
        let mut seen = match self.seen.lock() {
            Ok(seen) => seen,
            Err(poisoned) => poisoned.into_inner(),
        };
        let held = match &error {
            Error::Timeout(_) => true,
            Error::RetryAfter { .. } => {
                seen.retry_afters += 1;
                seen.retry_afters > 1
            }
            _ => false,
        };
        seen.last = Some(error.clone());
        if held {
            Error::Endpoint("held back from the retry".to_string())
        } else {
            error
        }
    }
}

#[async_trait]
impl Endpoint for Forward {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let request = self.request.clone();
        let answer = match &self.via {
            Via::Run => inv.issue(request).await,
            Via::Hub(hub) => {
                hub.issue_with_context(request, inv.capability, inv.issue_context().clone())
                    .await
            }
        };
        answer.map_err(|error| self.shown(error))
    }

    fn name(&self) -> &str {
        "gonk-panel-read"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-panel-read")
            .summary("a panel's read, tried again while the store is briefly unavailable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{Capability, FnEndpoint, Iri, ReprType, Verb};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A kernel whose `urn:test:read` fails with each of `failures` in turn, then answers `ok`,
    /// and whose `urn:test:panel` reads it through [`read`].
    fn flaky(failures: Vec<Error>) -> (Kernel, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let failures = Mutex::new(failures.into_iter());
        let flaky = FnEndpoint::new("flaky", move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            match failures.lock().unwrap().next() {
                Some(error) => Err(error),
                None => Ok(Representation::new(
                    ReprType::new("text/plain"),
                    b"ok".to_vec(),
                )),
            }
        });
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:test:read"), flaky)
            .bind(Exact::new("urn:test:panel"), Panel);
        (Kernel::new(Arc::new(space)), calls)
    }

    /// A panel: it reads `urn:test:read` through [`read`] and answers what that answered.
    struct Panel;

    #[async_trait]
    impl Endpoint for Panel {
        async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
            let read = Request::new(Verb::Source, Iri::parse("urn:test:read").unwrap());
            super::read(inv, read, Via::Run).await
        }
    }

    fn panel(kernel: &Kernel) -> Result<Representation> {
        let ask = Request::new(Verb::Source, Iri::parse("urn:test:panel").unwrap());
        block_on(kernel.issue(ask, &Capability::root()))
    }

    #[test]
    fn an_unavailable_store_is_asked_again_until_it_answers() {
        let (kernel, calls) = flaky(vec![
            Error::Unavailable("restarting".into()),
            Error::Unavailable("restarting".into()),
        ]);
        assert_eq!(panel(&kernel).unwrap().bytes, b"ok");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_store_that_stays_down_is_its_own_error_after_a_few_attempts() {
        let down = || Error::Unavailable("down".into());
        let (kernel, calls) = flaky((0..10).map(|_| down()).collect());
        assert_eq!(
            panel(&kernel).unwrap_err(),
            down(),
            "not the overlay's GaveUp"
        );
        assert_eq!(calls.load(Ordering::SeqCst), READ_ATTEMPTS);
    }

    #[test]
    fn a_time_budget_and_a_refusal_are_never_asked_again() {
        for error in [
            Error::Timeout("over your budget".into()),
            Error::Denied("no".into()),
            Error::OverBudget {
                detail: "too big".into(),
            },
        ] {
            let (kernel, calls) = flaky(vec![error.clone()]);
            assert_eq!(panel(&kernel).unwrap_err(), error);
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{error:?}");
        }
    }

    #[test]
    fn a_short_retry_after_is_waited_out_once_and_never_in_a_loop() {
        let after = || Error::RetryAfter {
            delay: Some(Duration::from_millis(20)),
            detail: "limited".into(),
        };
        let (kernel, calls) = flaky(vec![after()]);
        assert_eq!(panel(&kernel).unwrap().bytes, b"ok");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let (kernel, calls) = flaky(vec![after(), after(), after()]);
        assert_eq!(panel(&kernel).unwrap_err(), after());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "the second is returned");
        // A delay longer than the read may take is the caller's to wait, not the panel's.
        let long = Error::RetryAfter {
            delay: Some(READ_TIME_LIMIT * 2),
            detail: "limited".into(),
        };
        let (kernel, calls) = flaky(vec![long.clone()]);
        assert_eq!(panel(&kernel).unwrap_err(), long);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_attributes_carry_the_configured_milliseconds() {
        let s = Settings::parse(Some("45s"), Some("250ms")).unwrap();
        assert_eq!(
            s.attributes(),
            " data-ui-retry-limit='45000' data-ui-retry-base='250'"
        );
        let off = Settings::parse(Some("off"), None).unwrap();
        assert!(off.attributes().contains("data-ui-retry-limit='0'"));
    }

    #[test]
    fn a_malformed_or_impossible_value_is_refused_naming_its_key() {
        for (limit, base, key) in [
            (Some("ninety"), None, LIMIT_KEY),
            (Some("-5s"), None, LIMIT_KEY),
            (Some("2d"), None, LIMIT_KEY),
            (Some("2h"), None, LIMIT_KEY),
            (None, Some("0"), BASE_KEY),
            (None, Some("fast"), BASE_KEY),
            (Some("5s"), Some("10s"), BASE_KEY),
        ] {
            let refused = Settings::parse(limit, base).unwrap_err();
            assert!(refused.starts_with(key), "{refused}");
        }
    }
}
