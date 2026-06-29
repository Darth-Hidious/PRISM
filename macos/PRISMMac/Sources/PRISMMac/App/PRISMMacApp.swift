import AppKit
import SwiftUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }
}

@main
struct PRISMMacApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var store = AppStore()

    var body: some Scene {
        WindowGroup("PRISM Mac", id: "main") {
            ContentView()
                .environmentObject(store)
                .frame(minWidth: 1080, minHeight: 720)
        }
        .commands {
            CommandMenu("PRISM") {
                Button("New Query") {
                    store.selection = .knowledge
                }
                .keyboardShortcut("n", modifiers: [.command])

                Button("Run Workflow") {
                    store.selection = .workflows
                }
                .keyboardShortcut("r", modifiers: [.command, .shift])

                Divider()

                Button("Refresh Status") {
                    store.refreshStatus()
                }
                .keyboardShortcut("r", modifiers: [.command])
            }
        }

        Settings {
            SettingsView()
                .environmentObject(store)
                .frame(width: 560)
        }
    }
}

