import SwiftUI
import UIKit

/// The feed's sheets: a tool call's detail or a subagent's transcript. Rows
/// hold the router (a stable reference) instead of closures so unchanged rows
/// compare equal while the feed streams.
@MainActor @Observable
final class FeedRouter {
    enum Sheet: Identifiable, Hashable {
        case tool(itemId: String)
        case transcript(threadId: String, subagentId: String, agent: String)

        var id: Self { self }
    }

    var sheet: Sheet?
}

/// Opens images in the full-screen viewer and files in Quick Look. Installed
/// by `MediaPresenting` on the feed and on each sheet, since a presented
/// sheet cannot be covered by its presenter's covers.
@MainActor @Observable
final class MediaRouter {
    struct Target: Identifiable, Hashable {
        let url: URL
        let name: String
        var id: URL { url }
    }

    var image: Target?
    var file: Target?

    func showImage(_ url: URL, name: String) {
        image = Target(url: url, name: name)
    }

    func preview(_ url: URL, name: String) {
        file = Target(url: url, name: name)
    }
}

extension EnvironmentValues {
    @Entry var media: MediaRouter? = nil
}

struct MediaPresenting: ViewModifier {
    @State private var router = MediaRouter()

    func body(content: Content) -> some View {
        @Bindable var router = router
        content
            .environment(\.media, router)
            .fullScreenCover(item: $router.image) { target in
                ImageViewer(target: target)
            }
            .sheet(item: $router.file) { target in
                FilePreviewSheet(target: target)
            }
    }
}

extension Host {
    /// A link or image source from markdown: absolute as is, else a server path.
    func resolve(_ link: String) -> URL? {
        if let url = URL(string: link), url.scheme != nil { return url }
        return URL(string: url(path: link))
    }

    func attachment(_ id: String) -> URL? {
        URL(string: attachmentUrl(attachmentId: id))
    }
}

/// Decoded images by URL, shared by thumbnails, markdown images and the viewer.
final class ImageStore: @unchecked Sendable {
    static let shared = ImageStore()
    private let cache = NSCache<NSURL, UIImage>()

    func cached(_ url: URL) -> UIImage? {
        cache.object(forKey: url as NSURL)
    }

    func image(for url: URL) async throws -> UIImage {
        if let image = cached(url) { return image }
        let (data, response) = try await URLSession.shared.data(from: url)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw URLError(.badServerResponse)
        }
        guard let decoded = UIImage(data: data) else { throw URLError(.cannotDecodeContentData) }
        let image = await decoded.byPreparingForDisplay() ?? decoded
        cache.setObject(image, forKey: url as NSURL)
        return image
    }
}

/// An image from the server, from `ImageStore`.
struct RemoteImage: View {
    let url: URL?
    var contentMode: ContentMode = .fit
    @State private var image: UIImage?
    @State private var failed = false

    init(url: URL?, contentMode: ContentMode = .fit) {
        self.url = url
        self.contentMode = contentMode
        _image = State(initialValue: url.flatMap { ImageStore.shared.cached($0) })
    }

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image)
                    .resizable()
                    .aspectRatio(contentMode: contentMode)
            } else if failed {
                Image(systemName: "photo.badge.exclamationmark")
                    .font(.title2)
                    .foregroundStyle(.secondary)
                    .frame(minWidth: 60, minHeight: 60)
            } else {
                ProgressView()
                    .frame(minWidth: 60, minHeight: 60)
            }
        }
        .task(id: url) {
            guard let url, image == nil else { return }
            do {
                image = try await ImageStore.shared.image(for: url)
            } catch {
                failed = !Task.isCancelled
            }
        }
    }
}

enum FeedFormat {
    /// "12K"-style token counts.
    static func tokens(_ count: UInt64) -> String {
        count.formatted(.number.notation(.compactName))
    }

    static func bytes(_ count: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: count), countStyle: .file)
    }
}
