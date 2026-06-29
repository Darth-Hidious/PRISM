import SwiftUI

struct ChatSurfaceView: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        ZStack {
            ChatBackdrop()

            VStack(spacing: 0) {
                ChatHeader()
                    .padding(.horizontal, 24)
                    .padding(.top, 18)

                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        Spacer(minLength: 18)

                        ForEach(store.chatMessages) { message in
                            ChatMessageRow(message: message)
                        }
                    }
                    .frame(maxWidth: PRISMDesign.mainColumnWidth, alignment: .leading)
                    .padding(.horizontal, 24)
                    .padding(.bottom, 18)
                    .frame(maxWidth: .infinity)
                }

                ChatComposer()
                    .frame(maxWidth: PRISMDesign.mainColumnWidth)
                    .padding(.horizontal, 24)
                    .padding(.bottom, 20)
            }
        }
    }
}

private struct ChatBackdrop: View {
    var body: some View {
        Color(nsColor: .windowBackgroundColor)
        .ignoresSafeArea()
    }
}

private struct ChatHeader: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text("PRISM")
                    .font(.title2)
                    .fontWeight(.semibold)
                Text("Materials research command surface")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            Button {
                store.switchToWorkflowBoard()
            } label: {
                Label("Board", systemImage: "point.3.connected.trianglepath.dotted")
            }
            .buttonStyle(.bordered)
        }
        .frame(maxWidth: PRISMDesign.mainColumnWidth)
        .frame(maxWidth: .infinity)
    }
}

private struct ChatMessageRow: View {
    let message: ChatMessage

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            MessageAvatar(role: message.role)

            VStack(alignment: .leading, spacing: 8) {
                Text(message.title)
                    .font(.callout)
                    .fontWeight(.semibold)

                Text(message.body)
                    .font(.body)
                    .foregroundStyle(.primary)
                    .fixedSize(horizontal: false, vertical: true)

                if !message.attachments.isEmpty {
                    ViewThatFits(in: .horizontal) {
                        HStack(spacing: 7) {
                            attachmentChips
                        }

                        VStack(alignment: .leading, spacing: 6) {
                            attachmentChips
                        }
                    }
                }
            }

            Spacer(minLength: 0)
        }
        .padding(.vertical, 4)
    }

    @ViewBuilder
    private var attachmentChips: some View {
        ForEach(message.attachments, id: \.self) { attachment in
            Text(attachment)
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 8)
                .padding(.vertical, 5)
                .background(.quaternary.opacity(0.28), in: RoundedRectangle(cornerRadius: PRISMDesign.controlRadius))
        }
    }
}

private struct MessageAvatar: View {
    let role: ChatMessageRole

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: PRISMDesign.panelRadius)
                .fill(role == .user ? Color.accentColor.opacity(0.12) : Color.secondary.opacity(0.12))

            Image(systemName: role == .user ? "person.fill" : "sparkles")
                .foregroundStyle(role == .user ? Color.accentColor : Color.secondary)
        }
        .frame(width: 32, height: 32)
    }
}

private struct ChatComposer: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ContextBanner()

            HStack(alignment: .bottom, spacing: 10) {
                Menu {
                    Button("Attach file", systemImage: "paperclip") {}
                    Button("Use screenshot", systemImage: "camera.viewfinder") {}
                    Button("Use selected workflow widgets", systemImage: "square.stack.3d.up") {
                        store.switchToWorkflowBoard()
                    }
                } label: {
                    Image(systemName: "plus")
                        .frame(width: 28, height: 28)
                }
                .menuStyle(.borderlessButton)
                .help("Add context")

                TextField("Message PRISM", text: $store.chatPrompt, axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...5)
                    .onSubmit {
                        store.sendChatPrompt()
                    }

                Button {
                    store.sendChatPrompt()
                } label: {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .buttonStyle(.plain)
                .disabled(store.chatPrompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(12)
        .prismGlassSurface(cornerRadius: PRISMDesign.floatingControlRadius, interactive: true)
        .shadow(color: .black.opacity(0.1), radius: 18, y: 8)
    }
}

private struct ContextBanner: View {
    @EnvironmentObject private var store: AppStore

    var body: some View {
        HStack(spacing: 7) {
            Image(systemName: "app.connected.to.app.below.fill")
                .foregroundStyle(.secondary)

            Text("Working with")
                .font(.caption)
                .foregroundStyle(.secondary)

            ForEach(store.contextChips.prefix(3)) { chip in
                Label(chip.title, systemImage: chip.systemImage)
                    .font(.caption)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 4)
                    .background(.quaternary.opacity(0.25), in: RoundedRectangle(cornerRadius: PRISMDesign.controlRadius))
            }

            Spacer()

            Button {
                store.toggleInspector()
            } label: {
                Text("Manage")
                    .font(.caption)
            }
            .buttonStyle(.plain)
        }
    }
}
