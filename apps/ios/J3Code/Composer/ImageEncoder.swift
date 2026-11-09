import Foundation
import ImageIO
import UIKit
import UniformTypeIdentifiers

/// An image ready to upload.
struct EncodedImage: Sendable {
    var data: Data
    var mimeType: String
    var name: String
}

/// Prepares picked and pasted images for `POST /api/v1/attachments`, which
/// takes images up to 10 MiB.
enum ImageEncoder {
    static let maxBytes = 10 * 1024 * 1024
    static let maxSide = 2048

    enum Failure: LocalizedError {
        case unreadable
        case tooLarge

        var errorDescription: String? {
            switch self {
            case .unreadable: "The image couldn't be read."
            case .tooLarge: "The image is larger than 10 MB."
            }
        }
    }

    /// `data` as it is when a GIF or PNG within the size limit, anything else
    /// as a JPEG with quality 0.85 scaled to 2048 px on its longest side.
    /// `baseName` gets the extension.
    static func encode(_ data: Data, baseName: String) throws -> EncodedImage {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              CGImageSourceGetCount(source) > 0
        else { throw Failure.unreadable }
        let type = CGImageSourceGetType(source).flatMap { UTType($0 as String) }
        if let type, data.count <= maxBytes {
            if type.conforms(to: .gif) {
                return EncodedImage(data: data, mimeType: "image/gif", name: baseName + ".gif")
            }
            if type.conforms(to: .png) {
                return EncodedImage(data: data, mimeType: "image/png", name: baseName + ".png")
            }
        }
        guard let image = scaled(source, maxSide: maxSide).map(opaque) else { throw Failure.unreadable }
        guard let jpeg = jpeg(image, quality: 0.85), jpeg.count <= maxBytes else { throw Failure.tooLarge }
        return EncodedImage(data: jpeg, mimeType: "image/jpeg", name: baseName + ".jpg")
    }

    /// A small preview of `data`, at most `side` points (at 3×) on its
    /// longest side.
    static func thumbnail(_ data: Data, side: CGFloat) -> UIImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        return scaled(source, maxSide: Int(side * 3)).map { UIImage(cgImage: $0) }
    }

    /// The first frame, upright, scaled down to `maxSide` pixels.
    private static func scaled(_ source: CGImageSource, maxSide: Int) -> CGImage? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxSide,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    /// `image` over white when it has transparency, which JPEG can't hold.
    private static func opaque(_ image: CGImage) -> CGImage {
        switch image.alphaInfo {
        case .none, .noneSkipFirst, .noneSkipLast: return image
        default: break
        }
        guard let context = CGContext(
            data: nil, width: image.width, height: image.height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ) else { return image }
        let rect = CGRect(x: 0, y: 0, width: image.width, height: image.height)
        context.setFillColor(CGColor(gray: 1, alpha: 1))
        context.fill(rect)
        context.draw(image, in: rect)
        return context.makeImage() ?? image
    }

    private static func jpeg(_ image: CGImage, quality: Double) -> Data? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            data, UTType.jpeg.identifier as CFString, 1, nil
        ) else { return nil }
        let options = [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary
        CGImageDestinationAddImage(destination, image, options)
        return CGImageDestinationFinalize(destination) ? data as Data : nil
    }
}
