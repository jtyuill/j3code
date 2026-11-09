import SwiftUI

/// A tool call: compact summary, opened into its full detail.
struct ToolRow: View {
    let thread: ThreadModel
    let item: Item
    let name: String
    let intent: String?
    let inputJson: String
    let output: ToolOutput?
    let detail: ToolDetail?
    let router: FeedRouter

    var body: some View {
        Button {
            router.sheet = .tool(itemId: item.id)
        } label: {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: symbol)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .frame(width: 18, alignment: .center)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(title)
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(item.status == .failed ? Color.red : Color.primary)
                            .lineLimit(1)
                        if item.status == .running {
                            ProgressView()
                                .controlSize(.small)
                        }
                        Spacer(minLength: 0)
                    }
                    if let subtitle {
                        Text(subtitle)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    if let background = output?.background {
                        Text("Background job · \(background.state.label)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
            }
            .padding(10)
            .contentShape(RoundedRectangle(cornerRadius: 10))
            .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityHint("Shows the tool's details")
    }

    private var title: String {
        (intent?.isEmpty == false ? intent : nil) ?? name
    }

    private var symbol: String {
        switch detail {
        case .command?: "terminal"
        case .fileChange?: "doc.text"
        case .search?: "magnifyingglass"
        case .webSearch?: "globe"
        case .unknown?, nil: "wrench.and.screwdriver"
        }
    }

    private var subtitle: String? {
        switch detail {
        case let .command(command)?:
            command.isEmpty ? nil : command.replacingOccurrences(of: "\n", with: " ")
        case let .fileChange(path, _, additions, deletions, _)?:
            fileSubtitle(path: path, additions: additions, deletions: deletions)
        case let .search(pattern, _)?:
            pattern
        case let .webSearch(query, _)?:
            query
        case .unknown?, nil:
            nil
        }
    }

    private func fileSubtitle(path: String, additions: UInt64?, deletions: UInt64?) -> String? {
        var text = path
        if let additions, let deletions {
            text += "  +\(additions) −\(deletions)"
        } else if let additions {
            text += "  +\(additions)"
        } else if let deletions {
            text += "  −\(deletions)"
        }
        return text.isEmpty ? nil : text
    }
}

/// Everything a tool call carries: its arguments, structured detail, output
/// and images.
struct ToolDetailView: View {
    let thread: ThreadModel
    let item: Item
    @Environment(\.dismiss) private var dismiss
    @Environment(\.media) private var media

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    sections
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    @ViewBuilder
    private var sections: some View {
        if let parts {
            if let input = prettyJSON(parts.inputJson) {
                DetailSection("Input") { wrappingCode(input) }
            }
            if let command, !command.isEmpty {
                DetailSection("Command") { scrolledCode(command) }
            }
            if let diff, !diff.isEmpty {
                DetailSection("Diff") {
                    VStack(alignment: .leading, spacing: 4) {
                        if let path, !path.isEmpty {
                            Text(path)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                        diffView(diff)
                    }
                }
            }
            if let fileContent, !fileContent.isEmpty {
                DetailSection("File content") { scrolledCode(fileContent) }
            }
            if let matches, !matches.isEmpty {
                DetailSection("Matches") { matchList(matches) }
            }
            if let webResults, !webResults.isEmpty {
                DetailSection("Web results") { webResultList(webResults) }
            }
            if let output = parts.output {
                if !output.text.isEmpty || output.truncated || output.exitCode != nil {
                    DetailSection("Output") {
                        VStack(alignment: .leading, spacing: 6) {
                            if !output.text.isEmpty {
                                wrappingCode(output.text)
                            }
                            if output.truncated {
                                Text("Output truncated")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            if let exitCode = output.exitCode {
                                Text("Exit code \(exitCode)")
                                    .font(.caption.monospaced())
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                if !output.images.isEmpty {
                    DetailSection("Images") { imageList(output.images) }
                }
            }
        } else {
            ContentUnavailableView("Tool call unavailable", systemImage: "wrench.and.screwdriver")
        }
    }

    private var parts: (name: String, intent: String?, inputJson: String, output: ToolOutput?, detail: ToolDetail?)? {
        guard case let .tool(_, name, intent, inputJson, output, detail) = item.kind else { return nil }
        return (name, intent, inputJson, output, detail)
    }

    private var title: String {
        guard let parts else { return "Tool" }
        return (parts.intent?.isEmpty == false ? parts.intent : nil) ?? parts.name
    }

    private var command: String? {
        guard case let .command(command)? = parts?.detail else { return nil }
        return command
    }

    private var diff: String? {
        guard case let .fileChange(_, diff, _, _, _)? = parts?.detail else { return nil }
        return diff
    }

    private var path: String? {
        guard case let .fileChange(path, _, _, _, _)? = parts?.detail else { return nil }
        return path
    }

    private var fileContent: String? {
        guard case let .fileChange(_, _, _, _, content)? = parts?.detail else { return nil }
        return content
    }

    private var matches: [SearchMatch]? {
        guard case let .search(_, matches)? = parts?.detail else { return nil }
        return matches
    }

    private var webResults: [WebResult]? {
        guard case let .webSearch(_, results)? = parts?.detail else { return nil }
        return results
    }

    private func prettyJSON(_ text: String) -> String? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        guard let data = trimmed.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data),
              JSONSerialization.isValidJSONObject(object),
              let pretty = try? JSONSerialization.data(
                  withJSONObject: object, options: [.prettyPrinted, .sortedKeys]
              ),
              let string = String(data: pretty, encoding: .utf8)
        else { return trimmed }
        return string
    }

    private func wrappingCode(_ text: String) -> some View {
        Text(verbatim: text)
            .font(.caption.monospaced())
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }

    private func scrolledCode(_ text: String) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(verbatim: text)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .fixedSize(horizontal: true, vertical: false)
                .padding(8)
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }

    private func diffView(_ diff: String) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(
                    Array(diff.split(separator: "\n", omittingEmptySubsequences: false).enumerated()),
                    id: \.offset
                ) { _, line in
                    Text(verbatim: line.isEmpty ? " " : String(line))
                        .font(.caption.monospaced())
                        .foregroundStyle(diffColor(line))
                        .fixedSize(horizontal: true, vertical: false)
                }
            }
            .padding(8)
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }

    private func diffColor(_ line: Substring) -> Color {
        if line.hasPrefix("@@") { return .secondary }
        if line.hasPrefix("+") { return .green }
        if line.hasPrefix("-") { return .red }
        return .primary
    }

    private func matchList(_ matches: [SearchMatch]) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(matches.enumerated()), id: \.offset) { _, match in
                VStack(alignment: .leading, spacing: 1) {
                    Text(match.line.map { "\(match.path):\($0)" } ?? match.path)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                    if let text = match.text, !text.isEmpty {
                        Text(verbatim: text)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func webResultList(_ results: [WebResult]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(results.enumerated()), id: \.offset) { _, result in
                VStack(alignment: .leading, spacing: 2) {
                    if let url = URL(string: result.url) {
                        Link(result.title.isEmpty ? result.url : result.title, destination: url)
                            .font(.callout.weight(.medium))
                    } else {
                        Text(result.title)
                            .font(.callout.weight(.medium))
                    }
                    Text(result.url)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    if let snippet = result.snippet, !snippet.isEmpty {
                        Text(snippet)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func imageList(_ images: [ToolImage]) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(Array(images.enumerated()), id: \.offset) { _, image in
                    imageThumbnail(image)
                }
            }
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
    }

    private func imageThumbnail(_ image: ToolImage) -> some View {
        let url = URL(string: thread.host.attachmentUrl(attachmentId: image.attachmentId))
        return Button {
            if let url { media?.showImage(url, name: "Tool image") }
        } label: {
            RemoteImage(url: url, contentMode: .fill)
                .frame(width: 96, height: 96)
                .clipShape(RoundedRectangle(cornerRadius: 10))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Tool image")
    }
}

private struct DetailSection<Content: View>: View {
    let title: String
    let content: Content

    init(_ title: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private extension JobState {
    var label: String {
        switch self {
        case .running: "Running"
        case .completed: "Completed"
        case .failed: "Failed"
        case .cancelled: "Cancelled"
        }
    }
}
