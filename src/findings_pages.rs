//! Browse's bounded JSON findings pages, shared by its row consumers.
//!
//! Follow `next`, never the row count: browse may trim a page to its byte ceiling.
//! A failed later page fails the whole read; callers must not cache partial answers.

use ikigai_core::{ArgRef, Capability, Kernel, Request};
use serde_json::Value;

/// The largest row count browse accepts. Byte trimming can yield a smaller page.
const LIMIT: &[u8] = b"2000";

pub(crate) struct Pages {
    request: Request,
    after: Option<String>,
    rows: Vec<Value>,
}

impl Pages {
    pub(crate) fn new(request: Request) -> Self {
        Self {
            request: request.with_arg("limit", ArgRef::Inline(LIMIT.to_vec())),
            after: None,
            rows: Vec::new(),
        }
    }

    pub(crate) fn request(&self) -> Request {
        let mut request = self.request.clone();
        if let Some(after) = &self.after {
            request = request.with_arg("after", ArgRef::Inline(after.as_bytes().to_vec()));
        }
        request
    }

    /// Accept one page, returning whether another must be read.
    pub(crate) fn accept(&mut self, bytes: &[u8]) -> Result<bool, String> {
        let invalid = || {
            format!(
                "`{}` answered an invalid JSON findings page",
                self.request.target
            )
        };
        let mut page: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let next = match page.get("next") {
            Some(Value::Null) => None,
            Some(Value::String(next)) => Some(next.clone()),
            _ => return Err(invalid()),
        };
        let rows = page
            .get_mut("rows")
            .and_then(Value::as_array_mut)
            .ok_or_else(invalid)?;
        if let Some(next) = &next {
            // A cursor must make progress and name the last row returned. Refuse a
            // broken envelope instead of looping forever or quietly losing findings.
            if next.len() != 24
                || !next.bytes().all(|b| b.is_ascii_hexdigit())
                || self.after.as_ref().is_some_and(|after| next <= after)
                || rows
                    .last()
                    .and_then(|row| row.get("id"))
                    .and_then(Value::as_str)
                    != Some(next.as_str())
            {
                return Err(invalid());
            }
        }
        self.rows.append(rows);
        let more = next.is_some();
        self.after = next;
        Ok(more)
    }

    pub(crate) fn rows(self) -> Vec<Value> {
        self.rows
    }
}

pub(crate) async fn read(
    hub: &Kernel,
    capability: &Capability,
    request: Request,
) -> Result<Vec<Value>, String> {
    let mut pages = Pages::new(request);
    loop {
        let answer = hub
            .issue(pages.request(), capability)
            .await
            .map_err(|e| e.to_string())?;
        if !pages.accept(&answer.bytes)? {
            return Ok(pages.rows());
        }
    }
}

/// Paging uses ID order. Restore browse's triage order once every row is in hand.
/// The severity rank comes from the finding contract, as the queue's menu does.
pub(crate) fn triage(hub: &Kernel, rows: &mut [Value]) -> Result<(), String> {
    let severity = crate::queue::one_of(
        hub,
        &crate::queue::finding_iri(crate::queue::PROBE_ID),
        ikigai_core::Verb::Sink,
        "severity",
    )
    .ok_or_else(|| "the finding contract declares no severity set".to_string())?;
    let unrated = severity
        .iter()
        .position(|word| word == "info")
        .unwrap_or(severity.len());
    let rank = |row: &Value| {
        severity
            .iter()
            .position(|word| Some(word.as_str()) == crate::queue::rated(row))
            .unwrap_or(unrated)
    };
    fn position(row: &Value) -> (&str, u64, &str) {
        (
            row.get("path").and_then(Value::as_str).unwrap_or(""),
            row.get("start").and_then(Value::as_u64).unwrap_or(0),
            row.get("id").and_then(Value::as_str).unwrap_or(""),
        )
    }
    rows.sort_by(|a, b| (rank(a), position(a)).cmp(&(rank(b), position(b))));
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use async_trait::async_trait;
    use ikigai_core::{
        ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
        Representation, Verb,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    pub(crate) const FIRST: &str = "000000000000000000000001";
    pub(crate) const LAST: &str = "000000000000000000000002";
    pub(crate) type Calls = Arc<Mutex<Vec<Option<String>>>>;

    struct Listing {
        calls: Calls,
        fail_last: bool,
    }

    #[async_trait]
    impl Endpoint for Listing {
        async fn invoke(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
            assert!(inv.capability.allows("urn:test:findings"));
            assert!(!inv.capability.allows("urn:test:ungranted"));
            assert_eq!(inv.inline_str("as")?, "application/json");
            assert_eq!(inv.inline_str("state")?, "pending");
            assert_eq!(inv.inline_str("limit")?, "2000");
            let after = inv.inline_str("after").ok();
            self.calls.lock().unwrap().push(after.map(str::to_string));
            let page = match after {
                None => {
                    json!({"rows": [{"id": FIRST, "severity": "major", "path": "z.rs", "start": 4}], "next": FIRST})
                }
                Some(FIRST) if self.fail_last => {
                    return Err(Error::Conflict("later page failed".into()))
                }
                Some(FIRST) => {
                    json!({"rows": [{"id": LAST, "severity": "critical", "path": "a.rs", "start": 1}], "next": null})
                }
                _ => panic!("unexpected cursor: {after:?}"),
            };
            Ok(Representation::new(
                ReprType::new("application/json"),
                page.to_string().into_bytes(),
            ))
        }

        fn name(&self) -> &str {
            "test-findings-pages"
        }

        fn describe(&self) -> Description {
            let mut description = Description::new(self.name())
                .verb(Verb::Source)
                .verb(Verb::Sink);
            for name in ["as", "state", "limit", "after"] {
                description = description.input(ArgSpec::new(name).optional());
            }
            description.input(
                ArgSpec::new("severity")
                    .optional()
                    .one_of(["critical", "major", "minor", "info", "praise"]),
            )
        }
    }

    /// Two short pages simulate byte trimming; the scoped grant must survive both reads.
    pub(crate) fn hub(fail_last: bool) -> (Arc<Kernel>, Capability, Calls) {
        let calls = Calls::default();
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:repo:demo:findings"),
                Listing {
                    calls: Arc::clone(&calls),
                    fail_last,
                },
            )
            .bind(
                Exact::new(crate::queue::finding_iri(crate::queue::PROBE_ID)),
                Listing {
                    calls: Arc::clone(&calls),
                    fail_last,
                },
            );
        (
            Arc::new(Kernel::new(Arc::new(space))),
            Capability::scoped(["urn:test:findings".to_string()]),
            calls,
        )
    }

    #[test]
    fn malformed_or_nonadvancing_pages_fail_instead_of_truncating_or_looping() {
        let request = Request::new(Verb::Source, Iri::parse("urn:repo:demo:findings").unwrap());
        let mut pages = Pages::new(request);
        for bad in [
            json!([]),
            json!({"rows": []}),
            json!({"rows": [], "next": FIRST}),
            json!({"rows": [], "next": 2}),
        ] {
            assert!(pages.accept(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let first = serde_json::to_vec(&json!({"rows": [{"id": FIRST}], "next": FIRST})).unwrap();
        assert!(pages.accept(&first).unwrap());
        assert!(
            pages.accept(&first).is_err(),
            "a repeated cursor cannot make progress"
        );
    }
}
