import Foundation
import SwiftUI

@MainActor
final class AppStore: ObservableObject {
    @Published var selection: AppSection? = .overview
    @Published var workbenchMode: WorkbenchMode = .chat
    @Published var selectedWidgetID: CanvasWidget.ID?
    @Published var widgets: [CanvasWidget]
    @Published var connections: [CanvasConnection]
    @Published var canvasPrompt: String = "Find ESA-grade thermal protection candidates with traceable evidence and visible LLM limits."
    @Published var chatPrompt: String = "Find ESA-grade thermal protection candidates with traceable evidence and visible LLM limits."
    @Published var chatMessages: [ChatMessage]
    @Published var isPaletteVisible: Bool = false
    @Published var isInspectorVisible: Bool = false
    @Published var projectRoot: String = "/Users/siddharthakovid/Downloads/PRISM"
    @Published var pythonPath: String = "python3"
    @Published var cliPath: String = "prism"
    @Published var lastRefresh: Date = .now

    let catalog = PRISMCommandCatalog()
    private let widgetCatalog = CanvasWidgetCatalog()

    init() {
        let demoWidgets = widgetCatalog.demoWidgets()
        self.widgets = demoWidgets
        self.connections = widgetCatalog.demoConnections(for: demoWidgets)
        self.selectedWidgetID = demoWidgets.first?.id
        self.chatMessages = [
            ChatMessage(
                id: UUID(),
                role: .assistant,
                title: "PRISM",
                body: "Ask a materials question. I’ll keep evidence, model limits, and export state visible as the work develops.",
                attachments: ["MARC27 capabilities", "LLM limits", "ESA demo board"]
            ),
            ChatMessage(
                id: UUID(),
                role: .assistant,
                title: "Suggested start",
                body: "Try an ESA-grade thermal protection search, a MARC27 capability map, or a Stripe hardening check.",
                attachments: ["Knowledge", "Papers", "Evidence bundle"]
            )
        ]
    }

    var statusItems: [DemoStatusItem] {
        [
            DemoStatusItem(
                id: "protocol",
                title: "Frontend protocol",
                value: "JSON-RPC over stdio",
                systemImage: "arrow.left.arrow.right",
                isHealthy: true
            ),
            DemoStatusItem(
                id: "backend",
                title: "Backend launch",
                value: "prism backend",
                systemImage: "terminal",
                isHealthy: true
            ),
            DemoStatusItem(
                id: "stripe",
                title: "Stripe checkout",
                value: "Gate live top-up",
                systemImage: "exclamationmark.triangle",
                isHealthy: false
            ),
            DemoStatusItem(
                id: "esa",
                title: "ESA posture",
                value: "Governed control room",
                systemImage: "checkmark.seal",
                isHealthy: true
            )
        ]
    }

    func features(for section: AppSection) -> [FeatureItem] {
        catalog.features(for: section)
    }

    func refreshStatus() {
        lastRefresh = .now
    }

    var selectedWidget: CanvasWidget? {
        guard let selectedWidgetID else {
            return nil
        }
        return widgets.first { $0.id == selectedWidgetID }
    }

    func selectWidget(_ id: CanvasWidget.ID) {
        selectedWidgetID = id
    }

    func moveWidget(_ id: CanvasWidget.ID, by translation: CGSize) {
        guard let index = widgets.firstIndex(where: { $0.id == id }) else {
            return
        }

        let next = CGPoint(
            x: max(24, widgets[index].position.x + translation.width),
            y: max(24, widgets[index].position.y + translation.height)
        )
        widgets[index].position = next
    }

    func addWidget(kind: CanvasWidgetKind) {
        let widget = widgetCatalog.newWidget(kind: kind, index: widgets.count)
        widgets.append(widget)
        selectedWidgetID = widget.id
        workbenchMode = .workflow
    }

    func resetCanvas() {
        let demoWidgets = widgetCatalog.demoWidgets()
        widgets = demoWidgets
        connections = widgetCatalog.demoConnections(for: demoWidgets)
        selectedWidgetID = demoWidgets.first?.id
    }

    func runSelectedWidget() {
        guard let selectedWidgetID,
              let index = widgets.firstIndex(where: { $0.id == selectedWidgetID }) else {
            return
        }

        widgets[index].state = widgets[index].state == .gated ? .waiting : .running
        lastRefresh = .now
    }

    func completeSelectedWidget() {
        guard let selectedWidgetID,
              let index = widgets.firstIndex(where: { $0.id == selectedWidgetID }) else {
            return
        }

        widgets[index].state = .complete
        lastRefresh = .now
    }

    func togglePalette() {
        isPaletteVisible.toggle()
    }

    func toggleInspector() {
        isInspectorVisible.toggle()
    }

    func sendChatPrompt() {
        let trimmed = chatPrompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return
        }

        chatMessages.append(
            ChatMessage(
                id: UUID(),
                role: .user,
                title: "You",
                body: trimmed,
                attachments: contextChips.map(\.title)
            )
        )
        chatMessages.append(
            ChatMessage(
                id: UUID(),
                role: .assistant,
                title: "PRISM",
                body: "I’ll answer here first and keep executable work ready for the board: graph search, discourse review, compute jobs, and the ESA evidence bundle.",
                attachments: ["Answer", "Board ready"]
            )
        )
        chatPrompt = ""
        lastRefresh = .now
    }

    func switchToWorkflowBoard() {
        workbenchMode = .workflow
    }

    func switchToChat() {
        workbenchMode = .chat
    }

    var contextChips: [ContextChip] {
        [
            ContextChip(id: "app", title: "Work with PRISM", detail: "local backend", systemImage: "terminal"),
            ContextChip(id: "marc27", title: "MARC27", detail: "capabilities + billing limits", systemImage: "cloud"),
            ContextChip(id: "project", title: "Project", detail: "PRISM workspace", systemImage: "folder"),
            ContextChip(id: "limits", title: "LLM limits", detail: "visible before action", systemImage: "speedometer")
        ]
    }

    var canvasSummary: String {
        let running = widgets.filter { $0.state == .running }.count
        let gated = widgets.filter { $0.state == .gated }.count
        let complete = widgets.filter { $0.state == .complete }.count
        return "\(widgets.count) widgets   \(connections.count) links   \(running) running   \(gated) gated   \(complete) complete"
    }
}
