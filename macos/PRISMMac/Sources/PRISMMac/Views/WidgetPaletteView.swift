import SwiftUI

struct WidgetPaletteView: View {
    @EnvironmentObject private var store: AppStore

    private var primaryKinds: [CanvasWidgetKind] {
        [.question, .constraints, .knowledge, .papers, .materials, .modelLimits]
    }

    private var executionKinds: [CanvasWidgetKind] {
        [.discourse, .workflow, .simulation, .evidenceBundle, .billingGuardrail, .fabricNode]
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Label("Widgets", systemImage: "square.grid.3x3")
                    .font(.headline)

                Spacer()

                Button {
                    store.togglePalette()
                } label: {
                    Image(systemName: "sidebar.leading")
                }
                .buttonStyle(.borderless)
                .help("Collapse widgets")
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)

            Divider()

            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    PaletteSection(title: "Think", kinds: primaryKinds)
                    PaletteSection(title: "Act", kinds: executionKinds)

                    Button {
                        store.resetCanvas()
                    } label: {
                        Label("Load ESA demo board", systemImage: "rectangle.3.group")
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.bordered)
                }
                .padding(14)
            }
        }
        .background(.bar)
    }
}

private struct PaletteSection: View {
    @EnvironmentObject private var store: AppStore
    let title: String
    let kinds: [CanvasWidgetKind]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.caption)
                .fontWeight(.semibold)
                .foregroundStyle(.secondary)
                .textCase(.uppercase)

            ForEach(kinds) { kind in
                Button {
                    store.addWidget(kind: kind)
                } label: {
                    HStack(spacing: 9) {
                        Image(systemName: kind.systemImage)
                            .foregroundStyle(kind.tint)
                            .frame(width: 18)
                        Text(kind.title)
                            .lineLimit(1)
                        Spacer()
                        Image(systemName: "plus")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .padding(.horizontal, 9)
                .padding(.vertical, 8)
                .background(.quaternary.opacity(0.32), in: RoundedRectangle(cornerRadius: 6))
                .overlay {
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(.quaternary)
                }
            }
        }
    }
}
