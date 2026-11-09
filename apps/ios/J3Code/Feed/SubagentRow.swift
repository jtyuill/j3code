import SwiftUI

/// A subagent task: its prompt, live progress, result and transcript.
struct SubagentRow: View {
    let thread: ThreadModel
    let item: Item
    let subagentId: String
    let agent: String
    let prompt: String
    let model: String?
    let progress: String?
    let result: String?
    let router: FeedRouter
    @State private var expandedPrompt = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "person.2")
                    .foregroundStyle(.secondary)
                Text(agent)
                    .font(.subheadline.weight(.semibold))
                Spacer(minLength: 8)
                if let model, !model.isEmpty {
                    Text(model)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            if !prompt.isEmpty {
                Text(prompt)
                    .font(.callout)
                    .lineLimit(expandedPrompt ? nil : 3)
                    .textSelection(.enabled)
                if isLongPrompt {
                    Button(expandedPrompt ? "Less" : "More") {
                        withAnimation(.snappy) { expandedPrompt.toggle() }
                    }
                    .font(.caption)
                    .buttonStyle(.plain)
                    .foregroundStyle(Color.accentColor)
                }
            }
            if item.status == .running, let progress, !progress.isEmpty {
                Text(progress)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            if let result, !result.isEmpty {
                MarkdownView(text: result, host: thread.host)
            }
            Button {
                router.sheet = .transcript(
                    threadId: thread.threadId, subagentId: subagentId, agent: agent
                )
            } label: {
                Label("Transcript", systemImage: "text.bubble")
                    .font(.caption)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .contain)
    }

    /// Wrapped prompts and prompts with more than three lines get a toggle.
    private var isLongPrompt: Bool {
        prompt.split(separator: "\n", omittingEmptySubsequences: false).count > 3
            || prompt.count > 200
    }
}

/// A subagent's own transcript: user prompts, assistant output, thinking and
/// tool calls, fetched page by page from the server.
struct TranscriptView: View {
    let thread: ThreadModel
    let subagentId: String
    let agent: String
    @Environment(\.dismiss) private var dismiss
    @State private var entries: [TranscriptEntry] = []
    @State private var nextByte: UInt64 = 0
    @State private var error: String?
    @State private var isLoading = true

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach(Array(entries.enumerated()), id: \.offset) { _, entry in
                        TranscriptEntryRow(entry: entry, host: thread.host)
                    }
                    if let error {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(Color.red)
                            .textSelection(.enabled)
                    }
                    if isLoading && entries.isEmpty {
                        HStack {
                            Spacer()
                            ProgressView()
                            Spacer()
                        }
                        .padding(.vertical, 24)
                    } else if entries.isEmpty, error == nil {
                        Text("No transcript yet.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .refreshable { await fetch(from: nextByte) }
            .navigationTitle(agent)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .task { await load() }
        }
    }

    private func load() async {
        guard entries.isEmpty else { return }
        isLoading = true
        await fetch(from: 0)
        isLoading = false
    }

    /// Reads pages from `start` until the server says there are no more.
    private func fetch(from start: UInt64) async {
        var from = start
        do {
            while true {
                let page = try await thread.host.subagentTranscript(
                    threadId: thread.threadId, subagentId: subagentId, fromByte: from
                )
                if page.reset {
                    entries.removeAll()
                }
                entries.append(contentsOf: page.entries)
                nextByte = page.nextByte
                error = nil
                guard page.more, page.nextByte != from else { break }
                from = page.nextByte
            }
        } catch {
            self.error = describe(error)
        }
    }
}

private struct TranscriptEntryRow: View {
    let entry: TranscriptEntry
    let host: Host

    var body: some View {
        switch entry {
        case let .user(text):
            HStack(alignment: .top) {
                Spacer(minLength: 32)
                Text(text)
                    .font(.callout)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(
                        Color.accentColor.opacity(0.14),
                        in: RoundedRectangle(cornerRadius: 14)
                    )
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(Text("You: \(text)"))
        case let .assistant(text):
            MarkdownView(text: text, host: host)
        case let .thinking(text):
            MarkdownView(text: text, host: host, secondary: true)
        case let .toolCall(name, intent, _):
            toolCall(name: name, intent: intent)
        case let .toolResult(name, text, failed):
            ToolResultEntry(name: name, text: text, failed: failed)
        case let .error(text):
            Text(text)
                .font(.footnote)
                .foregroundStyle(Color.red)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityElement(children: .combine)
        }
    }

    private func toolCall(name: String, intent: String?) -> some View {
        let title = (intent?.isEmpty == false ? intent : nil) ?? name
        return HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "wrench.and.screwdriver")
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.caption.weight(.medium))
                if intent?.isEmpty == false {
                    Text(name)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .accessibilityElement(children: .combine)
    }
}

private struct ToolResultEntry: View {
    let name: String
    let text: String
    let failed: Bool
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: "wrench.and.screwdriver")
                    .foregroundStyle(.secondary)
                Text(name)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Text(verbatim: text)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .lineLimit(expanded ? nil : 12)
                .foregroundStyle(failed ? Color.red : Color.primary)
            if lineCount > 12 {
                Button(expanded ? "Less" : "More") {
                    withAnimation(.snappy) { expanded.toggle() }
                }
                .font(.caption)
                .buttonStyle(.plain)
                .foregroundStyle(Color.accentColor)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
    }

    private var lineCount: Int {
        text.isEmpty ? 0 : text.split(separator: "\n", omittingEmptySubsequences: false).count
    }
}
