//! The tao window + wry webview hosting the dashboard.

use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

use crate::application::launcher::LaunchOutcome;
use crate::domain::config::DesktopConfig;

/// Shown when the hub never became ready; auto-retries the dashboard so a
/// slow first boot self-heals without restarting the app.
fn waiting_page(url: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="refresh" content="2;url={url}">
<style>
  body{{margin:0;height:100vh;display:flex;align-items:center;justify-content:center;
       background:#0b1220;color:#dbe7ff;font:15px/1.6 -apple-system,'Segoe UI',sans-serif}}
  .card{{text-align:center}}
  .spin{{width:28px;height:28px;margin:0 auto 14px;border:3px solid #1d3050;
        border-top-color:#22d3ee;border-radius:50%;animation:r 1s linear infinite}}
  @keyframes r{{to{{transform:rotate(360deg)}}}}
  small{{color:#7d92b8}}
</style></head><body><div class="card"><div class="spin"></div>
<b>Starting CoXAgent hub…</b><br>
<small>Retrying automatically. Logs: ~/CoXAgent/logs/hub.log</small>
</div></body></html>"#
    )
}

/// Run the window until close; keeps the hub guard (if any) alive for the
/// whole event loop and drops it — killing the hub — on close.
pub fn run(cfg: &DesktopConfig, outcome: LaunchOutcome) -> wry::Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("CoXAgent")
        .with_inner_size(tao::dpi::LogicalSize::new(1360.0, 860.0))
        .build(&event_loop)
        .expect("window");

    let url = cfg.dashboard_url();
    let builder = if outcome.is_ready() {
        WebViewBuilder::new().with_url(&url)
    } else {
        WebViewBuilder::new().with_html(waiting_page(&url))
    };
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

    // Moved into the closure so the spawned hub lives as long as the window.
    let mut hub = Some(outcome);
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            hub.take(); // drops the HubGuard → kills the spawned hub
            *control_flow = ControlFlow::Exit;
        }
    });
}
