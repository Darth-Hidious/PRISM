import SwiftUI

struct SidebarView: View {
    @Binding var selection: AppSection?

    var body: some View {
        List(selection: $selection) {
            Section("PRISM") {
                ForEach(AppSection.allCases) { section in
                    SidebarRow(section: section)
                        .tag(section)
                }
            }
        }
        .listStyle(.sidebar)
        .navigationTitle("PRISM")
    }
}

private struct SidebarRow: View {
    let section: AppSection

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: section.systemImage)
                .foregroundStyle(.secondary)
                .frame(width: 16)

            VStack(alignment: .leading, spacing: 2) {
                Text(section.title)
                    .lineLimit(1)

                Text(section.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
    }
}

