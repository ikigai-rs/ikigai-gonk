//! Riding out a restart or a short outage (ledger #3811): one setting for gonk's pages and the
//! faces it frames, and the server-side half, a panel read retried inside its request.
//!
//! - **The setting.** [`LIMIT_KEY`] (default 90 s) and [`BASE_KEY`] (default 1 s), checked at
//!   start ([`UiRetry::parse`]). `main` installs them once ([`install`]); every full gonk page
//!   (`crate::render`) and every framed face (`crate::face_frame`) then carries them on `<html>`
//!   as `data-ui-retry-limit` and `data-ui-retry-base`, in milliseconds ([`attributes`]), which
//!   ikigai-ui 0.5.1's shell script reads at each failure. A face and a gonk page give up at the
//!   same moment, and an error page that reloads itself (`web/gonk.js`) waits by the same two.
//! - **A panel read** ([`reread`]): a read that meets `Unavailable` is asked again, a few times
//!   with short doubling waits, inside the request's own time; only then is the panel's note
//!   drawn.

use std::future::Future;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ikigai_core::{Error, Result};

/// How long after the first failure a page stops retrying and shows the error.
pub const LIMIT_KEY: &str = "gonk.ui.retry.limit";
/// The first wait's ceiling; each retry doubles it (the shell caps a wait at 30 s).
pub const BASE_KEY: &str = "gonk.ui.retry.base";

/// The limit with no [`LIMIT_KEY`] line: ikigai-ui's own default.
pub const DEFAULT_LIMIT: Duration = Duration::from_secs(90);
/// The base with no [`BASE_KEY`] line: ikigai-ui's own default.
pub const DEFAULT_BASE: Duration = Duration::from_secs(1);
/// The longest limit accepted: past an hour, a page that never gives up is a typo.
pub const MAX_LIMIT: Duration = Duration::from_secs(3600);
/// The longest base accepted: ikigai-ui caps every wait at 30 s, so a longer base means nothing.
pub const MAX_BASE: Duration = Duration::from_secs(30);

/// The two settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiRetry {
    /// How long after the first failure retrying stops; zero turns it off.
    pub limit: Duration,
    /// The first wait's ceiling.
    pub base: Duration,
}

impl Default for UiRetry {
    fn default() -> Self {
        UiRetry {
            limit: DEFAULT_LIMIT,
            base: DEFAULT_BASE,
        }
    }
}

impl UiRetry {
    /// The settings the two lines spell, refused when either is malformed or out of range.
    ///
    /// ```
    /// use std::time::Duration;
    /// use ikigai_gonk::ui_retry::UiRetry;
    /// assert_eq!(UiRetry::parse(None, None), Ok(UiRetry::default()));
    /// let set = UiRetry::parse(Some("2m"), Some("500ms")).unwrap();
    /// assert_eq!(set.limit, Duration::from_secs(120));
    /// assert_eq!(set.base, Duration::from_millis(500));
    /// assert_eq!(UiRetry::parse(Some("off"), None).unwrap().limit, Duration::ZERO);
    /// assert!(UiRetry::parse(Some("soon"), None).is_err());
    /// assert!(UiRetry::parse(Some("90"), None).is_err());
    /// assert!(UiRetry::parse(None, Some("0s")).is_err());
    /// assert!(UiRetry::parse(None, Some("1m")).is_err());
    /// ```
    pub fn parse(limit: Option<&str>, base: Option<&str>) -> std::result::Result<UiRetry, String> {
        let limit = match limit.map(str::trim) {
            None => DEFAULT_LIMIT,
            Some(off) if off.eq_ignore_ascii_case("off") => Duration::ZERO,
            Some(spelled) => {
                let limit = duration(LIMIT_KEY, spelled)?;
                if limit > MAX_LIMIT {
                    return Err(format!(
                        "{LIMIT_KEY}: `{spelled}` is over an hour; a page that waits that long \
                         for a server looks broken. Use a duration like `90s` or `5m`, or `off`"
                    ));
                }
                limit
            }
        };
        let base = match base.map(str::trim) {
            None => DEFAULT_BASE,
            Some(spelled) => {
                let base = duration(BASE_KEY, spelled)?;
                if base.is_zero() || base > MAX_BASE {
                    return Err(format!(
                        "{BASE_KEY}: `{spelled}` is not between 1ms and 30s; it is the first \
                         wait before a retry, and ikigai-ui caps every wait at 30s"
                    ));
                }
                base
            }
        };
        Ok(UiRetry { limit, base })
    }
}

/// `spelled` as a whole number of `ms`, `s` or `m`.
fn duration(key: &str, spelled: &str) -> std::result::Result<Duration, String> {
    let refused = || {
        format!(
            "{key}: `{spelled}` is not a duration: a whole number with `ms`, `s` or `m`, like \
             `90s`, `1s` or `500ms`"
        )
    };
    let split = spelled
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(refused)?;
    let (number, unit) = spelled.split_at(split);
    let number: u64 = number.parse().map_err(|_| refused())?;
    match unit {
        "ms" => Ok(Duration::from_millis(number)),
        "s" => Ok(Duration::from_secs(number)),
        "m" => number
            .checked_mul(60)
            .map(Duration::from_secs)
            .ok_or_else(refused),
        _ => Err(refused()),
    }
}

static SETTING: OnceLock<UiRetry> = OnceLock::new();

/// Install this process's settings. `main` calls it once, before a door opens; a second call
/// keeps the first.
pub fn install(setting: UiRetry) {
    let _ = SETTING.set(setting);
}

/// This process's settings: the installed ones, or the defaults.
pub fn setting() -> UiRetry {
    SETTING.get().copied().unwrap_or_default()
}

/// The two `<html>` attributes, each with a leading space, in milliseconds.
///
/// ```
/// use std::time::Duration;
/// use ikigai_gonk::ui_retry::{attributes_of, UiRetry};
/// assert_eq!(
///     attributes_of(UiRetry::default()),
///     " data-ui-retry-limit='90000' data-ui-retry-base='1000'"
/// );
/// ```
pub fn attributes_of(setting: UiRetry) -> String {
    format!(
        " data-ui-retry-limit='{}' data-ui-retry-base='{}'",
        setting.limit.as_millis(),
        setting.base.as_millis()
    )
}

/// [`attributes_of`] this process's [`setting`].
pub fn attributes() -> String {
    attributes_of(setting())
}

/// How many times a panel read is asked in all.
pub const READ_ATTEMPTS: u32 = 4;
/// The first wait before a panel read is asked again; each wait after doubles it.
pub const READ_BASE: Duration = Duration::from_millis(100);
/// The most a panel read waits in all when its request carries no deadline.
pub const READ_MAX_WAIT: Duration = Duration::from_secs(2);

/// `read`, asked again while it fails in a way a short wait may cure, then its last answer.
///
/// - **`Unavailable`** (the store restarting, a worker gone): asked again after 100 ms, 200 ms,
///   400 ms ([`READ_ATTEMPTS`], [`READ_BASE`]).
/// - **`RetryAfter` with a delay**: waited out ONCE, and only when it fits. ★ A per-caller budget
///   refusal (`crate::budget`) is a `RetryAfter` too, and is deliberately not `is_transient()`:
///   a repeat under the same window fails the same, so it is never asked again in a loop. A
///   window that opens within the time left is waited for once; a longer one is the note.
/// - **Everything else** (`Timeout`, `Denied`, `OverBudget`, …) is the answer at once.
///
/// Never past `time_left` (the request's own deadline, [`ikigai_core::Invocation::time_left`]),
/// and at most [`READ_MAX_WAIT`] when it has none: a wait is taken only when it leaves as much
/// time again for the read it waits for.
///
/// ★ Not `ikigai_throttle::Retry` (0.4.0): it is a `Space` overlay, wrapping endpoints when they
/// resolve, while a panel's read here is one or several issues composed in Rust (the Disk part
/// reads two resources, the hub's readouts go through another kernel); and it re-issues an
/// `Unavailable` AT ONCE, with no wait, so a store that takes a second to come back would spend
/// every attempt in microseconds. Its rules are kept: only reads, only the transient class looped,
/// a `RetryAfter` waited only when it fits the run's time left.
pub async fn reread<T, F, Fut>(time_left: Option<Duration>, mut read: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let started = Instant::now();
    let budget = time_left.map_or(READ_MAX_WAIT, |left| left.min(READ_MAX_WAIT));
    let mut waited_out = false;
    let mut attempt = 0;
    loop {
        let error = match read().await {
            Ok(answer) => return Ok(answer),
            Err(error) => error,
        };
        attempt += 1;
        if attempt >= READ_ATTEMPTS {
            return Err(error);
        }
        let wait = match &error {
            Error::Unavailable(_) => READ_BASE * 2u32.pow(attempt - 1),
            Error::RetryAfter {
                delay: Some(delay), ..
            } if !waited_out => {
                waited_out = true;
                *delay
            }
            _ => return Err(error),
        };
        let left = budget.saturating_sub(started.elapsed());
        if wait.saturating_mul(2) > left {
            return Err(error);
        }
        futures_timer::Delay::new(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn block<T>(f: impl Future<Output = T>) -> T {
        futures::executor::block_on(f)
    }

    #[test]
    fn an_unavailable_read_is_asked_again_and_its_answer_returned() {
        let asked = Cell::new(0);
        let answer = block(reread(None, || {
            asked.set(asked.get() + 1);
            let n = asked.get();
            async move {
                if n == 1 {
                    Err(Error::Unavailable("restarting".into()))
                } else {
                    Ok("content")
                }
            }
        }));
        assert_eq!(answer.unwrap(), "content");
        assert_eq!(asked.get(), 2);
    }

    #[test]
    fn a_read_that_stays_unavailable_is_asked_a_few_times_then_returned() {
        let asked = Cell::new(0);
        let answer: Result<()> = block(reread(None, || {
            asked.set(asked.get() + 1);
            async { Err(Error::Unavailable("down".into())) }
        }));
        assert!(matches!(answer, Err(Error::Unavailable(_))));
        assert_eq!(asked.get(), READ_ATTEMPTS);
    }

    #[test]
    fn a_budget_refusal_is_waited_out_at_most_once_and_never_past_the_time_left() {
        let asked = Cell::new(0);
        let short: Result<()> = block(reread(Some(Duration::from_secs(1)), || {
            asked.set(asked.get() + 1);
            async {
                Err(Error::RetryAfter {
                    delay: Some(Duration::from_millis(10)),
                    detail: "window".into(),
                })
            }
        }));
        assert!(short.is_err());
        assert_eq!(asked.get(), 2, "one wait, then the refusal");

        asked.set(0);
        let long: Result<()> = block(reread(Some(Duration::from_secs(1)), || {
            asked.set(asked.get() + 1);
            async {
                Err(Error::RetryAfter {
                    delay: Some(Duration::from_secs(3600)),
                    detail: "window".into(),
                })
            }
        }));
        assert!(long.is_err());
        assert_eq!(asked.get(), 1, "an hour does not fit a second");
    }

    #[test]
    fn other_failures_and_a_spent_deadline_are_answered_at_once() {
        for error in [Error::Timeout("slow".into()), Error::Denied("no".into())] {
            let asked = Cell::new(0);
            let kind = std::mem::discriminant(&error);
            let error = Cell::new(Some(error));
            let answer: Result<()> = block(reread(None, || {
                asked.set(asked.get() + 1);
                let e = error.take().unwrap();
                async move { Err(e) }
            }));
            assert_eq!(std::mem::discriminant(&answer.unwrap_err()), kind);
            assert_eq!(asked.get(), 1);
        }
        let asked = Cell::new(0);
        let _: Result<()> = block(reread(Some(Duration::from_millis(50)), || {
            asked.set(asked.get() + 1);
            async { Err(Error::Unavailable("down".into())) }
        }));
        assert_eq!(asked.get(), 1, "no wait fits 50 ms twice over");
    }
}
