import SwiftUI

/// One feed row, chosen by the item's kind.
struct ItemRow: View {
    let thread: ThreadModel
    let item: Item
    let router: FeedRouter

    var body: some View {
        switch item.kind {
        case let .user(text, attachments, delivery):
            UserMessageRow(
                text: text, attachments: attachments, delivery: delivery,
                host: thread.host, models: thread.connection.catalog?.models ?? []
            )
        case let .text(text):
            MarkdownView(text: text, host: thread.host)
        case let .thinking(text):
            ThinkingRow(text: text, expanded: thread.expandThinking, host: thread.host)
        case let .tool(_, name, intent, inputJson, output, detail):
            ToolRow(
                thread: thread, item: item, name: name, intent: intent,
                inputJson: inputJson, output: output, detail: detail, router: router
            )
        case let .todo(phases):
            TodoRow(phases: phases)
        case let .subagent(subagentId, agent, _, prompt, model, progress, result):
            SubagentRow(
                thread: thread, item: item, subagentId: subagentId, agent: agent,
                prompt: prompt, model: model, progress: progress, result: result,
                router: router
            )
        case let .goal(goal):
            GoalRow(goal: goal)
        case let .compaction(summary, tokensBefore):
            CompactionRow(summary: summary, tokensBefore: tokensBefore, host: thread.host)
        case let .notice(level, text):
            NoticeRow(level: level, text: text)
        case .stop:
            StopRow()
        case .unknown:
            EmptyView()
        }
    }
}

/// A reasoning block, collapsed or shown as secondary markdown.
private struct ThinkingRow: View {
    let text: String
    let host: Host
    @State private var expanded: Bool

    init(text: String, expanded: Bool, host: Host) {
        self.text = text
        self.host = host
        _expanded = State(initialValue: expanded)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(.snappy) { expanded.toggle() }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "brain")
                    Text("Thinking")
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.down")
                        .font(.caption2)
                        .rotationEffect(.degrees(expanded ? 0 : -90))
                }
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Thinking")
            .accessibilityValue(expanded ? "Expanded" : "Collapsed")

            if expanded {
                MarkdownView(text: text, host: host, secondary: true)
                    .padding(.leading, 2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
    }
}

/// omp's todo list: phases of tasks with their progress.
private struct TodoRow: View {
    let phases: [TodoPhase]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(phases.enumerated()), id: \.offset) { _, phase in
                VStack(alignment: .leading, spacing: 4) {
                    if !phase.name.isEmpty {
                        Text(phase.name)
                            .font(.caption.weight(.medium))
                            .foregroundStyle(.secondary)
                    }
                    ForEach(Array(phase.tasks.enumerated()), id: \.offset) { _, task in
                        taskRow(task)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }

    private func taskRow(_ task: TodoTask) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: icon(task.status))
                .font(.callout)
                .foregroundStyle(tint(task.status))
            Text(task.text)
                .font(.callout)
                .foregroundStyle(task.status == .abandoned ? Color.secondary : Color.primary)
                .strikethrough(task.status == .completed || task.status == .abandoned)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(task.text), \(label(task.status))")
    }

    private func icon(_ status: TodoStatus) -> String {
        switch status {
        case .pending: "circle"
        case .inProgress: "circle.dotted"
        case .completed: "checkmark.circle.fill"
        case .abandoned: "xmark.circle"
        case .blocked: "exclamationmark.circle"
        }
    }

    private func tint(_ status: TodoStatus) -> Color {
        switch status {
        case .inProgress: .accentColor
        case .completed: .green
        case .blocked: .orange
        case .pending, .abandoned: .secondary
        }
    }

    private func label(_ status: TodoStatus) -> String {
        switch status {
        case .pending: "Pending"
        case .inProgress: "In progress"
        case .completed: "Completed"
        case .abandoned: "Abandoned"
        case .blocked: "Blocked"
        }
    }
}

/// An omp goal: objective, state and token budget.
private struct GoalRow: View {
    let goal: Goal

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "target")
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 3) {
                Text(goal.objective)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 10) {
                    Text(stateLabel(goal.state))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if let usage {
                        Text(usage)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .monospacedDigit()
                    }
                }
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .combine)
    }

    private var usage: String? {
        switch (goal.tokensUsed, goal.budget) {
        case let (used?, budget?):
            "\(FeedFormat.tokens(used)) / \(FeedFormat.tokens(budget)) tokens"
        case let (used?, nil):
            "\(FeedFormat.tokens(used)) tokens"
        case let (nil, budget?):
            "Budget \(FeedFormat.tokens(budget)) tokens"
        case (nil, nil):
            nil
        }
    }

    private func stateLabel(_ state: GoalState) -> String {
        switch state {
        case .active: "Active"
        case .paused: "Paused"
        case .budgetLimited: "Budget limited"
        case .complete: "Complete"
        case .dropped: "Dropped"
        }
    }
}

/// A context compaction, with its summary when the server kept one.
private struct CompactionRow: View {
    let summary: String?
    let tokensBefore: UInt64?
    let host: Host
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                line
                Text(title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize()
                line
            }
            if let summary, !summary.isEmpty {
                DisclosureGroup(isExpanded: $expanded) {
                    MarkdownView(text: summary, host: host, secondary: true)
                        .padding(.top, 4)
                } label: {
                    Text("Summary")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var title: String {
        if let tokensBefore {
            "Context compacted · \(FeedFormat.tokens(tokensBefore)) tokens before"
        } else {
            "Context compacted"
        }
    }

    private var line: some View {
        Rectangle()
            .fill(Color(.separator))
            .frame(height: 1)
    }
}

/// A server notice: informational, a warning, or an error.
private struct NoticeRow: View {
    let level: NoticeLevel
    let text: String

    var body: some View {
        Label(text, systemImage: symbol)
            .font(.footnote)
            .foregroundStyle(color)
            .lineLimit(1)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
    }

    private var symbol: String {
        switch level {
        case .info: "info.circle"
        case .warning: "exclamationmark.triangle"
        case .error: "xmark.octagon"
        }
    }

    private var color: Color {
        switch level {
        case .info: .secondary
        case .warning: .orange
        case .error: .red
        }
    }
}

/// The turn was stopped by the user.
private struct StopRow: View {
    var body: some View {
        Text("Stopped by you")
            .font(.caption)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .center)
            .accessibilityElement(children: .combine)
    }
}

/// A turn's failure, shown after its last item.
struct TurnErrorCard: View {
    let error: TurnError

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(error.kind.label, systemImage: "exclamationmark.triangle.fill")
                .font(.caption.weight(.semibold))
            Text(error.message)
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
        }
        .foregroundStyle(Color.red)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(Color.red.opacity(0.12), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .combine)
    }
}

private extension TurnErrorKind {
    var label: String {
        switch self {
        case .usageLimit: "Usage limit"
        case .provider: "Provider error"
        case .omp: "omp error"
        case .crashed: "Crashed"
        case .refused: "Refused"
        }
    }
}
