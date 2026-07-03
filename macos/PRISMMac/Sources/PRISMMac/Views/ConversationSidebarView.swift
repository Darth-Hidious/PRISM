import SwiftUI

struct ConversationSidebarView: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Label("Threads", systemImage: "message")
                    .font(.headline)

                Spacer()

                Button {
                    store.togglePalette()
                } label: {
                    Image(systemName: "sidebar.leading")
                }
                .buttonStyle(.borderless)
                .help("Collapse threads")
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)

            Divider()

            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    SidebarAction(title: "New materials question", systemImage: "plus.message") {
                        store.chatPrompt = ""
                        store.switchToChat()
                    }

                    SidebarAction(title: "ESA demo workflow", systemImage: "rectangle.connected.to.line.below") {
                        store.switchToWorkflowBoard()
                    }

                    SectionLabel("Recent")

                    ThreadRow(title: "TPS candidate search", detail: "limits visible")
                    ThreadRow(title: "MARC27 API revamp", detail: "capabilities first")
                    ThreadRow(title: "Stripe hardening", detail: "top-up gated")
                    ThreadRow(title: "VS Code extension", detail: "developer surface")

                    SectionLabel("Pinned context")

                    ForEach(store.contextChips) { chip in
                        HStack(spacing: 9) {
                            Image(systemName: chip.systemImage)
                                .foregroundStyle(.secondary)
                                .frame(width: 18)

                            VStack(alignment: .leading, spacing: 2) {
                                Text(chip.title)
                                    .lineLimit(1)
                                Text(chip.detail)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                            }
                        }
                        .padding(.horizontal, 9)
                        .padding(.vertical, 7)
                    }
                }
                .padding(12)
            }
        }
        .background(.bar)
    }
}

private struct SidebarAction: View {
    let title: String
    let systemImage: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .buttonStyle(.bordered)
    }
}

private struct ThreadRow: View {
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .lineLimit(1)
            Text(detail)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .padding(.horizontal, 9)
        .padding(.vertical, 7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.18), in: RoundedRectangle(cornerRadius: PRISMDesign.controlRadius))
    }
}

private struct SectionLabel: View {
    let title: String

    init(_ title: String) {
        self.title = title
    }

    var body: some View {
        Text(title)
            .font(.caption)
            .fontWeight(.semibold)
            .foregroundStyle(.secondary)
            .textCase(.uppercase)
            .padding(.top, 8)
    }
}
