import SwiftUI

struct WidgetInspectorView: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Label("Inspector", systemImage: "sidebar.right")
                    .font(.headline)
                Spacer()
                Text(store.lastRefresh, style: .time)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button {
                    store.toggleInspector()
                } label: {
                    Image(systemName: "sidebar.right")
                }
                .buttonStyle(.borderless)
                .help("Collapse inspector")
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)

            Divider()

            if let widget = store.selectedWidget {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        WidgetSummaryPanel(widget: widget)
                        IOSection(title: "Inputs", values: widget.inputs)
                        IOSection(title: "Outputs", values: widget.outputs)
                        CommandPanel(widget: widget)
                        ActionPanel(widget: widget)
                    }
                    .padding(16)
                }
            } else {
                ContentUnavailableView(
                    "No Widget Selected",
                    systemImage: "square.dashed",
                    description: Text("Select a widget on the canvas to inspect its state, limits, command, and outputs.")
                )
            }
        }
        .background(.bar)
    }
}

private struct WidgetSummaryPanel: View {
    let widget: CanvasWidget

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 10) {
                ZStack {
                    RoundedRectangle(cornerRadius: 6)
                        .fill(widget.kind.tint.opacity(0.14))
                    Image(systemName: widget.kind.systemImage)
                        .foregroundStyle(widget.kind.tint)
                }
                .frame(width: 32, height: 32)

                VStack(alignment: .leading, spacing: 4) {
                    Text(widget.title)
                        .font(.title3)
                        .fontWeight(.semibold)
                        .lineLimit(2)

                    Label(widget.state.rawValue, systemImage: widget.state.systemImage)
                        .font(.caption)
                        .foregroundStyle(widget.state.tint)
                }
            }

            Text(widget.summary)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .background(.quaternary.opacity(0.24), in: RoundedRectangle(cornerRadius: 8))
    }
}

private struct IOSection: View {
    let title: String
    let values: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.caption)
                .fontWeight(.semibold)
                .foregroundStyle(.secondary)
                .textCase(.uppercase)

            ForEach(values, id: \.self) { value in
                HStack(alignment: .top, spacing: 7) {
                    Image(systemName: "smallcircle.filled.circle")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Text(value)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                }
                .font(.callout)
            }
        }
    }
}

private struct CommandPanel: View {
    let widget: CanvasWidget

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Contract")
                .font(.caption)
                .fontWeight(.semibold)
                .foregroundStyle(.secondary)
                .textCase(.uppercase)

            Text(widget.command)
                .font(.system(.callout, design: .monospaced))
                .textSelection(.enabled)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.quaternary.opacity(0.28), in: RoundedRectangle(cornerRadius: 6))
        }
    }
}

private struct ActionPanel: View {
    @EnvironmentObject private var store: AppStore
    let widget: CanvasWidget

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Actions")
                .font(.caption)
                .fontWeight(.semibold)
                .foregroundStyle(.secondary)
                .textCase(.uppercase)

            Button {
                store.runSelectedWidget()
            } label: {
                Label(widget.state == .gated ? "Request approval" : "Run widget", systemImage: widget.state == .gated ? "lock.open" : "play")
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.borderedProminent)

            HStack {
                Button {
                    store.addWidget(kind: widget.kind)
                } label: {
                    Label("Branch", systemImage: "arrow.triangle.branch")
                }

                Button {
                    store.completeSelectedWidget()
                } label: {
                    Label("Mark done", systemImage: "checkmark")
                }
            }
        }
    }
}
