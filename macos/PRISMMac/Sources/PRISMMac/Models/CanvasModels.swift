import CoreGraphics
import Foundation
import SwiftUI

enum WorkbenchMode: String, CaseIterable, Identifiable {
    case chat = "Chat"
    case workflow = "Workflow Board"

    var id: String { rawValue }

    var systemImage: String {
        switch self {
        case .chat: "message"
        case .workflow: "point.3.connected.trianglepath.dotted"
        }
    }
}

enum ChatMessageRole {
    case user
    case assistant
}

struct ChatMessage: Identifiable, Hashable {
    let id: UUID
    let role: ChatMessageRole
    let title: String
    let body: String
    let attachments: [String]
}

struct ContextChip: Identifiable, Hashable {
    let id: String
    let title: String
    let detail: String
    let systemImage: String
}

enum CanvasWidgetKind: String, CaseIterable, Identifiable {
    case question
    case constraints
    case knowledge
    case papers
    case materials
    case modelLimits
    case discourse
    case workflow
    case simulation
    case evidenceBundle
    case billingGuardrail
    case fabricNode

    var id: String { rawValue }

    var title: String {
        switch self {
        case .question: "Question"
        case .constraints: "Constraints"
        case .knowledge: "Knowledge"
        case .papers: "Papers"
        case .materials: "Materials"
        case .modelLimits: "Model Limits"
        case .discourse: "Discourse"
        case .workflow: "Workflow"
        case .simulation: "Simulation"
        case .evidenceBundle: "Evidence Bundle"
        case .billingGuardrail: "Billing Guardrail"
        case .fabricNode: "Fabric Node"
        }
    }

    var systemImage: String {
        switch self {
        case .question: "sparkles"
        case .constraints: "slider.horizontal.3"
        case .knowledge: "point.3.connected.trianglepath.dotted"
        case .papers: "doc.text.magnifyingglass"
        case .materials: "atom"
        case .modelLimits: "speedometer"
        case .discourse: "bubble.left.and.bubble.right"
        case .workflow: "flowchart"
        case .simulation: "cpu"
        case .evidenceBundle: "shippingbox"
        case .billingGuardrail: "creditcard.and.123"
        case .fabricNode: "network"
        }
    }

    var tint: Color {
        switch self {
        case .question: .blue
        case .constraints: .teal
        case .knowledge: .mint
        case .papers: .indigo
        case .materials: .cyan
        case .modelLimits: .orange
        case .discourse: .purple
        case .workflow: .pink
        case .simulation: .red
        case .evidenceBundle: .green
        case .billingGuardrail: .yellow
        case .fabricNode: .brown
        }
    }

    var defaultSize: CGSize {
        switch self {
        case .question, .evidenceBundle:
            CGSize(width: 300, height: 172)
        case .discourse, .simulation:
            CGSize(width: 280, height: 158)
        default:
            CGSize(width: 250, height: 142)
        }
    }
}

enum CanvasWidgetState: String {
    case idle = "Idle"
    case ready = "Ready"
    case running = "Running"
    case waiting = "Waiting"
    case complete = "Complete"
    case gated = "Gated"

    var systemImage: String {
        switch self {
        case .idle: "circle"
        case .ready: "checkmark.circle"
        case .running: "play.circle"
        case .waiting: "hourglass"
        case .complete: "checkmark.seal"
        case .gated: "lock.trianglebadge.exclamationmark"
        }
    }

    var tint: Color {
        switch self {
        case .idle: .secondary
        case .ready: .blue
        case .running: .orange
        case .waiting: .purple
        case .complete: .green
        case .gated: .red
        }
    }
}

struct CanvasWidget: Identifiable, Hashable {
    let id: UUID
    var kind: CanvasWidgetKind
    var title: String
    var subtitle: String
    var summary: String
    var command: String
    var state: CanvasWidgetState
    var position: CGPoint
    var size: CGSize
    var inputs: [String]
    var outputs: [String]

    var center: CGPoint {
        CGPoint(x: position.x + size.width / 2, y: position.y + size.height / 2)
    }
}

struct CanvasConnection: Identifiable, Hashable {
    let id: UUID
    let from: UUID
    let to: UUID
    let label: String
}
