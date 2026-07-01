import SwiftUI

struct CanvasWorkbenchView: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        HStack(spacing: 0) {
            if store.isPaletteVisible {
                Group {
                    if store.workbenchMode == .chat {
                        ConversationSidebarView()
                    } else {
                        WidgetPaletteView()
                    }
                }
                .frame(width: store.workbenchMode == .chat ? PRISMDesign.conversationSidebarWidth : PRISMDesign.workflowSidebarWidth)

                Divider()
            }

            Group {
                switch store.workbenchMode {
                case .chat:
                    ChatSurfaceView()
                case .workflow:
                    WorkflowCanvasSurfaceView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            if store.isInspectorVisible {
                Divider()

                Group {
                    if store.workbenchMode == .chat {
                        ContextInspectorView()
                    } else {
                        WidgetInspectorView()
                    }
                }
                .frame(width: store.workbenchMode == .chat ? PRISMDesign.contextInspectorWidth : PRISMDesign.workflowInspectorWidth)
            }
        }
        .toolbar {
            ToolbarItemGroup {
                Button {
                    store.togglePalette()
                } label: {
                    Label("Sidebar", systemImage: "sidebar.leading")
                }

                Picker("Mode", selection: $store.workbenchMode) {
                    ForEach(WorkbenchMode.allCases) { mode in
                        Label(mode.rawValue, systemImage: mode.systemImage)
                            .tag(mode)
                    }
                }
                .pickerStyle(.segmented)
                .frame(width: 240)

                Button {
                    if store.workbenchMode == .chat {
                        store.sendChatPrompt()
                    } else {
                        store.runSelectedWidget()
                    }
                } label: {
                    Label(store.workbenchMode == .chat ? "Send" : "Run", systemImage: store.workbenchMode == .chat ? "arrow.up.circle.fill" : "play.fill")
                }
                .disabled(
                    store.workbenchMode == .chat
                    ? store.chatPrompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    : store.selectedWidget == nil
                )

                if store.workbenchMode == .workflow {
                    Button {
                        store.completeSelectedWidget()
                    } label: {
                        Label("Complete", systemImage: "checkmark")
                    }
                    .disabled(store.selectedWidget == nil)

                    Button {
                        store.resetCanvas()
                    } label: {
                        Label("Reset", systemImage: "arrow.counterclockwise")
                    }
                }

                Button {
                    store.refreshStatus()
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }

                Button {
                    store.toggleInspector()
                } label: {
                    Label("Inspector", systemImage: "sidebar.right")
                }
            }
        }
    }
}

private struct WorkflowCanvasSurfaceView: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .topLeading) {
                CanvasBackdrop()

                ZStack(alignment: .topLeading) {
                    CanvasGrid()

                    ConnectionLayer(
                        widgets: store.widgets,
                        connections: store.connections
                    )

                    ForEach(store.widgets) { widget in
                        DraggableWidgetHost(widget: widget)
                    }
                }
                .frame(width: max(proxy.size.width, 1580), height: max(proxy.size.height, 820), alignment: .topLeading)

                CanvasTopBar()
                    .padding(18)

                VStack {
                    Spacer()
                    CanvasPromptBar()
                        .padding(.horizontal, 22)
                        .padding(.bottom, 18)
                }
            }
            .clipShape(Rectangle())
        }
    }
}

private struct CanvasTopBar: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("ESA Demo Board")
                    .font(.headline)
                Text(store.canvasSummary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            Button {
                store.runSelectedWidget()
            } label: {
                Label("Run Selection", systemImage: "play.fill")
            }
            .buttonStyle(.borderedProminent)
            .disabled(store.selectedWidget == nil)

            Button {
                store.addWidget(kind: .evidenceBundle)
            } label: {
                Label("Bundle", systemImage: "shippingbox")
            }
            .buttonStyle(.bordered)

            StatusChip(title: "Backend", value: "local", systemImage: "terminal")
            StatusChip(title: "MARC27", value: "capabilities", systemImage: "cloud")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .prismGlassSurface(cornerRadius: PRISMDesign.floatingControlRadius, interactive: true)
        .shadow(color: .black.opacity(0.08), radius: 12, y: 6)
    }
}

private struct CanvasPromptBar: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "sparkles")
                .foregroundStyle(.blue)

            TextField("Ask PRISM to build or revise the board", text: $store.canvasPrompt)
                .textFieldStyle(.plain)

            Button {
                store.addWidget(kind: .question)
            } label: {
                Label("Run", systemImage: "arrow.up.circle.fill")
            }
            .buttonStyle(.borderedProminent)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .prismGlassSurface(cornerRadius: PRISMDesign.floatingControlRadius, interactive: true)
        .shadow(color: .black.opacity(0.12), radius: 18, y: 8)
    }
}

private struct CanvasBackdrop: View {
    var body: some View {
        LinearGradient(
            colors: [
                Color(nsColor: .textBackgroundColor).opacity(0.72),
                Color(nsColor: .windowBackgroundColor)
            ],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
        .ignoresSafeArea()
    }
}

private struct CanvasGrid: View {
    var body: some View {
        Canvas { context, size in
            let spacing: CGFloat = 32
            var path = Path()

            var x: CGFloat = 0
            while x <= size.width {
                path.move(to: CGPoint(x: x, y: 0))
                path.addLine(to: CGPoint(x: x, y: size.height))
                x += spacing
            }

            var y: CGFloat = 0
            while y <= size.height {
                path.move(to: CGPoint(x: 0, y: y))
                path.addLine(to: CGPoint(x: size.width, y: y))
                y += spacing
            }

            context.stroke(path, with: .color(.secondary.opacity(0.055)), lineWidth: 1)
        }
    }
}

private struct ConnectionLayer: View {
    let widgets: [CanvasWidget]
    let connections: [CanvasConnection]

    var body: some View {
        Canvas { context, _ in
            for connection in connections {
                guard let from = widgets.first(where: { $0.id == connection.from }),
                      let to = widgets.first(where: { $0.id == connection.to }) else {
                    continue
                }

                let start = CGPoint(x: from.position.x + from.size.width + 5, y: from.center.y)
                let end = CGPoint(x: to.position.x - 5, y: to.center.y)
                let midX = (start.x + end.x) / 2
                var path = Path()
                path.move(to: start)
                path.addCurve(
                    to: end,
                    control1: CGPoint(x: midX, y: start.y),
                    control2: CGPoint(x: midX, y: end.y)
                )
                context.stroke(path, with: .color(.secondary.opacity(0.26)), lineWidth: 1.5)

                let marker = Path(ellipseIn: CGRect(x: end.x - 3, y: end.y - 3, width: 6, height: 6))
                context.fill(marker, with: .color(.secondary.opacity(0.42)))

                let labelPosition = CGPoint(x: midX - 24, y: ((start.y + end.y) / 2) - 16)
                context.draw(
                    Text(connection.label)
                        .font(.caption2)
                        .foregroundStyle(.secondary),
                    at: labelPosition,
                    anchor: .leading
                )
            }
        }
        .allowsHitTesting(false)
    }
}

private struct DraggableWidgetHost: View {
    @EnvironmentObject private var store: AppStore
    let widget: CanvasWidget
    @State private var dragOffset: CGSize = .zero

    var body: some View {
        CanvasWidgetView(
            widget: widget,
            isSelected: store.selectedWidgetID == widget.id
        )
        .frame(width: widget.size.width, height: widget.size.height)
        .position(
            x: widget.position.x + widget.size.width / 2 + dragOffset.width,
            y: widget.position.y + widget.size.height / 2 + dragOffset.height
        )
        .onTapGesture {
            store.selectWidget(widget.id)
        }
        .gesture(
            DragGesture()
                .onChanged { value in
                    dragOffset = value.translation
                    store.selectWidget(widget.id)
                }
                .onEnded { value in
                    store.moveWidget(widget.id, by: value.translation)
                    dragOffset = .zero
                }
        )
    }
}

private struct StatusChip: View {
    let title: String
    let value: String
    let systemImage: String

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: systemImage)
                .foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 0) {
                Text(title)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Text(value)
                    .font(.caption)
                    .fontWeight(.medium)
            }
        }
        .padding(.horizontal, 9)
        .padding(.vertical, 6)
        .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 6))
    }
}
