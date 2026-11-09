import Foundation
import Observation
import UIKit

/// What the composer's text view offers its model: the draft it holds and the
/// edit that accepting a completion makes.
@MainActor
protocol ComposerEditing: AnyObject {
    var draftText: String { get }
    var draftParts: [DraftPart] { get }
    var cursorOffset: Int { get }
    func apply(_ edit: CompletionEdit)
    func focus()
}

/// One image in the composer's attachment strip: uploading, uploaded, or
/// failed with the reason.
struct ComposerUpload: Identifiable {
    enum State: Equatable {
        case uploading
        case uploaded(Attachment)
        case failed(String)
    }

    let id = UUID()
    var name: String
    var thumbnail: UIImage?
    var state: State

    var attachment: Attachment? {
        if case let .uploaded(attachment) = state { attachment } else { nil }
    }

    var isUploading: Bool { state == .uploading }
}

/// The composer's state: the draft with its chips, the images uploaded for the
/// next message, the model and thinking level, and the completion menu.
@MainActor @Observable
final class ComposerModel {
    /// The model the next message asks for.
    var model: ModelRef?
    @ObservationIgnored weak var context: (any ComposerContext)?
    /// The draft as the text view holds it, kept in step by the editor.
    var parts: [DraftPart] = []
    private(set) var uploads: [ComposerUpload] = []
    private(set) var completionRows: [Completion] = []
    private(set) var completionSelection = 0
    @ObservationIgnored private(set) var completionQuery: CompletionQuery?
    @ObservationIgnored weak var editor: (any ComposerEditing)?
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var searchGeneration = 0
    @ObservationIgnored private var uploadTasks: [UUID: Task<Void, Never>] = [:]
    @ObservationIgnored private var imageNumber = 0

    init(context: any ComposerContext, model: ModelRef?) {
        self.context = context
        self.model = model
    }

    // MARK: Sending

    var catalog: Catalog? { context?.catalog }
    var returnSends: Bool { context?.returnSends ?? true }
    /// True when the message has non-blank text or an uploaded image, and no
    /// image is still uploading.
    var canSend: Bool {
        guard !isUploading else { return false }
        return !Draft.isBlank(parts) || uploads.contains { $0.attachment != nil }
    }

    var isUploading: Bool { uploads.contains(where: \.isUploading) }
    var isEmpty: Bool { Draft.serialize(parts).isEmpty }

    /// The message the draft makes, then an empty composer; nil when there is
    /// nothing to send or an image is still uploading.
    func take() -> ComposerMessage? {
        guard canSend else { return nil }
        let message = ComposerMessage(
            text: Draft.serialize(parts),
            attachments: uploads.compactMap(\.attachment),
            model: model
        )
        parts = []
        uploads = []
        closeCompletion()
        return message
    }

    /// Replaces the draft with `text`, as plain text: tokens in it stay text.
    func load(text: String) {
        parts = text.isEmpty ? [] : [.text(text)]
        closeCompletion()
    }

    func focus() { editor?.focus() }

    /// Sets the thinking level of the picked model.
    func setThinking(_ level: ThinkingLevel) {
        guard var ref = model else { return }
        ref.thinking = level
        model = ref
    }

    // MARK: Completion

    var completionOpen: Bool { !completionRows.isEmpty }

    /// Recomputes the menu for `text` with the caret at `cursor`, a UTF-16
    /// offset. File references search the project after a short pause; only
    /// the newest search's answer is used.
    func updateCompletion(text: String, cursor: Int) {
        searchTask?.cancel()
        searchTask = nil
        searchGeneration += 1
        guard let query = ComposerCompletion.query(in: text, cursor: cursor) else {
            closeCompletion()
            return
        }
        completionQuery = query
        switch query {
        case let .slash(query):
            guard let catalog else {
                closeCompletion()
                return
            }
            let commands = ComposerCompletion.commands(in: catalog, projectId: context?.projectId)
            present(ComposerCompletion.rank(commands: commands, skills: catalog.skills, query: query))
        case let .model(_, query):
            guard let catalog else {
                closeCompletion()
                return
            }
            present(ComposerCompletion.rank(models: catalog.models, query: query))
        case let .path(_, query, _):
            completionRows = []
            completionSelection = 0
            search(query)
        }
    }

    func closeCompletion() {
        searchTask?.cancel()
        searchTask = nil
        searchGeneration += 1
        completionRows = []
        completionSelection = 0
        completionQuery = nil
    }

    /// Moves the menu's selection by `delta`, wrapping at the ends.
    func moveSelection(_ delta: Int) {
        guard completionOpen else { return }
        let count = completionRows.count
        completionSelection = ((completionSelection + delta) % count + count) % count
    }

    /// Accepts the selected row, if the menu is open.
    @discardableResult
    func acceptSelected() -> Bool {
        guard completionOpen, completionRows.indices.contains(completionSelection) else { return false }
        accept(completionRows[completionSelection])
        return true
    }

    /// Replaces the token being completed with `completion`'s chip (or, for a
    /// directory, the text that continues completion inside it).
    func accept(_ completion: Completion) {
        guard let editor, let query = completionQuery else { return }
        let edit = ComposerCompletion.edit(
            accepting: completion, for: query, in: editor.draftText, cursor: editor.cursorOffset
        )
        editor.apply(edit)
        parts = editor.draftParts
        closeCompletion()
    }

    private func present(_ rows: [Completion]) {
        guard !rows.isEmpty else {
            closeCompletion()
            return
        }
        completionRows = Array(rows.prefix(6))
        completionSelection = 0
    }

    private func search(_ query: String) {
        guard let context else { return }
        let generation = searchGeneration
        searchTask = Task { @MainActor [weak self] in
            do {
                try await Task.sleep(for: .milliseconds(150))
            } catch {
                return
            }
            guard !Task.isCancelled else { return }
            do {
                let result = try await context.searchEntries(query: query)
                guard !Task.isCancelled, let self, generation == self.searchGeneration else { return }
                self.present(ComposerCompletion.rows(entries: result.entries))
            } catch {
                guard !Task.isCancelled, let self, generation == self.searchGeneration else { return }
                self.closeCompletion()
            }
        }
    }

    // MARK: Images

    /// Encodes and uploads one picked or pasted image; the strip shows it
    /// while it uploads and its reason when it fails.
    func addImage(_ data: Data, baseName: String) {
        imageNumber += 1
        let upload = ComposerUpload(
            name: "\(baseName) \(imageNumber)",
            thumbnail: ImageEncoder.thumbnail(data, side: 56),
            state: .uploading
        )
        uploads.append(upload)
        let name = upload.name
        uploadTasks[upload.id] = Task { @MainActor [weak self] in
            await self?.upload(upload.id, data: data, baseName: name)
        }
    }

    func remove(_ id: UUID) {
        uploadTasks[id]?.cancel()
        uploadTasks[id] = nil
        uploads.removeAll { $0.id == id }
    }

    private func upload(_ id: UUID, data: Data, baseName: String) async {
        do {
            guard let context else { throw ClientError.Disconnected }
            let encoded = try await Task.detached(priority: .userInitiated) {
                try ImageEncoder.encode(data, baseName: baseName)
            }.value
            guard !Task.isCancelled else { return }
            let attachment = try await context.upload(
                name: encoded.name, mimeType: encoded.mimeType, data: encoded.data
            )
            guard !Task.isCancelled else { return }
            update(id) { $0.state = .uploaded(attachment) }
        } catch {
            guard !Task.isCancelled else { return }
            update(id) { $0.state = .failed(describe(error)) }
        }
        uploadTasks[id] = nil
    }

    private func update(_ id: UUID, _ change: (inout ComposerUpload) -> Void) {
        guard let index = uploads.firstIndex(where: { $0.id == id }) else { return }
        change(&uploads[index])
    }
}
