import Foundation

/// What is being completed at the caret. Offsets are UTF-16, as UITextView
/// counts them; a chip is one U+FFFC character.
enum CompletionQuery: Hashable {
    /// `/query` at the very start of the message.
    case slash(String)
    /// A `^query` model mention whose `^` is at `start`.
    case model(start: Int, query: String)
    /// An `@query` file reference (`@"query` when `quoted`) whose `@` is at
    /// `start`.
    case path(start: Int, query: String, quoted: Bool)

    var start: Int {
        switch self {
        case .slash: 0
        case let .model(start, _), let .path(start, _, _): start
        }
    }
}

/// One row of the completion menu.
struct Completion: Identifiable, Hashable {
    enum Kind: Hashable {
        case command
        case skill
        /// A model, by slug (`provider/id`).
        case model(slug: String)
        /// A file or directory, by its path in the project.
        case path(String, directory: Bool)
    }

    var kind: Kind
    /// The row's title; commands and skills insert it as their chip.
    var label: String
    /// The line under the title: a description, a model's slug, a path.
    var detail: String?
    /// A command's argument hint, shown after the label.
    var hint: String?

    var id: String {
        switch kind {
        case .command, .skill: label
        case let .model(slug): "^" + slug
        case let .path(path, _): "@" + path
        }
    }
}

/// A completion's change to the draft: `parts` replace `range` (UTF-16).
struct CompletionEdit: Equatable {
    var range: NSRange
    var parts: [DraftPart]
}

/// Completion for the composer, as omp's editor and the desktop client do
/// it: slash commands and skills, `^` model mentions and `@` file
/// references.
enum ComposerCompletion {
    /// The character a chip occupies in the text view.
    static let chipCharacter: Character = "\u{FFFC}"

    /// The query ending at `cursor` in `text`: a slash command first, then
    /// a model mention, then a file reference.
    static func query(in text: String, cursor: Int) -> CompletionQuery? {
        let string = text as NSString
        guard cursor >= 0, cursor <= string.length else { return nil }
        let prefix = string.substring(to: cursor)
        if let query = slashQuery(prefix) { return .slash(query) }
        guard mentionsAllowed(text) else { return nil }
        if let (start, query) = modelQuery(prefix) { return .model(start: start, query: query) }
        if let (start, query, quoted) = pathQuery(prefix) {
            return .path(start: start, query: query, quoted: quoted)
        }
        return nil
    }

    /// `prefix` (the text before the caret) is `/` and non-whitespace.
    static func slashQuery(_ prefix: String) -> String? {
        guard prefix.hasPrefix("/") else { return nil }
        let query = prefix.dropFirst()
        guard !query.contains(where: { $0.isWhitespace || $0 == chipCharacter }) else { return nil }
        return String(query)
    }

    /// omp never expands mentions or references in local-execution drafts
    /// (those starting with `!` or `$`).
    static func mentionsAllowed(_ text: String) -> Bool {
        guard let first = text.first(where: { !$0.isWhitespace }) else { return true }
        return first != "!" && first != "$"
    }

    /// The `^query` token ending the prefix: it starts the text or follows
    /// whitespace.
    static func modelQuery(_ prefix: String) -> (start: Int, query: String)? {
        let start = prefix.lastIndex(where: \.isWhitespace).map(prefix.index(after:)) ?? prefix.startIndex
        let token = prefix[start...]
        guard token.hasPrefix("^"), !token.contains(chipCharacter) else { return nil }
        return (offset(of: start, in: prefix), String(token.dropFirst()))
    }

    /// The characters that end a path token, as omp's editor splits them.
    static func isPathDelimiter(_ character: Character) -> Bool {
        character == " " || character == "\t" || character.isNewline
            || character == "\"" || character == "'" || character == "="
    }

    /// omp's `@` reference ending the prefix: the token after the last path
    /// delimiter starts with `@`, or an unclosed `"` on the line opens right
    /// after an `@` that starts a token.
    static func pathQuery(_ prefix: String) -> (start: Int, query: String, quoted: Bool)? {
        let lineStart = prefix.lastIndex(where: \.isNewline).map(prefix.index(after:)) ?? prefix.startIndex
        let line = prefix[lineStart...]
        if line.filter({ $0 == "\"" }).count % 2 == 1, let open = line.lastIndex(of: "\""), open > lineStart {
            let at = line.index(before: open)
            let startsToken = at == lineStart || isPathDelimiter(line[line.index(before: at)])
            if line[at] == "@", startsToken {
                let query = line[line.index(after: open)...]
                guard !query.contains(chipCharacter) else { return nil }
                return (offset(of: at, in: prefix), String(query), true)
            }
        }
        let start = prefix.lastIndex(where: isPathDelimiter).map(prefix.index(after:)) ?? prefix.startIndex
        let token = prefix[start...]
        guard token.hasPrefix("@"), !token.contains(chipCharacter) else { return nil }
        return (offset(of: start, in: prefix), String(token.dropFirst()), false)
    }

    private static func offset(of index: String.Index, in text: String) -> Int {
        text.utf16.distance(from: text.startIndex, to: index)
    }

    // MARK: Rows

    /// The project's own commands, then the server's; the first occurrence of
    /// a name wins.
    static func commands(in catalog: Catalog, projectId: String?) -> [CatalogCommand] {
        var names = Set<String>()
        let project = projectId.flatMap { catalog.projectCommands[$0] } ?? []
        return (project + catalog.commands).filter { names.insert($0.name).inserted }
    }

    /// Commands and skills (as `/skill:<name>`) matching `query`, the text
    /// after `/`: prefix matches before substring matches, commands before
    /// skills within each group. A skill's name also matches after `skill:`.
    static func rank(commands: [CatalogCommand], skills: [CatalogSkill], query: String) -> [Completion] {
        let query = query.lowercased()
        let rows = commands.map { command in
            Completion(kind: .command, label: "/" + command.name,
                       detail: nonEmpty(command.description), hint: command.hint.flatMap(nonEmpty))
        } + skills.map { skill in
            Completion(kind: .skill, label: "/skill:" + skill.name, detail: nonEmpty(skill.description))
        }
        var prefix: [Completion] = []
        var contains: [Completion] = []
        for row in rows {
            let name = row.label.dropFirst().lowercased()
            let skillName = row.kind == .skill ? String(name.dropFirst("skill:".count)) : nil
            if name.hasPrefix(query) || skillName?.hasPrefix(query) == true {
                prefix.append(row)
            } else if name.contains(query) {
                contains.append(row)
            }
        }
        return prefix + contains
    }

    /// Catalog models matching `query` (after `^`) on their name or slug,
    /// ignoring case: prefix matches before substring matches.
    static func rank(models: [CatalogModel], query: String) -> [Completion] {
        let query = query.lowercased()
        var prefix: [Completion] = []
        var contains: [Completion] = []
        for model in models {
            let name = displayName(model)
            let keys = [name.lowercased(), model.slug.lowercased()]
            let row = Completion(kind: .model(slug: model.slug), label: name, detail: model.slug)
            if keys.contains(where: { $0.hasPrefix(query) }) {
                prefix.append(row)
            } else if keys.contains(where: { $0.contains(query) }) {
                contains.append(row)
            }
        }
        return prefix + contains
    }

    /// Rows for searched entries: the name (a directory's with a trailing
    /// `/`) over the whole path.
    static func rows(entries: [Entry]) -> [Completion] {
        entries.map { entry in
            let directory = entry.kind == .dir
            let path = directory && entry.path.hasSuffix("/") ? String(entry.path.dropLast()) : entry.path
            let name = fileName(path)
            return Completion(kind: .path(path, directory: directory),
                              label: directory ? name + "/" : name,
                              detail: path == name ? nil : path)
        }
    }

    /// The name a model shows: its catalog name, else its id.
    static func displayName(_ model: CatalogModel) -> String {
        let name = model.name.trimmingCharacters(in: .whitespacesAndNewlines)
        return name.isEmpty ? model.id : name
    }

    static func fileName(_ path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? path
    }

    private static func nonEmpty(_ text: String) -> String? {
        text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : text
    }

    // MARK: Accepting

    /// The token a file reference is written as: `@path`, or `@"path"` for
    /// a path with whitespace or a reference typed quoted.
    static func fileToken(_ path: String, quoted: Bool = false) -> String {
        quoted || path.contains(where: \.isWhitespace) ? "@\"\(path)\"" : "@" + path
    }

    /// What accepting `completion` for `query` does to `text`, the caret at
    /// `cursor`: a command, skill, model or file becomes a chip and a space;
    /// a directory becomes the text `@dir/` so completion continues in it.
    static func edit(accepting completion: Completion, for query: CompletionQuery,
                     in text: String, cursor: Int) -> CompletionEdit
    {
        let string = text as NSString
        var end = cursor
        if case .slash = query {
            // The whole leading token, wherever the caret is in it.
            while end < string.length, !isWhitespace(string.character(at: end)) { end += 1 }
        }
        let quoted = if case let .path(_, _, quoted) = query { quoted } else { false }
        let chip: Chip
        switch completion.kind {
        case .command, .skill:
            chip = Chip(kind: .command, token: completion.label, label: completion.label)
        case let .model(slug):
            chip = Chip(kind: .model, token: "^" + slug, label: completion.label)
        case let .path(path, directory: true):
            let insert = quoted || path.contains(where: \.isWhitespace) ? "@\"\(path)/" : "@\(path)/"
            return CompletionEdit(range: NSRange(location: query.start, length: end - query.start),
                                  parts: [.text(insert)])
        case let .path(path, directory: false):
            chip = Chip(kind: .file, token: fileToken(path, quoted: quoted), label: fileName(path))
        }
        // The space after the chip replaces one already there.
        if end < string.length, string.character(at: end) == 0x20 { end += 1 }
        return CompletionEdit(range: NSRange(location: query.start, length: end - query.start),
                              parts: [.chip(chip), .text(" ")])
    }

    private static func isWhitespace(_ unit: unichar) -> Bool {
        guard let scalar = Unicode.Scalar(unit) else { return false }
        return CharacterSet.whitespacesAndNewlines.contains(scalar)
    }
}
