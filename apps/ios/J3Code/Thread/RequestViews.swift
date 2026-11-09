import SwiftUI

/// An open request (`ask`, `select`, `confirm`, `input`, `editor`) above the
/// composer.
struct RequestCard: View {
    let request: Request
    let model: ThreadModel
    @State private var draft: RequestDraft
    @State private var editing = false

    init(request: Request, model: ThreadModel) {
        self.request = request
        self.model = model
        _draft = State(initialValue: RequestDraft(request))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(request.title ?? kindTitle, systemImage: "questionmark.bubble.fill")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.orange)
                Spacer()
                Button("Dismiss") { model.dismiss(request) }
                    .font(.caption)
                    .accessibilityIdentifier("dismiss-request")
            }
            ForEach(request.questions, id: \.id) { question in
                questionView(question)
            }
            if !RequestDraft.answersOnTap(request) {
                Button {
                    model.answer(request, draft.answers)
                } label: {
                    Text("Answer").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .disabled(!draft.isComplete || model.isBusy)
                .accessibilityIdentifier("submit-answer")
            }
        }
        .padding(12)
        .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(.orange.opacity(0.35)))
    }

    private var kindTitle: String {
        switch request.kind {
        case .ask: "omp asks"
        case .select: "Choose"
        case .confirm: "Confirm"
        case .input: "Input"
        case .editor: "Edit"
        }
    }

    @ViewBuilder
    private func questionView(_ question: Question) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if let header = question.header {
                Text(header).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
            }
            if !question.text.isEmpty, question.text != request.title {
                Text(question.text).font(.callout)
            }
            let options = RequestDraft.options(of: question, kind: request.kind)
            if !options.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(options, id: \.label) { option in
                        optionButton(option, question: question)
                    }
                }
            }
            if RequestDraft.takesText(question, kind: request.kind) {
                textField(question)
            }
        }
    }

    private func optionButton(_ option: QuestionOption, question: Question) -> some View {
        let selected = draft.choice(question).selected.contains(option.label)
        return Button {
            if RequestDraft.answersOnTap(request) {
                model.answer(request, [Answer(questionId: question.id, values: [option.label], other: nil)])
            } else {
                draft.toggle(option.label, in: question)
            }
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: question.multiple
                    ? (selected ? "checkmark.square.fill" : "square")
                    : (selected ? "largecircle.fill.circle" : "circle"))
                    .foregroundStyle(selected ? Color.accentColor : .secondary)
                VStack(alignment: .leading, spacing: 1) {
                    Text(option.label).foregroundStyle(.primary)
                    if let description = option.description {
                        Text(description).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
            .padding(.vertical, 4)
        }
        .buttonStyle(.plain)
        .disabled(model.isBusy)
        .accessibilityIdentifier("option-\(option.label)")
    }

    @ViewBuilder
    private func textField(_ question: Question) -> some View {
        let binding = Binding(
            get: { draft.choice(question).other },
            set: { draft.setText($0, for: question) }
        )
        let prompt = request.kind == .ask || request.kind == .select ? "Other answer" : "Answer"
        if request.kind == .editor {
            TextEditor(text: binding)
                .font(.body.monospaced())
                .frame(minHeight: 120, maxHeight: 240)
                .scrollContentBackground(.hidden)
                .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
        } else {
            TextField(prompt, text: binding, axis: .vertical)
                .lineLimit(1...6)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("answer-text")
        }
    }
}

/// The thread's queued prompts: edit, cancel, steer now, start now.
struct QueueView: View {
    let model: ThreadModel

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Queued").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                if model.queue.contains(where: \.held) {
                    Text("· held").font(.caption).foregroundStyle(.orange)
                    Spacer()
                    Button("Resume") { model.resumeQueue() }.font(.caption)
                }
            }
            ForEach(model.queue) { entry in
                HStack(alignment: .top, spacing: 8) {
                    Text("\(entry.position + 1).")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.text).font(.callout).lineLimit(3)
                        if !entry.attachments.isEmpty {
                            Label("\(entry.attachments.count)", systemImage: "paperclip")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                    Spacer(minLength: 0)
                    Menu {
                        Button { model.edit(entry) } label: { Label("Edit", systemImage: "pencil") }
                        if model.isActive {
                            Button { model.steer(entry) } label: {
                                Label("Steer Now", systemImage: "arrow.turn.down.right")
                            }
                        }
                        Button { model.startNow(entry) } label: {
                            Label(model.isActive ? "Stop and Start" : "Start Now", systemImage: "play")
                        }
                        Button(role: .destructive) { model.cancel(entry) } label: {
                            Label("Cancel", systemImage: "xmark")
                        }
                    } label: {
                        Image(systemName: "ellipsis.circle")
                    }
                    .accessibilityLabel("Queued prompt actions")
                }
                .padding(8)
                .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
            }
        }
    }
}
