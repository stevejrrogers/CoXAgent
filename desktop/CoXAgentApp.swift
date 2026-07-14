// CoXAgent macOS shell — a native window (WKWebView) that boots the bundled
// `coxagent hub` and loads the dashboard. No external browser, no server to
// start by hand. Unsigned/ad-hoc — build & run locally without a cert.
import Cocoa
import WebKit
import UserNotifications

let PORT = 4000

final class AppDelegate: NSObject, NSApplicationDelegate, WKNavigationDelegate, WKUIDelegate,
                         WKScriptMessageHandler, UNUserNotificationCenterDelegate {
    var window: NSWindow!
    var web: WKWebView!
    var hub: Process?

    func applicationDidFinishLaunching(_ note: Notification) {
        startHub()

        // Bridge web → native for notifications: the WKWebView has no web
        // Notification API, so the page posts to `coxnotify` and we raise a real
        // macOS notification instead. UNUserNotificationCenter registers the app
        // with the system (so it appears in System Settings › Notifications) and
        // is the supported path on modern macOS.
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        center.requestAuthorization(options: [.alert, .sound]) { granted, err in
            self.notifLog("authorization granted=\(granted) err=\(String(describing: err))")
        }

        let cfg = WKWebViewConfiguration()
        let ucc = WKUserContentController()
        ucc.add(self, name: "coxnotify")
        cfg.userContentController = ucc

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
    // The page posts {title, body, channel} to `coxnotify`; we raise a macOS
    // notification via UNUserNotificationCenter.
    func userContentController(_ ucc: WKUserContentController, didReceive message: WKScriptMessage) {
        guard message.name == "coxnotify", let d = message.body as? [String: Any] else { return }
        // Diagnostic pings from the page: log, don't raise a banner.
        if let dbg = d["debug"] as? String { notifLog("JS: \(dbg)"); return }
        let title = d["title"] as? String ?? "CoXAgent"
        let body = d["body"] as? String ?? ""
        let channel = d["channel"] as? String ?? ""
        notifLog("coxnotify received: title=\(title) channel=\(channel)")

        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        if !channel.isEmpty { content.userInfo = ["channel": channel] }
        let req = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(req) { err in
            if let err = err { self.notifLog("add() error: \(err)") }
        }
    }

    // Show the banner even when CoXAgent is the frontmost app.
    func userNotificationCenter(_ center: UNUserNotificationCenter,
                                willPresent notification: UNNotification,
                                withCompletionHandler completionHandler:
                                    @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .sound])
    }

    // Clicking a notification focuses the app and jumps to that channel.
    func userNotificationCenter(_ center: UNUserNotificationCenter,
                                didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let ch = response.notification.request.content.userInfo["channel"] as? String ?? ""
        DispatchQueue.main.async {
            NSApp.activate(ignoringOtherApps: true)
            self.window.makeKeyAndOrderFront(nil)
            if !ch.isEmpty {
                // Channel ids are URL-safe slugs; still escape quotes defensively.
                let safe = ch.replacingOccurrences(of: "\\", with: "\\\\")
                             .replacingOccurrences(of: "'", with: "\\'")
                self.web.evaluateJavaScript(
                    "window.__coxOpenChannel && window.__coxOpenChannel('\(safe)')",
                    completionHandler: nil)
            }
        }
        completionHandler()
    }

    // Append a line to ~/CoXAgent/notif.log for diagnosing the notification path.
    func notifLog(_ msg: String) {
        let path = FileManager.default.homeDirectoryForCurrentUser.path + "/CoXAgent/notif.log"
        let line = "\(Date()) \(msg)\n"
        if let data = line.data(using: .utf8) {
            if let fh = FileHandle(forWritingAtPath: path) {
                fh.seekToEndOfFile(); fh.write(data); fh.closeFile()
            } else {
                try? line.write(toFile: path, atomically: true, encoding: .utf8)
            }
        }
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
