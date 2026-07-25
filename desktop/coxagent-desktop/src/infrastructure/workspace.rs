//! First-run workspace preparation: `~/CoXAgent` with an empty registry.

use std::path::{Path, PathBuf};

/// Create the workspace directory and an empty `registry.json` if missing.
/// Idempotent; never overwrites an existing registry.
pub fn prepare(workspace: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(workspace)?;
    let registry = workspace.join("registry.json");
    if !registry.exists() {
        std::fs::write(&registry, "[]")?;
    }
    Ok(workspace.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_dir_and_empty_registry() {
        let tmp = tempfile::tempdir().expect("tmp");
        let ws = tmp.path().join("CoXAgent");
        prepare(&ws).expect("prepare");
        assert_eq!(
            std::fs::read_to_string(ws.join("registry.json")).unwrap(),
            "[]"
        );
    }

    #[test]
    fn never_overwrites_existing_registry() {
        let tmp = tempfile::tempdir().expect("tmp");
        let ws = tmp.path().to_path_buf();
        std::fs::write(ws.join("registry.json"), "[{\"id\":\"demo\"}]").unwrap();
        prepare(&ws).expect("prepare");
        assert_eq!(
            std::fs::read_to_string(ws.join("registry.json")).unwrap(),
            "[{\"id\":\"demo\"}]"
        );
    }
}
