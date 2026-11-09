import Foundation

/// An atomic token in the draft, drawn as a pill: a leading `/command` or
/// `/skill:name`, a `^provider/id` model mention or an `@path` file
/// reference.
struct Chip: Hashable {
    enum Kind: String, Hashable {
        case command
        case model
        case file
    }

    var kind: Kind
    /// What omp reads: `/compact`, `/skill:jev`, `^anthropic/claude-x`,
    /// `@src/main.rs` or `@"my notes.md"`.
    var token: String
    /// What the pill shows: the command, the model's name, the file name.
    var label: String
}

/// One run of the draft: typed text or a chip.
enum DraftPart: Hashable {
    case text(String)
    case chip(Chip)
}

enum Draft {
    /// The draft as omp reads it: each chip as its token followed by the text
    /// after it.
    static func serialize(_ parts: [DraftPart]) -> String {
        parts.map { part in
            switch part {
            case let .text(text): text
            case let .chip(chip): chip.token
            }
        }.joined()
    }

    /// True when the draft has neither a chip nor non-whitespace text.
    static func isBlank(_ parts: [DraftPart]) -> Bool {
        parts.allSatisfy { part in
            if case let .text(text) = part {
                text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            } else {
                false
            }
        }
    }

    /// `parts` with adjacent text runs joined and empty ones dropped.
    static func normalized(_ parts: [DraftPart]) -> [DraftPart] {
        var result: [DraftPart] = []
        for part in parts {
            switch part {
            case let .text(text):
                guard !text.isEmpty else { continue }
                if case let .text(previous) = result.last {
                    result[result.count - 1] = .text(previous + text)
                } else {
                    result.append(part)
                }
            case .chip:
                result.append(part)
            }
        }
        return result
    }
}
