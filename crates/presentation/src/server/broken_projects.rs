// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Projects the hub could not load, kept visible instead of dropped.
//!
//! A `coxagent.json` that does not parse now fails the project's load rather
//! than answering with `Config::default()`, whose empty `policy` would turn the
//! governance gates off unasked (COX-B043). Failing closed is only half the
//! answer: a project that fails to build never entered the hub's project map,
//! so `/api/projects` never mentioned it and the dashboard showed nothing at
//! all — the only trace was one line in the hub log, which is exactly where a
//! person who cannot find their project will not look.
//!
//! The record here travels beside the healthy handles so the listing can say
//! "this project is registered, and here is the field that stops it running".

use super::*;

/// A registered project the hub could not load, and why.
///
/// Deliberately NOT a [`ProjectHandle`] variant: a handle owns a store, a
/// runner and an engine, and every route behind it assumes those exist. A
/// broken project has none of them — it is a label and a reason, and the type
/// says so.
#[derive(Clone, Debug)]
pub struct BrokenProject {
    /// Registry id — the same id the healthy handle would have carried.
    pub id: String,
    /// The file a person has to fix, so the message is actionable without
    /// knowing the hub's directory layout.
    pub config_path: PathBuf,
    /// Why the load failed, already naming the offending field where the config
    /// parser could (e.g. `deploy.host_port: invalid value ...`).
    pub error: String,
}

/// The `/api/projects` entries for projects that failed to load.
///
/// Flagged `broken` so a client can list them but refuse to SELECT one: there
/// is no store, no runner and no route behind a broken project, so switching to
/// it would only produce 404s. `name` falls back to the id because the display
/// name lives in a state file this project never got to read.
pub(super) fn broken_entries(broken: &[BrokenProject]) -> Vec<serde_json::Value> {
    broken
        .iter()
        .map(|b| {
            serde_json::json!({
                "id": b.id,
                "name": b.id,
                "alias": "",
                "broken": true,
                "error": b.error,
                "config_path": b.config_path.display().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{broken_entries, BrokenProject};

    fn broken(id: &str, error: &str) -> BrokenProject {
        BrokenProject {
            id: id.to_owned(),
            config_path: std::path::PathBuf::from("/w")
                .join(id)
                .join("coxagent.json"),
            error: error.to_owned(),
        }
    }

    /// The point of the ticket's UI half: the reason a project is missing has
    /// to reach the person looking for it, not just the log.
    #[test]
    fn a_broken_project_carries_its_reason_and_the_file_to_fix() {
        let entries = broken_entries(&[broken(
            "cox",
            "invalid /w/cox/coxagent.json: deploy.host_port: invalid value",
        )]);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], "cox");
        assert_eq!(entries[0]["broken"], true);
        assert_eq!(
            entries[0]["error"],
            "invalid /w/cox/coxagent.json: deploy.host_port: invalid value"
        );
        assert_eq!(entries[0]["config_path"], "/w/cox/coxagent.json");
    }

    /// A broken entry must be labelled even though its display name lives in a
    /// state file the hub never got to read — an unnamed row is a row nobody
    /// can act on.
    #[test]
    fn a_broken_project_is_labelled_by_its_id_when_it_has_no_name_yet() {
        let entries = broken_entries(&[broken("cox", "unreadable")]);

        assert_eq!(entries[0]["name"], "cox");
    }

    /// The healthy hub: nothing extra appended, so the listing is unchanged for
    /// every project that loads.
    #[test]
    fn a_hub_with_nothing_broken_appends_nothing() {
        assert!(broken_entries(&[]).is_empty());
    }
}
