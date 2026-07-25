import AppKit
import CompanionCore
import SwiftUI

/// Menu-bar-only companion: shows hub health, offers quick actions.
/// The full dashboard lives in the desktop shell / browser.
@main
struct CompanionApp: App {
    @StateObject private var model = HubModel(config: .fromEnvironment())

    var body: some Scene {
        MenuBarExtra("CoXAgent", systemImage: model.status.symbolName) {
            Text(model.status.menuTitle)
            Divider()
            Button("Open Dashboard") { model.openDashboard() }
                .disabled(!model.status.isUp)
            Button("Launch CoXAgent App") { model.launchDesktopApp() }
            Button("Open Hub Log") { model.openHubLog() }
            Divider()
            Button("Refresh Now") { Task { await model.refresh() } }
            Button("Quit Companion") { NSApp.terminate(nil) }
        }
    }
}

/// Presentation model: polls the hub every 5 s and exposes the latest status.
@MainActor
final class HubModel: ObservableObject {
    @Published private(set) var status: HubStatus = .unknown

    private let config: CompanionConfig
    private let client: HubClient
    private var pollTask: Task<Void, Never>?

    init(config: CompanionConfig) {
        self.config = config
        self.client = HubClient(config: config)
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(nanoseconds: 5_000_000_000)
            }
        }
    }

    deinit { pollTask?.cancel() }

    func refresh() async {
        status = await client.probe()
    }

    func openDashboard() {
        NSWorkspace.shared.open(config.dashboardURL)
    }

    /// Launch the full desktop app (which boots the hub if needed); falls
    /// back to the dashboard URL when the app is not installed.
    func launchDesktopApp() {
        let appURL = URL(fileURLWithPath: "/Applications/CoXAgent.app")
        if FileManager.default.fileExists(atPath: appURL.path) {
            NSWorkspace.shared.openApplication(
                at: appURL, configuration: NSWorkspace.OpenConfiguration()
            )
        } else {
            NSWorkspace.shared.open(config.dashboardURL)
        }
    }

    func openHubLog() {
        NSWorkspace.shared.open(config.hubLogURL)
    }
}
