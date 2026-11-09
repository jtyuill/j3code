import Foundation

/// The user's choices for one open request, turned into `requests.answer`
/// answers (wire.md §5.6): `select` and `confirm` send the chosen option
/// label, `input` and `editor` the one text, `ask` the chosen labels and the
/// typed "other" answer.
struct RequestDraft: Equatable {
    struct Choice: Equatable {
        var selected: [String] = []
        var other = ""
    }

    let request: Request
    /// By question id.
    var choices: [String: Choice]

    init(_ request: Request) {
        self.request = request
        var choices: [String: Choice] = [:]
        for question in request.questions {
            var choice = Choice()
            if let value = question.default {
                if question.options.isEmpty {
                    choice.other = value
                } else if question.options.contains(where: { $0.label == value }) {
                    choice.selected = [value]
                }
            }
            choices[question.id] = choice
        }
        self.choices = choices
    }

    /// The options a question offers; `confirm` always offers Yes and No.
    static func options(of question: Question, kind: RequestKind) -> [QuestionOption] {
        if kind == .confirm, question.options.isEmpty {
            return [QuestionOption(label: "Yes", description: nil), QuestionOption(label: "No", description: nil)]
        }
        return question.options
    }

    /// Whether the question takes free text (`input`, `editor`, or an `ask`
    /// question with no options or with "other" allowed).
    static func takesText(_ question: Question, kind: RequestKind) -> Bool {
        switch kind {
        case .input, .editor: true
        case .ask: question.options.isEmpty || question.allowOther
        case .select: question.allowOther
        case .confirm: false
        }
    }

    func choice(_ question: Question) -> Choice {
        choices[question.id] ?? Choice()
    }

    /// Picks `label`; a single-choice question keeps only it, a multiple one
    /// toggles it.
    mutating func toggle(_ label: String, in question: Question) {
        var choice = self.choice(question)
        if question.multiple {
            if let index = choice.selected.firstIndex(of: label) {
                choice.selected.remove(at: index)
            } else {
                choice.selected.append(label)
            }
        } else {
            choice.selected = choice.selected == [label] ? [] : [label]
            if !choice.selected.isEmpty, request.kind != .ask { choice.other = "" }
        }
        choices[question.id] = choice
    }

    mutating func setText(_ text: String, for question: Question) {
        var choice = self.choice(question)
        choice.other = text
        if !question.multiple, !text.isEmpty, request.kind == .select { choice.selected = [] }
        choices[question.id] = choice
    }

    /// Every question has an answer.
    var isComplete: Bool {
        request.questions.allSatisfy { question in
            let choice = self.choice(question)
            switch request.kind {
            case .input, .editor: return true
            case .confirm, .select, .ask:
                return !choice.selected.isEmpty
                    || !choice.other.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            }
        }
    }

    var answers: [Answer] {
        request.questions.map { question in
            let choice = self.choice(question)
            let other = choice.other.trimmingCharacters(in: .whitespacesAndNewlines)
            switch request.kind {
            case .input, .editor:
                return Answer(questionId: question.id, values: [choice.other], other: nil)
            case .confirm:
                return Answer(questionId: question.id, values: choice.selected, other: nil)
            case .select:
                let values = other.isEmpty ? choice.selected : [other]
                return Answer(questionId: question.id, values: values, other: nil)
            case .ask:
                return Answer(questionId: question.id, values: choice.selected, other: other.isEmpty ? nil : other)
            }
        }
    }

    /// One tap answers a single-choice `select` or `confirm` question.
    static func answersOnTap(_ request: Request) -> Bool {
        (request.kind == .select || request.kind == .confirm)
            && request.questions.count == 1
            && request.questions.first.map { !$0.multiple && !$0.allowOther } == true
    }
}
