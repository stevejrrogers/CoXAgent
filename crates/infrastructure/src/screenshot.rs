//! Headless browser screenshots for visual QA — nobody on an autonomous team
//! ever LOOKS at the UI it ships unless someone takes a picture.
//!
//! Uses whatever Chrome/Chromium is installed (headless mode); returns `false`
//! quietly when none is — visual QA is a bonus pass, never a blocker.

use std::path::Path;
use std::time::Duration;

/// Candidate Chrome/Chromium binaries, most common first.
fn chrome_binary() -> Option<String> {
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "google-chrome",
        "chromium",
        "chromium-browser",
    ];
    for c in candidates {
        if c.starts_with('/') {
            if Path::new(c).exists() {
                return Some(c.to_owned());
            }
        } else if which(c) {
            return Some(c.to_owned());
        }
    }
    None
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
}

/// Capture `url` into `out` (PNG). Best-effort: `false` on any failure
/// (no browser, timeout, crash) — callers skip visual QA then.
pub async fn capture(url: &str, out: &Path) -> bool {
    let Some(chrome) = chrome_binary() else {
        return false;
    };
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Some(out_str) = out.to_str() else {
        return false;
    };
    let run = tokio::process::Command::new(&chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--window-size=1280,900",
            "--hide-scrollbars",
            &format!("--screenshot={out_str}"),
            url,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(Duration::from_secs(30), run).await,
        Ok(Ok(s)) if s.success()
    ) && out.exists()
}

/// Port adapter over the local Chrome/Chromium.
pub struct ChromeScreenshot;

#[async_trait::async_trait]
impl coxagent_application::ports::outbound::ScreenshotPort for ChromeScreenshot {
    async fn capture(&self, url: &str, out: &std::path::Path) -> bool {
        capture(url, out).await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn which_finds_sh_on_unix() {
        #[cfg(unix)]
        assert!(super::which("sh"));
        assert!(!super::which("definitely-not-a-binary-xyz"));
    }
}
