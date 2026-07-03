import SwiftUI

struct ContextInspectorView: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Label("Context", systemImage: "app.connected.to.app.below.fill")
                    .font(.headline)
                Spacer()
                Button {
                    store.toggleInspector()
                } label: {
                    Image(systemName: "sidebar.right")
                }
                .buttonStyle(.borderless)
                .help("Collapse context")
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)

            Divider()

            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    InspectorSection(title: "Work With") {
                        ForEach(store.contextChips) { chip in
                            HStack(alignment: .top, spacing: 9) {
                                Image(systemName: chip.systemImage)
                                    .foregroundStyle(.secondary)
                                    .frame(width: 18)

                                VStack(alignment: .leading, spacing: 2) {
                                    Text(chip.title)
                                    Text(chip.detail)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }
                        }
                    }

                    InspectorSection(title: "Limits") {
                        LimitRow(title: "Context", value: "model-defined")
                        LimitRow(title: "Tool calls", value: "approval-gated")
                        LimitRow(title: "Compute", value: "budget required")
                        LimitRow(title: "Stripe", value: "read-only")
                    }

                    InspectorSection(title: "Artifacts") {
                        ArtifactRow(title: "ESA evidence bundle", status: "not created")
                        ArtifactRow(title: "Workflow board", status: "\(store.widgets.count) widgets")
                        ArtifactRow(title: "MARC27 capability map", status: "available")
                    }

                    Button {
                        store.switchToWorkflowBoard()
                    } label: {
                        Label("Open workflow board", systemImage: "point.3.connected.trianglepath.dotted")
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.borderedProminent)
                }
                .padding(16)
            }
        }
        .background(.bar)
    }
}

private struct InspectorSection<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.caption)
                .fontWeight(.semibold)
                .foregroundStyle(.secondary)
                .textCase(.uppercase)

            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct LimitRow: View {
    let title: String
    let value: String

    var body: some View {
        HStack {
            Text(title)
            Spacer()
            Text(value)
                .foregroundStyle(.secondary)
        }
        .font(.callout)
    }
}

private struct ArtifactRow: View {
    let title: String
    let status: String

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
            Text(status)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.2), in: RoundedRectangle(cornerRadius: PRISMDesign.controlRadius))
    }
}
