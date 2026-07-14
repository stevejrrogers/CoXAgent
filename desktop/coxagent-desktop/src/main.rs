//! CoXAgent cross-platform desktop shell.
//!
//! A tiny native window (tao) with a system webview (wry) that boots the
//! bundled `coxagent hub` and loads the dashboard — the Windows/Linux/macOS
//! counterpart of the Swift shell, sharing the same "start the hub, open the
//! dashboard, kill the hub on quit" behaviour. No external browser, no server
//! to start by hand.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

const PORT: u16 = 4000;

/// The bundled hub binary sits next to this executable. It is named `cox-server`
/// (not `coxagent`) so it never collides case-insensitively with the shell
/// executable `CoXAgent` on Windows/macOS filesystems.
fn hub_path() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let dir = exe.parent().expect("exe dir");
    let name = if cfg!(windows) {
        "cox-server.exe"
    } else {
        "cox-server"
    };
    dir.join(name)
}

/// Per-user workspace: `~/CoXAgent` (registry, project state, logs).
fn workspace() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("CoXAgent")
}

/// Boot the hub, creating the workspace + an empty registry on first run.
fn start_hub() -> std::io::Result<Child> {
    let ws = workspace();
    std::fs::create_dir_all(&ws)?;
    let registry = ws.join("registry.json");
    if !registry.exists() {
        std::fs::write(&registry, "[]")?;
    }
    Command::new(hub_path())
        .args([
            "hub",
            "--registry",
            registry.to_str().unwrap_or("registry.json"),
            "--port",
            &PORT.to_string(),
        ])
        .current_dir(&ws)
        .env("COXAGENT_HOST", "127.0.0.1")
        .spawn()
}

/// Wait (up to ~24s) for the hub to accept connections before loading the UI.
fn wait_ready() {
    for _ in 0..80 {
        if TcpStream::connect(("127.0.0.1", PORT)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn main() -> wry::Result<()> {
    let mut hub = start_hub().ok();
    wait_ready();

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("CoXAgent")
        .with_inner_size(tao::dpi::LogicalSize::new(1360.0, 860.0))
        .build(&event_loop)
        .expect("window");

    let url = format!("http://127.0.0.1:{PORT}/");
    let builder = WebViewBuilder::new().with_url(&url);
    // Windows/macOS attach via the window handle; Linux attaches to the GTK box.
    #[cfg(not(target_os = "linux"))]
    let _webview = builder.build(&window)?;
    #[cfg(target_os = "linux")]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().expect("gtk vbox");
        builder.build_gtk(vbox)?
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            if let Some(mut child) = hub.take() {
                let _ = child.kill();
            }
            *control_flow = ControlFlow::Exit;
        }
    });
}
