import Foundation

enum AppSection: String, CaseIterable, Identifiable {
    case overview
    case agent
    case knowledge
    case research
    case workflows
    case models
    case compute
    case nodes
    case billing
    case settings

    var id: String { rawValue }

    var title: String {
        switch self {
        case .overview: "Overview"
        case .agent: "Agent"
        case .knowledge: "Knowledge"
        case .research: "Research"
        case .workflows: "Workflows"
        case .models: "Models"
        case .compute: "Compute"
        case .nodes: "Nodes & Mesh"
        case .billing: "Billing"
        case .settings: "Settings"
        }
    }

    var detail: String {
        switch self {
        case .overview: "Status and demo posture"
        case .agent: "Chat, tools, approvals"
        case .knowledge: "Query graph and platform"
        case .research: "Materials research loops"
        case .workflows: "YAML workflow runs"
        case .models: "Hosted LLM catalog"
        case .compute: "Jobs and deployments"
        case .nodes: "Local node and peers"
        case .billing: "Credits and usage"
        case .settings: "Runtime configuration"
        }
    }

    var systemImage: String {
        switch self {
        case .overview: "gauge.with.dots.needle.bottom.50percent"
        case .agent: "sparkles"
        case .knowledge: "point.3.connected.trianglepath.dotted"
        case .research: "doc.text.magnifyingglass"
        case .workflows: "flowchart"
        case .models: "brain.head.profile"
        case .compute: "cpu"
        case .nodes: "network"
        case .billing: "creditcard"
        case .settings: "gearshape"
        }
    }
}

enum FeaturePriority: String {
    case mvp = "MVP"
    case next = "Next"
    case advanced = "Advanced"
    case gated = "Gated"
}

struct FeatureItem: Identifiable, Hashable {
    let id: String
    let title: String
    let summary: String
    let macSurface: String
    let command: String
    let priority: FeaturePriority
    let note: String
}

struct DemoStatusItem: Identifiable, Hashable {
    let id: String
    let title: String
    let value: String
    let systemImage: String
    let isHealthy: Bool
}

