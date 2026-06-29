import SwiftUI

struct FeatureListView: View {
    let features: [FeatureItem]

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(features) { feature in
                FeatureRow(feature: feature)
            }
        }
    }
}

private struct FeatureRow: View {
    let feature: FeatureItem

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                Text(feature.title)
                    .font(.headline)

                Spacer()

                Text(feature.priority.rawValue)
                    .font(.caption)
                    .fontWeight(.semibold)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(priorityTint.opacity(0.16), in: Capsule())
                    .foregroundStyle(priorityTint)
            }

            Text(feature.summary)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(alignment: .top, spacing: 18) {
                MetadataColumn(title: "Mac surface", value: feature.macSurface)
                MetadataColumn(title: "CLI contract", value: feature.command)
            }

            Text(feature.note)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.background, in: RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(.quaternary)
        )
    }

    private var priorityTint: Color {
        switch feature.priority {
        case .mvp: .blue
        case .next: .purple
        case .advanced: .secondary
        case .gated: .orange
        }
    }
}

private struct MetadataColumn: View {
    let title: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.system(.callout, design: .monospaced))
                .textSelection(.enabled)
                .lineLimit(3)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

