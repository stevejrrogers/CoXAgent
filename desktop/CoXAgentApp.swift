// CoXAgent macOS shell — a native window (WKWebView) that boots the bundled
// `coxagent hub` and loads the dashboard. No external browser, no server to
// start by hand. Unsigned/ad-hoc — build & run locally without a cert.
import Cocoa
import WebKit

let PORT = 4000

final class AppDelegate: NSObject, NSApplicationDelegate, WKNavigationDelegate, WKUIDelegate,
                         WKScriptMessageHandler, NSUserNotificationCenterDelegate {
    var window: NSWindow!
    var web: WKWebView!
    var hub: Process?

    func applicationDidFinishLaunching(_ note: Notification) {
        startHub()

        // Bridge web → native for notifications: the WKWebView has no web
        // Notification API, so the page posts to `coxnotify` and we raise a real
        // macOS notification instead.
        let cfg = WKWebViewConfiguration()
        let ucc = WKUserContentController()
        ucc.add(self, name: "coxnotify")
        cfg.userContentController = ucc
        NSUserNotificationCenter.default.delegate = self

        web = WKWebView(frame: NSMakeRect(0, 0, 1360, 860), configuration: cfg)
        web.navigationDelegate = self
        web.uiDelegate = self // present native panels for JS alert/confirm/prompt

        window = NSWindow(
            contentRect: NSMakeRect(0, 0, 1360, 860),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = "CoXAgent"
        window.center()
        window.contentView = web
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)

        loadWhenReady()
    }

    // The bundled server binary sits next to this executable. It is named
    // `cox-server` (not `coxagent`) to avoid colliding case-insensitively with
    // the shell executable `CoXAgent` on macOS's default filesystem.
    func coxagentURL() -> URL {
        Bundle.main.executableURL!.deletingLastPathComponent().appendingPathComponent("cox-server")
    }

    func workspace() -> String {
        FileManager.default.homeDirectoryForCurrentUser.path + "/CoXAgent"
    }

    // Give subprocesses a login-like PATH so `claude`/`docker` are found even
    // when launched from Finder (which has a minimal PATH).
    func richEnv() -> [String: String] {
        var env = ProcessInfo.processInfo.environment
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let extra = ["\(home)/.local/bin", "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
        env["PATH"] = extra.joined(separator: ":") + ":" + (env["PATH"] ?? "")
        return env
    }

    // First-run prompt: set an admin password to require login, or skip for a
    // no-login local app. Returns nil when left blank.
    func promptForPassword() -> String? {
        let alert = NSAlert()
        alert.messageText = "Secure CoXAgent with a login?"
        alert.informativeText =
            "Set an admin password to require sign-in (recommended if others use this Mac). "
            + "Leave blank to open without a login."
        alert.addButton(withTitle: "Continue")
        let field = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.placeholderString = "admin password (optional)"
        alert.accessoryView = field
        alert.window.initialFirstResponder = field
        alert.runModal()
        let pw = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        return pw.isEmpty ? nil : pw
    }

    func startHub() {
        let fm = FileManager.default
        let ws = workspace()
        let reg = ws + "/registry.json"
        let stateDir = ws + "/default/state"
        try? fm.createDirectory(atPath: stateDir, withIntermediateDirectories: true)
        try? fm.createDirectory(atPath: ws + "/default/codebase", withIntermediateDirectories: true)

        let wsURL = URL(fileURLWithPath: ws)
        let authPath = ws + "/auth.json"

        // First run: optionally set a login, then onboard a default project.
        var hubEnv = richEnv()
        if !fm.fileExists(atPath: reg) {
            // Ask once whether to protect the app with a login.
            if !fm.fileExists(atPath: authPath), let pw = promptForPassword() {
                hubEnv["COXAGENT_ADMIN_USER"] = "root"
                hubEnv["COXAGENT_ADMIN_PASSWORD"] = pw
            }
            let ob = Process()
            ob.executableURL = coxagentURL()
            ob.environment = richEnv()
            ob.currentDirectoryURL = wsURL
            ob.arguments = ["--state-dir", stateDir, "onboard", "--name", "My Project", "--alias", "MYP"]
            try? ob.run(); ob.waitUntilExit()
            let json = "[{\"id\":\"default\",\"path\":\"\(ws)/default\"}]"
            try? json.write(toFile: reg, atomically: true, encoding: .utf8)
        }

        let p = Process()
        p.executableURL = coxagentURL()
        p.environment = hubEnv // carries the admin login on first run, if chosen
        p.currentDirectoryURL = wsURL // Finder launches with cwd=/ (read-only)
        p.arguments = ["hub", "--registry", reg, "--port", "\(PORT)"]
        // Redirect to a log file: a GUI app has no console, and letting the hub
        // write to the inherited (closed) stdio can kill it with SIGPIPE.
        let logPath = ws + "/hub.log"
        FileManager.default.createFile(atPath: logPath, contents: nil)
        if let fh = FileHandle(forWritingAtPath: logPath) {
            p.standardOutput = fh
            p.standardError = fh
        }
        try? p.run()
        hub = p
    }

    // Poll /api/health, then load the dashboard once the hub is listening.
    func loadWhenReady(_ attempt: Int = 0) {
        let health = URL(string: "http://127.0.0.1:\(PORT)/api/health")!
        URLSession.shared.dataTask(with: health) { _, resp, _ in
            DispatchQueue.main.async {
                if let http = resp as? HTTPURLResponse, http.statusCode == 200 {
                    self.web.load(URLRequest(url: URL(string: "http://127.0.0.1:\(PORT)/")!))
                } else if attempt < 80 {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) {
                        self.loadWhenReady(attempt + 1)
                    }
                }
            }
        }.resume()
    }

    func applicationWillTerminate(_ note: Notification) { hub?.terminate() }
    func applicationShouldTerminateAfterLastWindowClosed(_ s: NSApplication) -> Bool { true }

    // MARK: - WKUIDelegate: native panels for JS alert()/confirm()/prompt().
    // Without these, WKWebView silently returns default values — so in-app
    // confirms (e.g. "Remove project?") would resolve to false and never fire.
    func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping () -> Void) {
        let a = NSAlert(); a.messageText = "CoXAgent"; a.informativeText = message
        a.addButton(withTitle: "OK")
        a.beginSheetModal(for: window) { _ in completionHandler() }
    }

    func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void) {
        let a = NSAlert(); a.messageText = "CoXAgent"; a.informativeText = message
        a.addButton(withTitle: "OK"); a.addButton(withTitle: "Cancel")
        a.beginSheetModal(for: window) { resp in completionHandler(resp == .alertFirstButtonReturn) }
    }

    func webView(_ webView: WKWebView, runJavaScriptTextInputPanelWithPrompt prompt: String,
                 defaultText: String?, initiatedByFrame frame: WKFrameInfo,
                 completionHandler: @escaping (String?) -> Void) {
        let a = NSAlert(); a.messageText = "CoXAgent"; a.informativeText = prompt
        a.addButton(withTitle: "OK"); a.addButton(withTitle: "Cancel")
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 280, height: 24))
        field.stringValue = defaultText ?? ""
        a.accessoryView = field; a.window.initialFirstResponder = field
        a.beginSheetModal(for: window) { resp in
            completionHandler(resp == .alertFirstButtonReturn ? field.stringValue : nil)
        }
    }

    // MARK: - Native notifications (bridged from the web page).
    // The page posts {title, body, channel} to `coxnotify`; we deliver a macOS
    // notification. NSUserNotification is deprecated but, unlike
    // UNUserNotificationCenter, it works reliably for an ad-hoc-signed app run
    // from outside /Applications — the exact case here.
    func userContentController(_ ucc: WKUserContentController, didReceive message: WKScriptMessage) {
        guard message.name == "coxnotify", let d = message.body as? [String: Any] else { return }
        let n = NSUserNotification()
        n.title = d["title"] as? String ?? "CoXAgent"
        n.informativeText = d["body"] as? String ?? ""
        n.soundName = NSUserNotificationDefaultSoundName
        if let ch = d["channel"] as? String, !ch.isEmpty { n.userInfo = ["channel": ch] }
        NSUserNotificationCenter.default.deliver(n)
    }

    // Show the banner even when CoXAgent is the frontmost app.
    func userNotificationCenter(_ center: NSUserNotificationCenter,
                                shouldPresent notification: NSUserNotification) -> Bool { true }

    // Clicking a notification focuses the app and jumps to that channel.
    func userNotificationCenter(_ center: NSUserNotificationCenter,
                                didActivate notification: NSUserNotification) {
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
        guard let ch = notification.userInfo?["channel"] as? String, !ch.isEmpty else { return }
        // Channel ids are URL-safe slugs; still escape quotes defensively.
        let safe = ch.replacingOccurrences(of: "\\", with: "\\\\")
                     .replacingOccurrences(of: "'", with: "\\'")
        web.evaluateJavaScript("window.__coxOpenChannel && window.__coxOpenChannel('\(safe)')",
                               completionHandler: nil)
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
