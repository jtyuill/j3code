import QuickLook
import SwiftUI
import UIKit

/// A full-screen image: pinch or double-tap to zoom, drag to pan.
struct ImageViewer: View {
    let target: MediaRouter.Target
    @Environment(\.dismiss) private var dismiss
    @State private var image: UIImage?
    @State private var failed = false

    init(target: MediaRouter.Target) {
        self.target = target
        _image = State(initialValue: ImageStore.shared.cached(target.url))
    }

    var body: some View {
        NavigationStack {
            ZStack {
                Color.black.ignoresSafeArea()
                if let image {
                    ZoomableImage(image: image)
                        .ignoresSafeArea()
                        .accessibilityLabel(target.name)
                        .accessibilityAddTraits(.isImage)
                } else if failed {
                    ContentUnavailableView("Image unavailable", systemImage: "photo")
                        .foregroundStyle(.white)
                } else {
                    ProgressView().tint(.white)
                }
            }
            .navigationTitle(target.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(.black.opacity(0.6), for: .navigationBar)
            .toolbarColorScheme(.dark, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close", systemImage: "xmark") { dismiss() }
                }
                if let image {
                    ToolbarItem(placement: .primaryAction) {
                        let shared = Image(uiImage: image)
                        ShareLink(item: shared, preview: SharePreview(target.name, image: shared))
                    }
                }
            }
        }
        .task {
            guard image == nil else { return }
            do {
                image = try await ImageStore.shared.image(for: target.url)
            } catch {
                failed = true
            }
        }
    }
}

private struct ZoomableImage: UIViewRepresentable {
    let image: UIImage

    func makeUIView(context: Context) -> ZoomingScrollView {
        ZoomingScrollView(image: image)
    }

    func updateUIView(_ view: ZoomingScrollView, context: Context) {
        view.imageView.image = image
    }
}

final class ZoomingScrollView: UIScrollView, UIScrollViewDelegate {
    let imageView: UIImageView

    init(image: UIImage) {
        imageView = UIImageView(image: image)
        super.init(frame: .zero)
        imageView.contentMode = .scaleAspectFit
        addSubview(imageView)
        delegate = self
        minimumZoomScale = 1
        maximumZoomScale = 8
        showsVerticalScrollIndicator = false
        showsHorizontalScrollIndicator = false
        contentInsetAdjustmentBehavior = .never
        let doubleTap = UITapGestureRecognizer(target: self, action: #selector(doubleTapped(_:)))
        doubleTap.numberOfTapsRequired = 2
        addGestureRecognizer(doubleTap)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        if zoomScale == minimumZoomScale, imageView.frame.size != bounds.size {
            imageView.frame = bounds
            contentSize = bounds.size
        }
    }

    func viewForZooming(in scrollView: UIScrollView) -> UIView? {
        imageView
    }

    @objc private func doubleTapped(_ recognizer: UITapGestureRecognizer) {
        if zoomScale > minimumZoomScale {
            setZoomScale(minimumZoomScale, animated: true)
        } else {
            let point = recognizer.location(in: imageView)
            let scale: CGFloat = 3
            let size = CGSize(width: bounds.width / scale, height: bounds.height / scale)
            let rect = CGRect(
                x: point.x - size.width / 2, y: point.y - size.height / 2,
                width: size.width, height: size.height
            )
            zoom(to: rect, animated: true)
        }
    }
}

/// Downloads a file to a temporary file named after it and shows it in Quick Look.
struct FilePreviewSheet: View {
    let target: MediaRouter.Target
    @Environment(\.dismiss) private var dismiss
    @State private var file: URL?
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Group {
                if let file {
                    QuickLookPreview(url: file)
                        .ignoresSafeArea(edges: .bottom)
                } else if let error {
                    ContentUnavailableView(
                        "Can't open \(target.name)", systemImage: "doc", description: Text(error)
                    )
                } else {
                    ProgressView("Downloading…")
                }
            }
            .navigationTitle(target.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
                if let file {
                    ToolbarItem(placement: .primaryAction) {
                        ShareLink(item: file)
                    }
                }
            }
        }
        .task { await download() }
        .onDisappear {
            if let file { try? FileManager.default.removeItem(at: file.deletingLastPathComponent()) }
        }
    }

    private func download() async {
        do {
            let (temporary, response) = try await URLSession.shared.download(from: target.url)
            if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                throw URLError(.badServerResponse)
            }
            let directory = FileManager.default.temporaryDirectory
                .appending(path: UUID().uuidString, directoryHint: .isDirectory)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let name = target.name.replacingOccurrences(of: "/", with: "_")
            let destination = directory.appending(path: name.isEmpty ? "file" : name)
            try FileManager.default.moveItem(at: temporary, to: destination)
            file = destination
        } catch {
            self.error = describe(error)
        }
    }
}

private struct QuickLookPreview: UIViewControllerRepresentable {
    let url: URL

    func makeCoordinator() -> Source { Source(url: url) }

    func makeUIViewController(context: Context) -> QLPreviewController {
        let controller = QLPreviewController()
        controller.dataSource = context.coordinator
        return controller
    }

    func updateUIViewController(_ controller: QLPreviewController, context: Context) {
        if context.coordinator.url != url {
            context.coordinator.url = url
            controller.reloadData()
        }
    }

    final class Source: NSObject, QLPreviewControllerDataSource {
        var url: URL

        init(url: URL) { self.url = url }

        func numberOfPreviewItems(in controller: QLPreviewController) -> Int { 1 }

        func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> any QLPreviewItem {
            url as NSURL
        }
    }
}
