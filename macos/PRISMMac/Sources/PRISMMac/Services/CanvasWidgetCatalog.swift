import CoreGraphics
import Foundation

struct CanvasWidgetCatalog {
    func demoWidgets() -> [CanvasWidget] {
        let question = widget(
            .question,
            title: "ESA materials question",
            subtitle: "Goal node",
            summary: "Find TPS and high-temperature materials candidates with traceable evidence, cost limits, and reproducible outputs.",
            command: "input.message",
            state: .ready,
            position: CGPoint(x: 120, y: 110),
            inputs: ["Mission profile", "Temperature window", "Failure mode"],
            outputs: ["Research plan", "Candidate branches"]
        )

        let constraints = widget(
            .constraints,
            title: "Mission constraints",
            subtitle: "Editable assumptions",
            summary: "Temperature, density, manufacturability, evidence depth, model budget, and approval policy live here.",
            command: "local widget",
            state: .ready,
            position: CGPoint(x: 120, y: 340),
            inputs: ["User edits"],
            outputs: ["Bounded search space"]
        )

        let knowledge = widget(
            .knowledge,
            title: "Knowledge graph",
            subtitle: "MARC27 graph + semantic search",
            summary: "Search entities, neighbors, properties, provenance, and embedding matches from the platform.",
            command: "prism query --platform --semantic <query> --json",
            state: .ready,
            position: CGPoint(x: 500, y: 90),
            inputs: ["Question", "Constraints"],
            outputs: ["Entities", "Relations", "Scores"]
        )

        let papers = widget(
            .papers,
            title: "Paper evidence",
            subtitle: "DOI and corpus retrieval",
            summary: "Collect cited papers and source bytes for reviewable evidence rather than loose prose.",
            command: "prism research <query> --json",
            state: .waiting,
            position: CGPoint(x: 500, y: 280),
            inputs: ["Search terms", "Entity names"],
            outputs: ["Citations", "Extracted claims"]
        )

        let materials = widget(
            .materials,
            title: "Candidate table",
            subtitle: "Editable comparison widget",
            summary: "Rank candidate material systems with properties, uncertainties, evidence count, and known gaps.",
            command: "ui.card materials",
            state: .idle,
            position: CGPoint(x: 500, y: 475),
            inputs: ["Graph hits", "Paper claims"],
            outputs: ["Shortlist", "Rejected candidates"]
        )

        let limits = widget(
            .modelLimits,
            title: "LLM limits",
            subtitle: "Visible guardrail",
            summary: "Context window, max output, max tool calls, credit ceiling, and provider policy are shown before runs.",
            command: "GET /api/v1/agent/capabilities",
            state: .complete,
            position: CGPoint(x: 860, y: 90),
            inputs: ["Capabilities", "Model choice"],
            outputs: ["Run envelope", "Quota warnings"]
        )

        let discourse = widget(
            .discourse,
            title: "Discourse review",
            subtitle: "Multi-agent critique",
            summary: "Have domain agents challenge the shortlist and surface missing evidence before a final claim.",
            command: "prism discourse run <spec> --json",
            state: .idle,
            position: CGPoint(x: 860, y: 292),
            inputs: ["Shortlist", "Evidence", "Limits"],
            outputs: ["Verdict", "Open questions"]
        )

        let workflow = widget(
            .workflow,
            title: "Workflow run",
            subtitle: "Parameter form",
            summary: "Turn YAML workflows into visible, branchable, dry-run-first widgets.",
            command: "prism workflow run <name> --json",
            state: .idle,
            position: CGPoint(x: 860, y: 500),
            inputs: ["Candidate", "Parameters"],
            outputs: ["Job spec", "Dry-run summary"]
        )

        let simulation = widget(
            .simulation,
            title: "Simulation job",
            subtitle: "Budget-gated compute",
            summary: "Submit a bounded compute job only after approvals, budget, and idempotency are visible.",
            command: "prism run <image> --json",
            state: .gated,
            position: CGPoint(x: 1210, y: 220),
            inputs: ["Workflow job spec"],
            outputs: ["Job id", "Artifacts", "Logs"]
        )

        let bundle = widget(
            .evidenceBundle,
            title: "ESA evidence bundle",
            subtitle: "Exportable proof",
            summary: "Collect prompts, model limits, citations, graph mutations, job artifacts, and final answer into one reviewable bundle.",
            command: "prism publish <path> --json",
            state: .idle,
            position: CGPoint(x: 1210, y: 485),
            inputs: ["Verdict", "Artifacts", "Citations"],
            outputs: ["Bundle manifest", "Reviewer export"]
        )

        return [
            question,
            constraints,
            knowledge,
            papers,
            materials,
            limits,
            discourse,
            workflow,
            simulation,
            bundle
        ]
    }

    func demoConnections(for widgets: [CanvasWidget]) -> [CanvasConnection] {
        func id(_ kind: CanvasWidgetKind) -> UUID {
            widgets.first { $0.kind == kind }?.id ?? UUID()
        }

        return [
            connection(id(.question), id(.knowledge), "query"),
            connection(id(.question), id(.papers), "research"),
            connection(id(.constraints), id(.knowledge), "bounds"),
            connection(id(.knowledge), id(.materials), "entities"),
            connection(id(.papers), id(.materials), "evidence"),
            connection(id(.materials), id(.discourse), "shortlist"),
            connection(id(.modelLimits), id(.discourse), "limits"),
            connection(id(.discourse), id(.workflow), "verdict"),
            connection(id(.workflow), id(.simulation), "job spec"),
            connection(id(.simulation), id(.evidenceBundle), "artifacts"),
            connection(id(.discourse), id(.evidenceBundle), "claims")
        ]
    }

    func newWidget(kind: CanvasWidgetKind, index: Int) -> CanvasWidget {
        widget(
            kind,
            title: kind.title,
            subtitle: "New widget",
            summary: "Drop this widget into the PRISM canvas, connect it to context, then run or inspect it.",
            command: defaultCommand(for: kind),
            state: kind == .billingGuardrail ? .gated : .idle,
            position: CGPoint(x: 160 + CGFloat(index % 4) * 90, y: 140 + CGFloat(index % 5) * 72),
            inputs: ["Canvas context"],
            outputs: ["Widget output"]
        )
    }

    private func widget(
        _ kind: CanvasWidgetKind,
        title: String,
        subtitle: String,
        summary: String,
        command: String,
        state: CanvasWidgetState,
        position: CGPoint,
        inputs: [String],
        outputs: [String]
    ) -> CanvasWidget {
        CanvasWidget(
            id: UUID(),
            kind: kind,
            title: title,
            subtitle: subtitle,
            summary: summary,
            command: command,
            state: state,
            position: position,
            size: kind.defaultSize,
            inputs: inputs,
            outputs: outputs
        )
    }

    private func connection(_ from: UUID, _ to: UUID, _ label: String) -> CanvasConnection {
        CanvasConnection(id: UUID(), from: from, to: to, label: label)
    }

    private func defaultCommand(for kind: CanvasWidgetKind) -> String {
        switch kind {
        case .question: "input.message"
        case .constraints: "local widget"
        case .knowledge: "prism query --platform --semantic <query> --json"
        case .papers: "prism research <query> --json"
        case .materials: "ui.card materials"
        case .modelLimits: "GET /api/v1/agent/capabilities"
        case .discourse: "prism discourse run <spec> --json"
        case .workflow: "prism workflow run <name> --json"
        case .simulation: "prism run <image> --json"
        case .evidenceBundle: "prism publish <path> --json"
        case .billingGuardrail: "prism billing usage"
        case .fabricNode: "prism node status"
        }
    }
}
