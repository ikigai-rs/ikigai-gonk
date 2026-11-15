//! Named sets of ledgers for page filters (ledger #3191).
//!
//! Each repeatable `gonk.project = "name ledger-a,ledger-b"` line declares membership.
//! Repeated names add members; a ledger can belong to several projects or none. These
//! declarations carry no authority: the ledger's union reads under the viewer's capability.
use ikigai_core::{Error, Result};
use ikigai_ledger::Ledger;
use std::collections::BTreeMap;

/// Singular, type-first configuration key.
pub const KEY: &str = "gonk.project";

/// Deployment declarations, resolved afresh for each page and facet request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Projects {
    members: BTreeMap<String, Vec<String>>,
    source: Option<std::path::PathBuf>,
}

impl Projects {
    /// Parse repeatable `<project> <comma-separated ledgers>` declarations. A bare name
    /// declares an empty project. Names follow the ledger's path-safe naming rule.
    pub fn parse(lines: &[String]) -> std::result::Result<Self, String> {
        let mut projects = Self::default();
        for line in lines {
            let line = line.trim();
            let (name, members) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let refuse = |why| format!("{KEY} = {line:?}: {why}");
            Ledger::parse(name).map_err(|e| refuse(e.to_string()))?;
            let entry = projects.members.entry(name.to_string()).or_default();
            if !members.trim().is_empty() {
                for member in members.split(',').map(str::trim) {
                    Ledger::parse(member).map_err(|e| refuse(e.to_string()))?;
                    if !entry.iter().any(|m| m == member) {
                        entry.push(member.to_string());
                    }
                }
            }
        }
        Ok(projects)
    }

    /// Read this config file on each request, so saved views keep names rather than a
    /// frozen membership. A missing config file declares no projects.
    pub fn with_config(mut self, path: std::path::PathBuf) -> Self {
        self.source = Some(path);
        self
    }

    /// One consistent declaration for a request. Invalid edits refuse project listings;
    /// they never silently reuse old membership. No filesystem path is sent to a viewer.
    pub fn snapshot(&self) -> Result<Self> {
        let Some(path) = &self.source else {
            return Ok(self.clone());
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(_) => {
                return Err(Error::Endpoint(
                    "Project declarations could not be read.".into(),
                ))
            }
        };
        Self::parse(&crate::config::values_for(&text, KEY)).map_err(|detail| {
            Error::InvalidArgument {
                name: KEY.into(),
                detail,
            }
        })
    }

    /// Declared projects in name order. Callers must intersect members with readable ledgers
    /// before offering a project or its members in navigation.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.members
            .iter()
            .map(|(name, members)| (name.as_str(), members.as_slice()))
    }

    /// Expand project names and add explicit ledgers, preserving first occurrence. `None`
    /// keeps an ordinary single-ledger listing. Unknown projects fail rather than silently
    /// changing the selection. Empty projects produce an empty union, never the base ledger.
    pub fn resolve(&self, projects: Option<&str>, ledgers: Option<&str>) -> Result<Option<String>> {
        let Some(projects) = projects.filter(|p| !p.trim().is_empty()) else {
            return Ok(ledgers.map(str::to_string));
        };
        if self.source.is_some() {
            return self.snapshot()?.resolve(Some(projects), ledgers);
        }
        let mut selected: Vec<String> = Vec::new();
        for name in projects.split(',').map(str::trim) {
            let members = self
                .members
                .get(name)
                .ok_or_else(|| Error::InvalidArgument {
                    name: "projects".into(),
                    detail: format!("Unknown project `{name}`."),
                })?;
            for member in members {
                if !selected.contains(member) {
                    selected.push(member.clone());
                }
            }
        }
        if let Some(ledgers) = ledgers.filter(|v| !v.trim().is_empty()) {
            for member in ledgers.split(',').map(str::trim) {
                Ledger::parse(member)?;
                if !selected.iter().any(|m| m == member) {
                    selected.push(member.to_string());
                }
            }
        }
        Ok(Some(selected.join(",")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_projects_and_explicit_ledgers_form_one_set() {
        let p = Projects::parse(&[
            "work default,crew,default".into(),
            "work review".into(),
            "other crew,extra".into(),
            "empty".into(),
        ])
        .unwrap();
        assert_eq!(
            p.resolve(Some("work,other,work"), Some("extra,last"))
                .unwrap()
                .as_deref(),
            Some("default,crew,review,extra,last")
        );
        assert_eq!(p.resolve(Some("empty"), None).unwrap().as_deref(), Some(""));
        assert_eq!(p.resolve(None, None).unwrap(), None);
        assert!(p.resolve(Some("missing"), None).is_err());
        for line in [
            "",
            "Bad default",
            "work default,,crew",
            "work ../private",
            "work crew,not a name",
        ] {
            assert!(Projects::parse(&[line.into()]).is_err(), "{line}");
        }
    }
}
