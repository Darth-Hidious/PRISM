import SwiftUI

struct DetailView: View {
    @EnvironmentObject private var store: AppStore
    let section: AppSection

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                HeaderView(section: section)

                if section == .overview {
                    StatusGrid(items: store.statusItems)
                }

                FeatureListView(features: store.features(for: section))
            }
            .padding(24)
            .frame(maxWidth: 980, alignment: .leading)
        }
        .navigationTitle(section.title)
    }
}

private struct HeaderView: View {
    let section: AppSection

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(section.title, systemImage: section.systemImage)
                .font(.title)
                .fontWeight(.semibold)

            Text(headerText)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var headerText: String {
        switch section {
        case .overview:
            "Native macOS shell over the existing PRISM CLI and JSON-RPC backend protocol."
        case .agent:
            "The agent timeline is the primary Mac surface: streaming text, tool cards, approvals, and cost."
        case .knowledge:
            "Structured graph and semantic search controls for materials queries."
        case .research:
            "High-level materials research loops with depth, citations, and exportable artifacts."
        case .workflows:
            "YAML workflows become parameterized Mac forms rather than long command lines."
        case .models:
            "Hosted model catalog and chat-target selection without exposing provider secrets."
        case .compute:
            "Job and deployment controls with status-first, confirmation-aware actions."
        case .nodes:
            "Local node, mesh, and federation state for controlled distributed work."
        case .billing:
            "Usage and pricing visibility. Stripe top-up stays gated until server hardening is complete."
        case .settings:
            "Runtime paths, backend launch settings, login state, and chat configuration."
        }
    }
}

private struct StatusGrid: View {
    let items: [DemoStatusItem]

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 210), spacing: 12)], spacing: 12) {
            ForEach(items) { item in
                HStack(alignment: .top, spacing: 12) {
                    Image(systemName: item.systemImage)
                        .foregroundStyle(item.isHealthy ? .green : .orange)
                        .frame(width: 22)

                    VStack(alignment: .leading, spacing: 4) {
                        Text(item.title)
                            .font(.headline)
                        Text(item.value)
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
            }
        }
    }
}

