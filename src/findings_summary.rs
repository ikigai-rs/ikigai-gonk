//! Pending findings per file, counted by severity (ledger #2130).
//!
//! This is a composition over browse's findings resource, not a second interpretation of
//! its RDF: pending includes computed supersession, and a retracted decision can make a
//! finding pending again. The nested read keeps the caller's capability and browse's
//! freshness. It may read file contents and run browse's drift pass; it calls no model.

use std::collections::BTreeMap;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Request, Result, UriTemplate, Verb,
};
use serde::{Deserialize, Serialize};

const JSON: &str = "application/json";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// Extend gonk's browse space over exactly the roots browse serves. Rebuilding that space
/// when roots change rebuilds these bindings too.
pub(crate) fn bind(mut space: EndpointSpace, roots: &[String]) -> EndpointSpace {
    for root in roots {
        let name = format!("urn:repo:{root}:findings-summary");
        space = space
            .bind(Exact::new(&name), Summary { root: root.clone() })
            .bind(
                UriTemplate::parse(format!("{name}:{{path}}"))
                    .expect("a validated browse root name"),
                Summary { root: root.clone() },
            );
    }
    space
}

struct Summary {
    root: String,
}

#[derive(Deserialize)]
struct Finding {
    path: String,
    pr: Option<u64>,
    effective_severity: Option<String>,
    severity: Option<String>,
}

#[derive(Default, Serialize)]
struct FileCounts {
    path: String,
    total: usize,
    severities: BTreeMap<String, usize>,
    unrated: usize,
}

#[async_trait]
impl Endpoint for Summary {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let grant = format!("{}{}", ikigai_browse::CAP_PREFIX, self.root);
        if !inv.capability.allows(&grant) && !inv.capability.allows(ikigai_browse::CAP_WILDCARD) {
            return Err(Error::Denied(format!(
                "findings-summary: capability does not grant `{grant}`"
            )));
        }
        let encoded = inv.bindings.get("path");
        let path = encoded
            .map(|encoded| {
                crate::browse::decode_rel(encoded)
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(|path| path.display().to_string())
                    .ok_or_else(|| Error::InvalidArgument {
                        name: "path".to_string(),
                        detail: "expected a percent-encoded root-relative file path".to_string(),
                    })
            })
            .transpose()?;
        let listing = match encoded {
            Some(path) => format!("urn:repo:{}:findings:{path}", self.root),
            None => format!("urn:repo:{}:findings", self.root),
        };
        // No page or limit: count the whole set. Browse refuses an oversized unpaged
        // answer rather than truncating it; propagate that refusal instead of reporting
        // a partial count. The nested read also owns what "pending" means.
        let request = Request::new(
            Verb::Source,
            Iri::parse(&listing).map_err(|e| Error::Endpoint(e.to_string()))?,
        )
        .with_arg("state", ArgRef::Inline(b"pending".to_vec()))
        .with_arg("as", ArgRef::Inline(JSON.as_bytes().to_vec()));
        let answer = inv.issue(request).await?;
        let findings: Vec<Finding> = serde_json::from_slice(&answer.bytes)
            .map_err(|e| Error::Endpoint(format!("findings-summary: invalid findings: {e}")))?;
        let mut files = BTreeMap::<String, FileCounts>::new();
        // A named file with no findings still answers explicitly with zero counts.
        if let Some(path) = path {
            files.insert(
                path.clone(),
                FileCounts {
                    path,
                    ..FileCounts::default()
                },
            );
        }
        for finding in findings {
            // PR findings address a diff, not a file's current content.
            if finding.pr.is_some() {
                continue;
            }
            let file = files
                .entry(finding.path.clone())
                .or_insert_with(|| FileCounts {
                    path: finding.path,
                    ..FileCounts::default()
                });
            file.total += 1;
            match finding.effective_severity.or(finding.severity) {
                Some(severity) => *file.severities.entry(severity).or_default() += 1,
                None => file.unrated += 1,
            }
        }
        let total: usize = files.values().map(|file| file.total).sum();
        let body = serde_json::json!({
            "repo": self.root,
            "state": "pending",
            "total": total,
            "files": files.into_values().collect::<Vec<_>>(),
        });
        // Default expiry is Always: neither decisions nor computed supersession may be
        // cached under a filesystem-only thread.
        Ok(Representation::new(
            ReprType::new(JSON),
            body.to_string().into_bytes(),
        ))
    }

    fn name(&self) -> &str {
        "gonk-findings-summary"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("Pending findings per file, counted by severity")
            .summary(
                "urn:repo:{root}:findings-summary[:{path}]. Counts every pending file \
                 finding in browse's current reading, including all severities and unrated \
                 findings. Omitting path summarizes every file with pending findings in \
                 the root; naming an empty file returns zero. PR findings are excluded. \
                 JSON: {repo, state, total, files: [{path, total, severities: {word: count}, \
                 unrated}]}, sorted by path and severity word; zero severity counts are \
                 omitted. Uses effective_severity (the model's proposal while pending). \
                 Reads browse's findings with the caller's capability, including its \
                 supersession and drift passes; no model call. Browse's oversized-answer \
                 refusal is propagated, never converted into a partial count.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .input(
                ArgSpec::new("path")
                    .binding()
                    .optional()
                    .class(XSD_STRING)
                    .summary("root-relative file path, percent-encoded; omitted = every file"),
            )
            .input(
                ArgSpec::new("as")
                    .optional()
                    .class(XSD_STRING)
                    .one_of([JSON])
                    .default_value(JSON),
            )
            .output(JSON)
    }
}
