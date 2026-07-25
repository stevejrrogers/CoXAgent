//! The launch use case: reuse an already-running hub if one answers on the
//! port, otherwise spawn the bundled hub and wait for it to accept
//! connections. The spawned process is owned by a [`HubGuard`] that kills it
//! on drop, so the hub dies with the window even on panic.

use std::time::Duration;

use crate::domain::config::LaunchPolicy;

/// Outbound port: "is anything accepting TCP connections on this port?"
pub trait ProbePort {
    fn is_up(&self, port: u16) -> bool;
}

/// Outbound port: start the hub process.
pub trait SpawnPort {
    fn spawn(&self) -> std::io::Result<Box<dyn HubHandle>>;
}

/// A running hub process the shell owns.
pub trait HubHandle {
    fn kill(&mut self);
}

/// Owns a spawned hub; kills it on drop (window close, panic, early return).
pub struct HubGuard(Box<dyn HubHandle>);

impl Drop for HubGuard {
    fn drop(&mut self) {
        self.0.kill();
    }
}

/// What happened at launch — the window layer renders accordingly.
// Guard fields are never *read*: they exist to be held for the window's
// lifetime and dropped (killing the hub) on close.
#[allow(dead_code)]
pub enum LaunchOutcome {
    /// A hub was already listening; we attach to it and never kill it.
    Reused,
    /// We spawned the hub and it came up. Guard keeps it alive/kills on drop.
    Started(HubGuard),
    /// We spawned but it never answered within the policy window (guard still
    /// kills it on drop), or the spawn itself failed.
    NotReady(Option<HubGuard>),
}

impl LaunchOutcome {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Reused | Self::Started(_))
    }
}

/// Run the launch sequence. `sleep` is injected so tests run instantly.
pub fn launch(
    policy: &LaunchPolicy,
    spawner: &dyn SpawnPort,
    probe: &dyn ProbePort,
    sleep: impl Fn(Duration),
) -> LaunchOutcome {
    if probe.is_up(policy.port) {
        return LaunchOutcome::Reused;
    }
    let Ok(handle) = spawner.spawn() else {
        return LaunchOutcome::NotReady(None);
    };
    let guard = HubGuard(handle);
    for _ in 0..policy.ready_attempts {
        if probe.is_up(policy.port) {
            return LaunchOutcome::Started(guard);
        }
        sleep(policy.ready_interval);
    }
    LaunchOutcome::NotReady(Some(guard))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    struct FakeProbe {
        /// Number of probes that answer "down" before the port looks up.
        down_first: AtomicU32,
    }
    impl FakeProbe {
        fn up_after(n: u32) -> Self {
            Self {
                down_first: AtomicU32::new(n),
            }
        }
    }
    impl ProbePort for FakeProbe {
        fn is_up(&self, _port: u16) -> bool {
            let left = self.down_first.load(Ordering::SeqCst);
            if left == 0 {
                true
            } else {
                self.down_first.store(left - 1, Ordering::SeqCst);
                false
            }
        }
    }

    struct FakeHandle {
        killed: Arc<AtomicBool>,
    }
    impl HubHandle for FakeHandle {
        fn kill(&mut self) {
            self.killed.store(true, Ordering::SeqCst);
        }
    }

    struct FakeSpawner {
        fail: bool,
        killed: Arc<AtomicBool>,
        spawned: Cell<u32>,
    }
    impl FakeSpawner {
        fn ok() -> Self {
            Self {
                fail: false,
                killed: Arc::new(AtomicBool::new(false)),
                spawned: Cell::new(0),
            }
        }
        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::ok()
            }
        }
    }
    impl SpawnPort for FakeSpawner {
        fn spawn(&self) -> std::io::Result<Box<dyn HubHandle>> {
            if self.fail {
                return Err(std::io::Error::other("no binary"));
            }
            self.spawned.set(self.spawned.get() + 1);
            Ok(Box::new(FakeHandle {
                killed: self.killed.clone(),
            }))
        }
    }

    fn policy(attempts: u32) -> LaunchPolicy {
        LaunchPolicy {
            port: 4000,
            ready_attempts: attempts,
            ready_interval: Duration::from_millis(1),
        }
    }

    #[test]
    fn reuses_running_hub_without_spawning() {
        let spawner = FakeSpawner::ok();
        let out = launch(&policy(5), &spawner, &FakeProbe::up_after(0), |_| {});
        assert!(matches!(out, LaunchOutcome::Reused));
        assert_eq!(spawner.spawned.get(), 0);
    }

    #[test]
    fn spawns_and_waits_until_ready() {
        let spawner = FakeSpawner::ok();
        let slept = Rc::new(Cell::new(0u32));
        let s2 = slept.clone();
        // Probe: down at pre-check + 2 waits, then up.
        let out = launch(&policy(10), &spawner, &FakeProbe::up_after(3), move |_| {
            s2.set(s2.get() + 1);
        });
        assert!(out.is_ready());
        assert!(matches!(out, LaunchOutcome::Started(_)));
        assert_eq!(spawner.spawned.get(), 1);
        assert_eq!(slept.get(), 2);
        assert!(!spawner.killed.load(Ordering::SeqCst));
        drop(out);
        assert!(spawner.killed.load(Ordering::SeqCst), "guard kills on drop");
    }

    #[test]
    fn times_out_but_keeps_guard() {
        let spawner = FakeSpawner::ok();
        let out = launch(&policy(3), &spawner, &FakeProbe::up_after(100), |_| {});
        assert!(!out.is_ready());
        let LaunchOutcome::NotReady(Some(_guard)) = &out else {
            panic!("expected NotReady with guard");
        };
        drop(out);
        assert!(
            spawner.killed.load(Ordering::SeqCst),
            "still killed on drop"
        );
    }

    #[test]
    fn spawn_failure_yields_not_ready_without_guard() {
        let out = launch(
            &policy(3),
            &FakeSpawner::failing(),
            &FakeProbe::up_after(100),
            |_| {},
        );
        assert!(matches!(out, LaunchOutcome::NotReady(None)));
    }
}
