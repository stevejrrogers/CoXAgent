//! Spawns the bundled hub binary with output captured to the workspace log.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::application::launcher::{HubHandle, SpawnPort};
use crate::domain::config::hub_binary_name;

/// The bundled hub sits next to this executable (see `hub_binary_name` for
/// why it is not called `coxagent`).
pub fn hub_binary_path() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let dir = exe.parent().expect("exe dir");
    dir.join(hub_binary_name())
}

pub struct HubSpawner {
    hub_bin: PathBuf,
    workspace: PathBuf,
    port: u16,
}

impl HubSpawner {
    pub fn new(hub_bin: PathBuf, workspace: PathBuf, port: u16) -> Self {
        Self {
            hub_bin,
            workspace,
            port,
        }
    }

    /// Desktop launchers (Finder/Explorer) give the process no console; send
    /// hub output to `<workspace>/logs/hub.log` so failures are diagnosable.
    fn log_file(&self) -> Option<std::fs::File> {
        let dir = self.workspace.join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::File::create(dir.join("hub.log")).ok()
    }
}

impl SpawnPort for HubSpawner {
    fn spawn(&self) -> std::io::Result<Box<dyn HubHandle>> {
        let registry = self.workspace.join("registry.json");
        let mut cmd = Command::new(&self.hub_bin);
        cmd.args([
            "hub",
            "--registry",
            registry.to_str().unwrap_or("registry.json"),
            "--port",
            &self.port.to_string(),
        ])
        .current_dir(&self.workspace)
        .env("COXAGENT_HOST", "127.0.0.1")
        .stdin(Stdio::null());
        match (self.log_file(), self.log_file()) {
            (Some(out), Some(err)) => {
                cmd.stdout(out).stderr(err);
            }
            _ => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        let child = cmd.spawn()?;
        Ok(Box::new(ChildHandle(child)))
    }
}

struct ChildHandle(Child);

impl HubHandle for ChildHandle {
    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait(); // reap; no zombie between kill and exit
    }
}

/// Kept for the launcher integration test: spawn an arbitrary command as the
/// "hub" so the real spawner code path is exercised without the real binary.
#[cfg(test)]
pub fn spawner_for(
    hub_bin: &std::path::Path,
    workspace: &std::path::Path,
    port: u16,
) -> HubSpawner {
    HubSpawner::new(hub_bin.to_path_buf(), workspace.to_path_buf(), port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn hub_binary_sits_next_to_exe() {
        let p = hub_binary_path();
        assert_eq!(
            p.file_name().and_then(|n| n.to_str()),
            Some(hub_binary_name())
        );
        assert_eq!(
            p.parent(),
            std::env::current_exe().unwrap().parent(),
            "must resolve beside the shell executable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn spawns_captures_log_and_kills() {
        use crate::application::launcher::SpawnPort;
        let ws = tempfile::tempdir().expect("tmp");
        // `sleep` ignores the hub args and just stays alive like a hub would.
        let spawner = spawner_for(Path::new("/bin/sleep"), ws.path(), 4000);
        // /bin/sleep rejects our args? No: "hub --registry … 4000" — sleep
        // exits with usage error, which is fine: we only assert spawn+kill.
        let mut handle = spawner.spawn().expect("spawn");
        assert!(ws.path().join("logs").join("hub.log").exists());
        handle.kill(); // must not panic even if already exited
        handle.kill(); // idempotent
    }

    #[test]
    fn spawn_missing_binary_errors() {
        let ws = tempfile::tempdir().expect("tmp");
        let spawner = spawner_for(Path::new("/definitely/not/here"), ws.path(), 4000);
        assert!(spawner.spawn().is_err());
    }
}
