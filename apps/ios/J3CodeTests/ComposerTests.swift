import XCTest
import UIKit

@testable import J3Code

@MainActor
final class ComposerTests: XCTestCase {
    // MARK: Queries

    func testSlashQueryOnlyAtMessageStart() {
        XCTAssertEqual(ComposerCompletion.query(in: "/com", cursor: 4), .slash("com"))
        XCTAssertEqual(ComposerCompletion.query(in: "/", cursor: 1), .slash(""))
        XCTAssertEqual(ComposerCompletion.query(in: "/compact now", cursor: 3), .slash("co"))
        XCTAssertNil(ComposerCompletion.query(in: "/compact now", cursor: 12))
        XCTAssertNil(ComposerCompletion.query(in: "hi /x", cursor: 5))
        XCTAssertNil(ComposerCompletion.query(in: "hi /x", cursor: 2))
        XCTAssertNil(ComposerCompletion.slashQuery("hi /x"))
    }

    func testModelQueryAfterWhitespaceAndAtStart() {
        XCTAssertEqual(ComposerCompletion.query(in: "^clau", cursor: 5), .model(start: 0, query: "clau"))
        XCTAssertEqual(ComposerCompletion.query(in: "use ^clau", cursor: 9), .model(start: 4, query: "clau"))
        XCTAssertEqual(ComposerCompletion.query(in: "use ^clau", cursor: 7), .model(start: 4, query: "cl"))
        XCTAssertNil(ComposerCompletion.query(in: "a^b", cursor: 3))
        XCTAssertNil(ComposerCompletion.query(in: "^a\u{FFFC}", cursor: 3))
        // omp never expands mentions in local-execution drafts.
        XCTAssertNil(ComposerCompletion.query(in: "!ls ^gpt", cursor: 8))
    }

    func testPathQueryPlainAndQuoted() {
        XCTAssertEqual(
            ComposerCompletion.query(in: "see @src/ma", cursor: 11),
            .path(start: 4, query: "src/ma", quoted: false)
        )
        XCTAssertEqual(
            ComposerCompletion.query(in: "see @\"my dir/fi", cursor: 15),
            .path(start: 4, query: "my dir/fi", quoted: true)
        )
        XCTAssertEqual(
            ComposerCompletion.query(in: "key=@x", cursor: 6),
            .path(start: 4, query: "x", quoted: false)
        )
        XCTAssertNil(ComposerCompletion.query(in: "mail me@host", cursor: 12))
        XCTAssertNil(ComposerCompletion.query(in: "see @\"a b\" c", cursor: 12))
        XCTAssertNil(ComposerCompletion.query(in: "look at @src/ma ", cursor: 16))
    }

    // MARK: Ranking

    func testRankingPrefixBeforeSubstringAndCommandsBeforeSkills() {
        let commands = [
            CatalogCommand(name: "review", description: "", hint: nil),
            CatalogCommand(name: "preview", description: "", hint: nil),
        ]
        let skills = [
            CatalogSkill(name: "preview-helper", description: "Helps"),
            CatalogSkill(name: "jev", description: "Judges"),
        ]
        func labels(_ query: String) -> [String] {
            ComposerCompletion.rank(commands: commands, skills: skills, query: query).map(\.label)
        }

        XCTAssertEqual(labels(""), ["/review", "/preview", "/skill:preview-helper", "/skill:jev"])
        XCTAssertEqual(labels("view"), ["/review", "/preview", "/skill:preview-helper"])
        XCTAssertEqual(labels("pre"), ["/preview", "/skill:preview-helper"])
        XCTAssertEqual(labels("skill:j"), ["/skill:jev"])
        XCTAssertEqual(labels("JEV"), ["/skill:jev"])
        XCTAssertEqual(ComposerCompletion.rank(commands: commands, skills: skills, query: "view")[0].kind,
                       .command)

        let ordered = ComposerCompletion.rank(
            commands: [
                CatalogCommand(name: "xray", description: "", hint: nil),
                CatalogCommand(name: "ray", description: "", hint: nil),
            ],
            skills: [], query: "ray"
        ).map(\.label)
        XCTAssertEqual(ordered, ["/ray", "/xray"])
    }

    func testModelRanking() {
        let models = [
            CatalogModel(provider: "z", id: "xclaude", name: "XClaude", thinking: nil, default: false),
            CatalogModel(provider: "anthropic", id: "claude-x", name: "Claude X", thinking: nil, default: true),
            CatalogModel(provider: "openai", id: "gpt-5", name: "GPT 5", thinking: nil, default: false),
        ]
        func labels(_ query: String) -> [String] {
            ComposerCompletion.rank(models: models, query: query).map(\.label)
        }

        XCTAssertEqual(labels("claude"), ["Claude X", "XClaude"])
        XCTAssertEqual(labels("gpt"), ["GPT 5"])
        XCTAssertEqual(labels("open"), ["GPT 5"])
        XCTAssertEqual(labels("claude-x"), ["Claude X"])
        XCTAssertTrue(labels("zzz").isEmpty)
    }

    func testProjectCommandsComeFirstAndShadowTheServers() {
        let catalog = Catalog(
            models: [], skills: [],
            commands: [CatalogCommand(name: "compact", description: "server", hint: "[focus]")],
            projectCommands: [
                "p": [
                    CatalogCommand(name: "compact", description: "project", hint: nil),
                    CatalogCommand(name: "review", description: "", hint: nil),
                ],
            ],
            status: .ready, message: nil
        )
        let commands = ComposerCompletion.commands(in: catalog, projectId: "p")
        XCTAssertEqual(commands.map(\.name), ["compact", "review"])
        XCTAssertEqual(commands[0].description, "project")
    }

    // MARK: Chips

    func testTextEditorGrowsToEightLinesThenClamps() {
        let view = ComposerTextView()
        let width: CGFloat = 200

        view.text = "one line"
        let one = view.measuredHeight(forWidth: width)
        view.text = (1...3).map { "Line \($0)" }.joined(separator: "\n")
        let three = view.measuredHeight(forWidth: width)
        view.text = (1...20).map { "Line \($0)" }.joined(separator: "\n")
        let many = view.measuredHeight(forWidth: width)

        XCTAssertGreaterThan(three, one)
        XCTAssertGreaterThan(many, three)
        XCTAssertEqual(many, ceil(view.font!.lineHeight) * 8, accuracy: 1)
    }

    func testChipSerializationRoundTrip() {
        let parts: [DraftPart] = [
            .chip(Chip(kind: .file, token: ComposerCompletion.fileToken("a b/c"), label: "c")),
            .text(" and "),
            .chip(Chip(kind: .model, token: "^p/m", label: "M")),
            .text(" done"),
        ]
        XCTAssertEqual(ComposerCompletion.fileToken("a b/c"), "@\"a b/c\"")
        XCTAssertEqual(ComposerCompletion.fileToken("src/main.rs"), "@src/main.rs")
        XCTAssertEqual(Draft.serialize(parts), "@\"a b/c\" and ^p/m done")

        let attributed = Draft.attributedString(parts, attributes: [:]) { chip in
            ChipRenderer.attachment(
                chip, font: UIFont.preferredFont(forTextStyle: .body), trait: UITraitCollection.current
            )
        }
        XCTAssertEqual(Draft.parts(from: attributed), parts)
        XCTAssertEqual(Draft.serialize(Draft.parts(from: attributed)), "@\"a b/c\" and ^p/m done")
    }

    func testAcceptingChipsAndDirectories() {
        let command = Completion(kind: .command, label: "/compact", detail: "Compacts", hint: nil)
        let slash = ComposerCompletion.edit(
            accepting: command, for: .slash("co"), in: "/co", cursor: 3
        )
        XCTAssertEqual(slash.range, NSRange(location: 0, length: 3))
        XCTAssertEqual(slash.parts, [
            .chip(Chip(kind: .command, token: "/compact", label: "/compact")),
            .text(" "),
        ])

        let directory = Completion(kind: .path("my dir", directory: true), label: "my dir/", detail: "my dir", hint: nil)
        let path = ComposerCompletion.edit(
            accepting: directory, for: .path(start: 4, query: "my", quoted: false), in: "see @my", cursor: 7
        )
        XCTAssertEqual(path.range, NSRange(location: 4, length: 3))
        XCTAssertEqual(path.parts, [.text("@\"my dir/")])

        let file = Completion(kind: .path("src/main.rs", directory: false), label: "main.rs", detail: "src/main.rs", hint: nil)
        let accepted = ComposerCompletion.edit(
            accepting: file, for: .path(start: 4, query: "src/ma", quoted: false), in: "see @src/ma", cursor: 11
        )
        XCTAssertEqual(accepted.parts, [
            .chip(Chip(kind: .file, token: "@src/main.rs", label: "main.rs")),
            .text(" "),
        ])
    }

    // MARK: Sending

    func testTakeReturnsMessageAndClears() {
        let context = FakeContext()
        let ref = ModelRef(provider: "p", id: "m", thinking: .high)
        let model = ComposerModel(context: context, model: ref)
        XCTAssertFalse(model.canSend)
        XCTAssertNil(model.take())

        model.load(text: " \n ")
        XCTAssertFalse(model.canSend)

        model.load(text: "Hello")
        XCTAssertTrue(model.canSend)
        XCTAssertEqual(model.take(), ComposerMessage(text: "Hello", attachments: [], model: ref))
        XCTAssertFalse(model.canSend)
        XCTAssertNil(model.take())
        XCTAssertTrue(model.parts.isEmpty)
    }

    func testCannotSendWhileUploadInFlight() async throws {
        let context = FakeContext()
        context.blocksUploads = true
        let model = ComposerModel(context: context, model: nil)
        model.addImage(Self.pngData(), baseName: "Test")

        XCTAssertEqual(model.uploads.count, 1)
        XCTAssertTrue(model.isUploading)
        XCTAssertFalse(model.canSend)

        model.load(text: "Hi")
        XCTAssertFalse(model.canSend)

        context.releaseUpload()
        try await waitUntil { !model.isUploading }
        XCTAssertTrue(model.canSend)

        let message = model.take()
        XCTAssertEqual(message?.text, "Hi")
        XCTAssertEqual(message?.attachments.count, 1)
        XCTAssertNil(model.take())
        XCTAssertTrue(model.uploads.isEmpty)
    }

    func testUploadedImageAloneCanSend() async throws {
        let context = FakeContext()
        let model = ComposerModel(context: context, model: nil)
        model.addImage(Self.pngData(), baseName: "Test")
        try await waitUntil { !model.isUploading }
        XCTAssertTrue(model.canSend)

        let message = model.take()
        XCTAssertEqual(message?.text, "")
        XCTAssertEqual(message?.attachments.count, 1)
    }

    func testPickedFileUploadsUnderItsName() async throws {
        let context = FakeContext()
        let model = ComposerModel(context: context, model: nil)
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("notes \(UUID().uuidString).md")
        try Data("remember the milk".utf8).write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }

        model.addFile(url)
        XCTAssertEqual(model.uploads.map(\.name), [url.lastPathComponent])
        XCTAssertNil(model.uploads[0].thumbnail)
        try await waitUntil { !model.isUploading }

        // A file alone is a message; omp reads it as an `@` mention.
        let attachment = try XCTUnwrap(model.take()?.attachments.first)
        XCTAssertEqual(attachment.name, url.lastPathComponent)
        XCTAssertEqual(attachment.size, 17)
        XCTAssertFalse(attachment.mimeType.hasPrefix("image/"))
    }

    func testMissingFileFailsItsUpload() async throws {
        let model = ComposerModel(context: FakeContext(), model: nil)
        model.addFile(FileManager.default.temporaryDirectory.appendingPathComponent("gone-\(UUID().uuidString).txt"))
        try await waitUntil { !model.isUploading }
        guard case .failed = model.uploads[0].state else { return XCTFail("the upload should fail") }
        XCTAssertFalse(model.canSend)
    }

    // MARK: Side questions

    func testOnlyABtwDraftIsASideQuestion() {
        XCTAssertEqual(SideQuestion.question(in: "/btw why is it slow?"), "why is it slow?")
        XCTAssertEqual(SideQuestion.question(in: "/btw\n two\nlines "), "two\nlines")
        XCTAssertEqual(SideQuestion.question(in: "/btw"), "")
        XCTAssertNil(SideQuestion.question(in: "/btwx why"))
        XCTAssertNil(SideQuestion.question(in: "so /btw why"))
        XCTAssertNil(SideQuestion.question(in: "/compact"))

        // The command's chip counts as its token.
        let model = ComposerModel(context: FakeContext(), model: nil)
        model.parts = [.chip(Chip(kind: .command, token: "/btw", label: "btw")), .text(" why?")]
        XCTAssertEqual(model.sideQuestion, "why?")
        model.clear()
        XCTAssertNil(model.sideQuestion)
        XCTAssertTrue(model.parts.isEmpty)
    }

    // MARK: Helpers

    private func waitUntil(timeout: TimeInterval = 5, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            guard Date() < deadline else {
                XCTFail("Timed out waiting for the composer")
                return
            }
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    private static func pngData() -> Data {
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4))
        let image = renderer.image { context in
            UIColor.systemBlue.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
        }
        return image.pngData()!
    }
}

/// The composer's context without a host: no catalog, and uploads that the
/// test can hold open.
@MainActor
private final class FakeContext: ComposerContext {
    var catalog: Catalog?
    var projectId: String?
    var returnSends = true
    var entries: [Entry] = []
    var blocksUploads = false

    private var released = false
    private var waiting: CheckedContinuation<Void, Never>?

    func searchEntries(query: String) async throws -> EntriesResult {
        EntriesResult(entries: entries, truncated: false)
    }

    func upload(name: String, mimeType: String, data: Data) async throws -> Attachment {
        let attachment = Attachment(
            id: UUID().uuidString, name: name, mimeType: mimeType, size: UInt64(data.count)
        )
        if blocksUploads, !released {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                waiting = continuation
            }
        }
        return attachment
    }

    func releaseUpload() {
        released = true
        waiting?.resume()
        waiting = nil
    }
}
