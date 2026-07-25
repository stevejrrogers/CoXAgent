//! CoXAgent cross-platform desktop shell — composition root.
//!
//! A tiny native window (tao) with a system webview (wry) that boots the
//! bundled `coxagent hub` and loads the dashboard — the Windows/Linux/macOS
//! counterpart of the Swift shell, sharing the same "start the hub, open the
//! dashboard, kill the hub on quit" behaviour. No external browser, no server
//! to start by hand.
//!
//! Layering mirrors the main workspace (hexagonal):
//! - `domain`         — pure config + launch policy, no IO
//! - `application`    — the launch use case over ports (probe/spawn)
//! - `infrastructure` — real TCP probe, process spawner, workspace prep
//! - `presentation`   — the tao/wry window

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod application;
mod domain;
mod infrastructure;
mod presentation;

use application::launcher::launch;
use domain::config::DesktopConfig;
use infrastructure::{probe::TcpProbe, process::HubSpawner, workspace};

fn main() -> wry::Result<()> {
    let cfg = DesktopConfig::from_env();
    let ws = workspace::prepare(&cfg.workspace).expect("prepare workspace");
    let spawner = HubSpawner::new(infrastructure::process::hub_binary_path(), ws, cfg.port);
    let outcome = launch(&cfg.policy(), &spawner, &TcpProbe, |d| {
        std::thread::sleep(d)
    });
    presentation::window::run(&cfg, outcome)
}
