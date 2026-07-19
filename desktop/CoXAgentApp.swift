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
    // In remote mode, the local operator processes this machine contributes to
    // the shared team (one per locally-provisioned project). They idle until the
    // user Starts them from the web, and are torn down when the app quits.
    var operators: [Process] = []
    // The dashboard origin the WebView loads. Defaults to the embedded hub on
    // localhost, but points at a central hub when remote mode is configured.
    var base = "http://127.0.0.1:\(PORT)"

    // Remote-hub mode: when a hub URL is configured (env `COXAGENT_HUB_URL` or a
    // `~/CoXAgent/hub.url` file), this machine does NOT spawn its own hub — it is
    // a thin viewer onto a central, hosted hub that everyone shares. Returns the
    // trimmed URL (no trailing slash) or nil for the default embedded mode.
    func remoteHub() -> String? {
        var raw: String?
        if let u = ProcessInfo.processInfo.environment["COXAGENT_HUB_URL"], !u.isEmpty {
            raw = u
        } else if let s = try? String(contentsOfFile: workspace() + "/hub.url", encoding: .utf8),
            !s.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            raw = s
        } else if let b = Bundle.main.object(forInfoDictionaryKey: "CoxDefaultHubURL") as? String,
            !b.isEmpty {
            // Baked at build time (COXAGENT_DEFAULT_HUB) — a company build that
            // points at its hosted hub out of the box.
            raw = b
        }
        guard var u = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !u.isEmpty else {
            return nil
        }
        while u.hasSuffix("/") { u.removeLast() }
        return u
    }

    func applicationDidFinishLaunching(_ note: Notification) {
        // Only boot a local hub in embedded mode; in remote mode we just view the
        // central hub and contribute this machine's operators to the shared team.
        if let remote = remoteHub() {
            base = remote
            startOperators()
        } else {
            startHub()
        }

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
        ucc.add(self, name: "coxupdate")
        cfg.userContentController = ucc

        web = WKWebView(frame: NSMakeRect(0, 0, 1360, 860), configuration: cfg)
        web.navigationDelegate = self
        web.uiDelegate = self // present native panels for JS alert/confirm/prompt

        window = NSWindow(
            contentRect: NSMakeRect(0, 0, 1360, 860),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = (remoteHub() != nil) ? "CoXAgent — \(base)" : "CoXAgent"
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

    /// Quick synchronous check: is a local MinIO answering on :9000?
    func minioReachable() -> Bool {
        var ok = false
        let sem = DispatchSemaphore(value: 0)
        var req = URLRequest(url: URL(string: "http://127.0.0.1:9000/minio/health/live")!)
        req.timeoutInterval = 0.6
        URLSession.shared.dataTask(with: req) { _, resp, _ in
            if let http = resp as? HTTPURLResponse, http.statusCode == 200 { ok = true }
            sem.signal()
        }.resume()
        _ = sem.wait(timeout: .now() + 1.0)
        return ok
    }

    /// Quick TCP connect check to 127.0.0.1:<port> (used to detect coturn).
    func tcpReachable(port: UInt16) -> Bool {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        if fd < 0 { return false }
        defer { close(fd) }
        var tv = timeval(tv_sec: 0, tv_usec: 400_000)
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr)
        let ok = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) == 0
            }
        }
        return ok
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

        // If a local MinIO is running (scripts/minio.sh up), store uploaded files
        // in it; otherwise the hub falls back to local disk. Reachability-gated
        // so the app never breaks when MinIO is down.
        if hubEnv["COXAGENT_S3_ENDPOINT"] == nil && minioReachable() {
            hubEnv["COXAGENT_S3_ENDPOINT"] = "http://127.0.0.1:9000"
            hubEnv["COXAGENT_S3_BUCKET"] = "coxagent"
            hubEnv["COXAGENT_S3_ACCESS_KEY"] = "coxagent"
            hubEnv["COXAGENT_S3_SECRET_KEY"] = "coxagent123"
        }
        // If a local MongoDB is up (scripts/mongo.sh up), use it as the server-side
        // documentation store so docs persist independently of project state.
        if hubEnv["COXAGENT_MONGO_URL"] == nil && tcpReachable(port: 27017) {
            hubEnv["COXAGENT_MONGO_URL"] = "mongodb://127.0.0.1:27017"
            hubEnv["COXAGENT_MONGO_DB"] = "coxagent"
        }
        // If a local coturn is up (scripts/turn.sh up), advertise it for TURN so
        // calls traverse NATs; secret matches the script's default.
        if hubEnv["COXAGENT_TURN_URL"] == nil && tcpReachable(port: 3478) {
            hubEnv["COXAGENT_TURN_URL"] = "turn:127.0.0.1:3478"
            hubEnv["COXAGENT_TURN_SECRET"] = "coxturn_dev_secret_change_me"
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

    // Remote mode: spawn one local operator per locally-provisioned project so
    // this machine contributes compute to the shared team using THIS user's own
    // credentials. Each operator idles (COXAGENT_WAIT_FOR_START) until the user
    // Starts it from the web, so opening the app never spends tokens unbidden.
    // Coordination (Postgres/Redis) DSNs come from ~/CoXAgent/coordination.json,
    // which the runner loads itself.
    func startOperators() {
        let ws = workspace()
        guard let data = FileManager.default.contents(atPath: ws + "/registry.json"),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        else { return }
        let operatorName = ProcessInfo.processInfo.environment["COXAGENT_OPERATOR"] ?? NSUserName()
        for proj in arr {
            guard let path = proj["path"] as? String,
                  FileManager.default.fileExists(atPath: path + "/codebase")
            else { continue }
            var env = richEnv()
            env["COXAGENT_OPERATOR"] = operatorName
            env["COXAGENT_WAIT_FOR_START"] = "1"
            let p = Process()
            p.executableURL = coxagentURL()
            p.environment = env
            p.currentDirectoryURL = URL(fileURLWithPath: ws)
            p.arguments = ["--state-dir", path + "/state", "run", "--work-dir", path + "/codebase"]
            let logPath = ws + "/operator-\(proj["id"] as? String ?? "proj").log"
            FileManager.default.createFile(atPath: logPath, contents: nil)
            if let fh = FileHandle(forWritingAtPath: logPath) {
                p.standardOutput = fh
                p.standardError = fh
            }
            try? p.run()
            operators.append(p)
        }
    }

    // Poll /api/health, then load the dashboard once the hub (local or remote)
    // is reachable.
    func loadWhenReady(_ attempt: Int = 0) {
        let health = URL(string: "\(base)/api/health")!
        URLSession.shared.dataTask(with: health) { _, resp, _ in
            DispatchQueue.main.async {
                if let http = resp as? HTTPURLResponse, http.statusCode == 200 {
                    var req = URLRequest(url: URL(string: "\(self.base)/")!)
                    // The SPA is tiny and versioned with the hub — never let a
                    // stale cached copy outlive an upgrade.
                    req.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
                    self.web.load(req)
                } else if attempt < 80 {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) {
                        self.loadWhenReady(attempt + 1)
                    }
                }
            }
        }.resume()
    }

    func applicationWillTerminate(_ note: Notification) {
        hub?.terminate()
        for op in operators { op.terminate() }
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ s: NSApplication) -> Bool { true }

    // MARK: - WKUIDelegate: native panels for JS alert()/confirm()/prompt().
    // Without these, WKWebView silently returns default values — so in-app
    // confirms (e.g. "Remove project?") would resolve to false and never fire.
    // External links (downloads, docs, GitHub releases) open in the system
    // browser — the embedded webview neither downloads files nor should it
    // navigate away from the app. Anything not on the hub origin goes out.
    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        if let url = navigationAction.request.url,
           let scheme = url.scheme, scheme == "http" || scheme == "https",
           let host = url.host, host != "127.0.0.1", host != "localhost" {
            NSWorkspace.shared.open(url)
            decisionHandler(.cancel)
            return
        }
        decisionHandler(.allow)
    }

    // target=_blank / window.open: same rule — external to the browser.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction,
                 windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = navigationAction.request.url {
            NSWorkspace.shared.open(url)
        }
        return nil
    }

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

    // MARK: - Media capture (camera/mic) for WebRTC calls.
    // Grant the web content access; macOS still gates the app itself via TCC
    // using the Info.plist usage strings on first use.
    @available(macOS 12.0, *)
    func webView(_ webView: WKWebView,
                 requestMediaCapturePermissionFor origin: WKSecurityOrigin,
                 initiatedByFrame frame: WKFrameInfo,
                 type: WKMediaCaptureType,
                 decisionHandler: @escaping (WKPermissionDecision) -> Void) {
        decisionHandler(.grant)
    }

    // MARK: - Native notifications (bridged from the web page).
    // The page posts {title, body, channel} to `coxnotify`; we raise a macOS
    // notification via UNUserNotificationCenter.
    func userContentController(_ ucc: WKUserContentController, didReceive message: WKScriptMessage) {
        // In-place self-update: the page sends the release .dmg URL; we
        // download, swap the bundle on disk, and relaunch — no manual steps.
        if message.name == "coxupdate", let url = message.body as? String {
            selfUpdate(urlString: url)
            return
        }
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

    // ── In-place self-update ─────────────────────────────────────────────────
    // Download the release .dmg, stage the new bundle, then hand off to a tiny
    // detached script that swaps the .app after this process exits and
    // relaunches it. HTTPS-only; any failure falls back to the browser.
    func updateSay(_ text: String) {
        DispatchQueue.main.async {
            let js = "typeof toasty==='function'&&toasty(" +
                String(data: try! JSONSerialization.data(withJSONObject: [text]), encoding: .utf8)!.dropFirst().dropLast() + ",'ok')"
            self.web.evaluateJavaScript(String(js), completionHandler: nil)
        }
    }

    func selfUpdate(urlString: String) {
        // HTTPS, or plain http ONLY to the local hub (the download proxy).
        guard let url = URL(string: urlString),
              url.path.lowercased().hasSuffix(".dmg"),
              url.scheme == "https"
                || (url.scheme == "http" && ["127.0.0.1", "localhost"].contains(url.host ?? "")) else {
            if let u = URL(string: urlString) { NSWorkspace.shared.open(u) }
            return
        }
        updateSay("Đang tải bản cập nhật…")
        let task = URLSession.shared.downloadTask(with: url) { temp, resp, err in
            // A 404/500 body is NOT a dmg — verify status and a sane size
            // before ever touching hdiutil.
            let status = (resp as? HTTPURLResponse)?.statusCode ?? 0
            let size = temp.flatMap { try? FileManager.default.attributesOfItem(atPath: $0.path)[.size] as? Int } ?? 0
            guard let temp = temp, err == nil, status == 200, size > 1_000_000 else {
                self.notifLog("selfUpdate download rejected: status=\(status) size=\(size) err=\(String(describing: err))")
                self.updateSay("Tải thất bại (status \(status)) — mở trình duyệt để tải tay.")
                NSWorkspace.shared.open(url)
                return
            }
            do { try self.applyUpdate(dmg: temp) }
            catch {
                self.notifLog("selfUpdate failed: \(error)")
                self.updateSay("Không tự cài được — mở trình duyệt để tải tay.")
                NSWorkspace.shared.open(url)
            }
        }
        task.resume()
    }

    func applyUpdate(dmg: URL) throws {
        let fm = FileManager.default
        let work = fm.temporaryDirectory.appendingPathComponent("coxupdate-\(UUID().uuidString)")
        try fm.createDirectory(at: work, withIntermediateDirectories: true)
        let dmgPath = work.appendingPathComponent("update.dmg")
        try fm.moveItem(at: dmg, to: dmgPath)
        let mount = work.appendingPathComponent("mnt")

        func run(_ launchPath: String, _ args: [String]) throws {
            let p = Process()
            p.launchPath = launchPath
            p.arguments = args
            try p.run()
            p.waitUntilExit()
            if p.terminationStatus != 0 { throw NSError(domain: "coxupdate", code: Int(p.terminationStatus)) }
        }
        try run("/usr/bin/hdiutil", ["attach", dmgPath.path, "-nobrowse", "-readonly", "-mountpoint", mount.path])
        defer { try? run("/usr/bin/hdiutil", ["detach", mount.path, "-force"]) }
        guard let appName = try fm.contentsOfDirectory(atPath: mount.path).first(where: { $0.hasSuffix(".app") }) else {
            throw NSError(domain: "coxupdate", code: 2)
        }
        let staged = work.appendingPathComponent("staged.app")
        try run("/usr/bin/ditto", [mount.appendingPathComponent(appName).path, staged.path])

        let target = Bundle.main.bundleURL.path
        let script = work.appendingPathComponent("swap.sh")
        let sh = """
        #!/bin/bash
        while kill -0 \(ProcessInfo.processInfo.processIdentifier) 2>/dev/null; do sleep 0.3; done
        rm -rf "\(target)"
        /usr/bin/ditto "\(staged.path)" "\(target)"
        /usr/bin/xattr -dr com.apple.quarantine "\(target)" 2>/dev/null
        open "\(target)"
        rm -rf "\(work.path)"
        """
        try sh.write(to: script, atomically: true, encoding: .utf8)
        try run("/bin/chmod", ["+x", script.path])
        let p = Process()
        p.launchPath = "/bin/bash"
        p.arguments = [script.path]
        try p.run() // detached: outlives us
        DispatchQueue.main.async {
            self.updateSay("Đã tải xong — app sẽ tự khởi động lại…")
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { NSApp.terminate(nil) }
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
