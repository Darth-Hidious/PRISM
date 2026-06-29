import SwiftUI

struct SettingsView: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        Form {
            Section("Runtime") {
                TextField("Project Root", text: $store.projectRoot)
                TextField("Python", text: $store.pythonPath)
                TextField("PRISM CLI", text: $store.cliPath)
            }

            Section("Backend") {
                LabeledContent("Protocol", value: "JSON-RPC over stdio")
                LabeledContent("Launch command", value: "prism backend")
                LabeledContent("Last refresh", value: store.lastRefresh.formatted(date: .abbreviated, time: .standard))
            }

            Section("Policy") {
                LabeledContent("Stripe top-up", value: "Warning-gated")
                LabeledContent("Secrets", value: "Never displayed")
                LabeledContent("Local tools", value: "Approval-gated")
            }
        }
        .formStyle(.grouped)
        .padding()
    }
}

