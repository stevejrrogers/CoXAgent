// CoXAgent macOS shell — a native window (WKWebView) that boots the bundled
// `coxagent hub` and loads the dashboard. No external browser, no server to
// start by hand. Unsigned/ad-hoc — build & run locally without a cert.
import Cocoa
import WebKit

let PORT = 4000

final class AppDelegate: NSObject, NSApplicationDelegate, WKNavigationDelegate {
    var window: NSWindow!
    var web: WKWebView!
    var hub: Process?

    func applicationDidFinishLaunching(_ note: Notification) {
        startHub()

        let cfg = WKWebViewConfiguration()
        web = WKWebView(frame: NSMakeRect(0, 0, 1360, 860), configuration: cfg)
        web.navigationDelegate = self

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

    func startHub() {
        let fm = FileManager.default
        let ws = workspace()
        let reg = ws + "/registry.json"
        let stateDir = ws + "/default/state"
        try? fm.createDirectory(atPath: stateDir, withIntermediateDirectories: true)
        try? fm.createDirectory(atPath: ws + "/default/codebase", withIntermediateDirectories: true)

        let wsURL = URL(fileURLWithPath: ws)

        // First run: onboard a default project and write the registry.
        if !fm.fileExists(atPath: reg) {
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
        p.environment = richEnv()
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
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
