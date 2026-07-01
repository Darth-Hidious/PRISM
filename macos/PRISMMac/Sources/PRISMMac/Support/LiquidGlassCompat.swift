import SwiftUI

extension View {
    @ViewBuilder
    func prismGlassSurface(cornerRadius: CGFloat, interactive: Bool = false) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)

        if #available(macOS 26.0, *) {
            self.glassEffect(.regular.interactive(interactive), in: shape)
        } else {
            self.background(.regularMaterial, in: shape)
        }
    }
}
