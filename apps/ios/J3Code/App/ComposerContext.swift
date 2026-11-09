import Foundation

/// What the composer (`Composer/`) needs from the screen it sits in: the
/// open thread or the new-thread form.
@MainActor
protocol ComposerContext: AnyObject {
    /// Models, skills and slash commands; `nil` before the first connection.
    var catalog: Catalog? { get }
    /// The project `@path` completion searches and whose slash commands
    /// (`catalog.projectCommands[projectId]`) are offered; `nil` when none is
    /// chosen yet.
    var projectId: String? { get }
    /// Return sends on a hardware keyboard (otherwise ⌘Return does).
    var returnSends: Bool { get }
    /// `projects.searchEntries` in `projectId`.
    func searchEntries(query: String) async throws -> EntriesResult
    /// Uploads an image or file for the next message.
    func upload(name: String, mimeType: String, data: Data) async throws -> Attachment
}

/// A message the composer hands over on send.
struct ComposerMessage: Equatable {
    /// The draft with every chip written as its token (`/skill:name`,
    /// `^provider/id`, `@path`), as omp reads it.
    var text: String
    var attachments: [Attachment]
    /// The model and thinking level picked for this message.
    var model: ModelRef?
}
