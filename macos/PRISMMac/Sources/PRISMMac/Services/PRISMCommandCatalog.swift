import Foundation

struct PRISMCommandCatalog {
    func features(for section: AppSection) -> [FeatureItem] {
        switch section {
        case .overview:
            overview
        case .agent:
            agent
        case .knowledge:
            knowledge
        case .research:
            research
        case .workflows:
            workflows
        case .models:
            models
        case .compute:
            compute
        case .nodes:
            nodes
        case .billing:
            billing
        case .settings:
            settings
        }
    }

    private var overview: [FeatureItem] {
        [
            item("Native shell", "Sidebar/detail Mac workbench over the existing backend protocol.", "NavigationSplitView", "open macos/PRISMMac/Package.swift", .mvp, "This is the first scaffold."),
            item("Protocol bridge", "Start the Rust backend and fold UI events into Swift models.", "Process + JSON-RPC client", "prism backend --project-root . --python python3", .next, "Next implementation slice."),
            item("ESA demo mode", "Show health, capabilities, billing packages, and LLM guardrails without exposing secrets.", "Read-only dashboard cards", "prism status", .next, "Keep payment actions gated.")
        ]
    }

    private var agent: [FeatureItem] {
        [
            item("Agent chat", "Render streaming text, tool calls, approval prompts, costs, and turn completion.", "Timeline + composer", "prism backend", .mvp, "Use ui.* events from FRONTEND_PROTOCOL."),
            item("Resume session", "Open prior conversations from a native picker.", "Session picker sheet", "prism resume", .next, "Mac should make resume visual."),
            item("Tool approvals", "Approve once, deny, or allow for session from native cards.", "Approval card", "approval.respond", .mvp, "Keep local actions explicit.")
        ]
    }

    private var knowledge: [FeatureItem] {
        [
            item("Platform query", "Ask MARC27 knowledge graph using natural language.", "Query form", "prism query --platform \"titanium alloys\" --json", .mvp, "Best ESA-safe live surface."),
            item("Semantic search", "Search by meaning with a result limit.", "Search controls", "prism query --semantic \"creep resistance\" --json", .mvp, "Good for materials discovery."),
            item("Cypher mode", "Expose direct graph queries to advanced users.", "Advanced disclosure", "prism query --cypher \"MATCH (a:Alloy) RETURN a\" --json", .advanced, "Keep away from default flow.")
        ]
    }

    private var research: [FeatureItem] {
        [
            item("Research loop", "Run a materials-science research question with depth control.", "Goal editor + stream", "prism research \"novel refractory alloys\" --depth 1 --json", .mvp, "This is the ESA headline surface."),
            item("Result artifact", "Save a research transcript or bundle for review.", "Export action", "prism research <query> --json", .next, "Needs event folding first.")
        ]
    }

    private var workflows: [FeatureItem] {
        [
            item("Workflow list", "Browse YAML workflows discovered by PRISM.", "List + detail", "prism workflow list", .mvp, "Start read-only."),
            item("Workflow run", "Edit parameters and choose dry-run or execute.", "Parameter form", "prism workflow run explore --set space=Ni-Cr-Co --execute", .mvp, "Map --set pairs to rows."),
            item("Workflow show", "Show definition and required inputs.", "Inspector pane", "prism workflow show explore", .mvp, "Good first JSON-RPC command view.")
        ]
    }

    private var models: [FeatureItem] {
        [
            item("Model catalog", "List hosted models by provider.", "Provider tabs", "prism models list --json", .mvp, "Backed by MARC27 model catalog."),
            item("Model search", "Search by model id, display name, or provider.", "Search field", "prism models search claude --json", .mvp, "Useful for chat target selection."),
            item("Chat target", "Choose MARC27, local OpenAI-compatible server, or direct vendor.", "Settings panel", "prism use show", .mvp, "Do not store cloud provider keys in the app.")
        ]
    }

    private var compute: [FeatureItem] {
        [
            item("Run job", "Submit a container job to local, MARC27, or BYOC.", "Job form", "prism run python:3.11 --backend marc27 --json", .next, "Needs budget/status guardrails."),
            item("Job status", "Inspect one compute job by UUID.", "Status card", "prism job-status <uuid>", .mvp, "Read-only status is safe."),
            item("Deployments", "List, inspect, stop, and health-check services.", "Deployment table", "prism deploy list --json", .next, "Stop should ask confirmation.")
        ]
    }

    private var nodes: [FeatureItem] {
        [
            item("Node status", "Show local node capabilities and service health.", "Status cards", "prism node status", .mvp, "Useful for local demos."),
            item("Mesh discover", "Find LAN peers via mDNS.", "Peer list", "prism mesh discover", .next, "Keep timeout configurable."),
            item("Fabric identity", "Show cross-org identity and peer orgs.", "Read-only fabric panel", "prism federation whoami --json", .mvp, "Good governance proof point."),
            item("Key rotation", "Rotate E2EE node keypair.", "Danger action", "prism node key rotate", .gated, "Requires explicit destructive confirmation.")
        ]
    }

    private var billing: [FeatureItem] {
        [
            item("Balance", "Show credit balance and usage summary.", "Billing cards", "prism billing", .mvp, "Read-only and demo-safe."),
            item("Usage", "Break down spend by service.", "Usage table", "prism billing usage", .mvp, "ESA wants governance evidence."),
            item("Prices", "Show pricing table and packages.", "Package grid", "prism billing prices", .mvp, "Safe to show without payment."),
            item("Top up", "Open Stripe checkout.", "Warning-gated button", "prism billing topup starter", .gated, "Wait for webhook/idempotency hardening before ESA live use.")
        ]
    }

    private var settings: [FeatureItem] {
        [
            item("Runtime paths", "Set project root, Python path, and CLI binary.", "Settings scene", "prism status", .mvp, "Use AppStorage later."),
            item("Configure LLM", "View and edit local PRISM chat configuration.", "Form", "prism configure --show", .next, "Keep secrets out of persisted config."),
            item("Login", "Run device-flow auth and show status.", "Account sheet", "prism login", .mvp, "Never display token values.")
        ]
    }

    private func item(
        _ title: String,
        _ summary: String,
        _ macSurface: String,
        _ command: String,
        _ priority: FeaturePriority,
        _ note: String
    ) -> FeatureItem {
        FeatureItem(
            id: "\(title)-\(command)",
            title: title,
            summary: summary,
            macSurface: macSurface,
            command: command,
            priority: priority,
            note: note
        )
    }
}

