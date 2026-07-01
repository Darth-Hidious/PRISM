import SwiftUI

struct CanvasWidgetView: View {
    let widget: CanvasWidget
    let isSelected: Bool

    var body: some View {
        ZStack(alignment: .leading) {
            RoundedRectangle(cornerRadius: 8)
                .fill(widget.kind.tint.opacity(isSelected ? 0.18 : 0.11))
                .frame(width: 4)
                .padding(.vertical, 10)

            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top, spacing: 9) {
                    ZStack {
                        RoundedRectangle(cornerRadius: 6)
                            .fill(widget.kind.tint.opacity(0.12))
                        Image(systemName: widget.kind.systemImage)
                            .foregroundStyle(widget.kind.tint)
                    }
                    .frame(width: 30, height: 30)

                    VStack(alignment: .leading, spacing: 2) {
                        Text(widget.title)
                            .font(.headline)
                            .lineLimit(1)

                        Text(widget.subtitle)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }

                    Spacer(minLength: 8)

                    StatePill(state: widget.state)
                }

                Text(widget.summary)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)

                Spacer(minLength: 0)

                HStack(spacing: 8) {
                    MetricPill(title: "In", value: "\(widget.inputs.count)")
                    MetricPill(title: "Out", value: "\(widget.outputs.count)")
                    Spacer()
                    Image(systemName: "arrow.up.and.down.and.arrow.left.and.right")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(13)
            .padding(.leading, 3)
        }
        .prismGlassSurface(cornerRadius: PRISMDesign.panelRadius, interactive: true)
        .overlay {
            RoundedRectangle(cornerRadius: PRISMDesign.panelRadius)
                .stroke(borderStyle, lineWidth: isSelected ? 2 : 1)
        }
        .overlay(alignment: .leading) {
            PortStack(count: widget.inputs.count, tint: widget.kind.tint)
                .offset(x: -5)
        }
        .overlay(alignment: .trailing) {
            PortStack(count: widget.outputs.count, tint: widget.kind.tint)
                .offset(x: 5)
        }
        .shadow(color: .black.opacity(isSelected ? 0.16 : 0.08), radius: isSelected ? 14 : 8, y: 4)
    }

    private var borderStyle: AnyShapeStyle {
        isSelected ? AnyShapeStyle(widget.kind.tint) : AnyShapeStyle(.quaternary)
    }
}

private struct PortStack: View {
    let count: Int
    let tint: Color

    private var visibleCount: Int {
        max(1, min(count, 4))
    }

    var body: some View {
        VStack(spacing: 8) {
            ForEach(0..<visibleCount, id: \.self) { _ in
                Circle()
                    .fill(.background)
                    .frame(width: 10, height: 10)
                    .overlay {
                        Circle()
                            .stroke(tint.opacity(0.75), lineWidth: 2)
                    }
                    .shadow(color: .black.opacity(0.08), radius: 2, y: 1)
            }
        }
        .padding(.vertical, 9)
    }
}

private struct StatePill: View {
    let state: CanvasWidgetState

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: state.systemImage)
            Text(state.rawValue)
        }
        .font(.caption2)
        .fontWeight(.medium)
        .foregroundStyle(state.tint)
        .padding(.horizontal, 7)
        .padding(.vertical, 4)
        .background(state.tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 5))
    }
}

private struct MetricPill: View {
    let title: String
    let value: String

    var body: some View {
        HStack(spacing: 4) {
            Text(title)
                .foregroundStyle(.secondary)
            Text(value)
                .fontWeight(.semibold)
        }
        .font(.caption)
        .padding(.horizontal, 7)
        .padding(.vertical, 4)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 5))
    }
}
