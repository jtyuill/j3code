import SwiftUI
import UIKit

/// The user's message: a trailing bubble with its attachments.
struct UserMessageRow: View {
    let text: String
    let attachments: [Attachment]
    let delivery: Delivery
    let host: Host
    let models: [CatalogModel]
    @Environment(\.media) private var media
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.displayScale) private var displayScale
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    var body: some View {
        HStack(alignment: .top) {
            Spacer(minLength: 48)
            VStack(alignment: .trailing, spacing: 6) {
                if delivery == .steer {
                    Label("Steered", systemImage: "arrow.turn.down.right")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if !images.isEmpty {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            ForEach(images) { attachment in thumbnail(attachment) }
                        }
                    }
                    .defaultScrollAnchor(.trailing, for: .alignment)
                    .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
                }
                ForEach(files) { attachment in fileChip(attachment) }
                if !text.isEmpty {
                    richText
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 9)
                        .background(Color.accentColor.opacity(0.14), in: RoundedRectangle(cornerRadius: 18))
                        .accessibilityLabel(Text("You: \(text)"))
                }
            }
        }
    }

    private var images: [Attachment] { attachments.filter { $0.mimeType.hasPrefix("image/") } }
    private var files: [Attachment] { attachments.filter { !$0.mimeType.hasPrefix("image/") } }

    private func thumbnail(_ attachment: Attachment) -> some View {
        let url = host.attachment(attachment.id)
        return Button {
            if let url { media?.showImage(url, name: attachment.name) }
        } label: {
            RemoteImage(url: url, contentMode: .fill)
                .frame(width: 96, height: 96)
                .clipShape(RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Image \(attachment.name)")
    }

    private func fileChip(_ attachment: Attachment) -> some View {
        Button {
            if let url = host.attachment(attachment.id) { media?.preview(url, name: attachment.name) }
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "doc")
                    .foregroundStyle(Color.accentColor)
                VStack(alignment: .leading, spacing: 1) {
                    Text(attachment.name).font(.subheadline).lineLimit(1)
                    Text(FeedFormat.bytes(attachment.size)).font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityHint("Opens a preview")
    }

    /// `richUserText`: model mentions as chips, magic keywords in color.
    private var richText: Text {
        let rich = richUserText(text: text, models: models)
        let source = rich.text as NSString
        var result: Text?
        var cursor = 0

        func append(_ text: Text) {
            result = result.map { Text("\($0)\(text)") } ?? text
        }
        func plain(upTo end: Int) {
            guard end > cursor else { return }
            append(Text(verbatim: source.substring(with: NSRange(location: cursor, length: end - cursor))))
        }

        for span in rich.spans {
            let location = Int(span.location), length = Int(span.length)
            guard location >= cursor, location + length <= source.length else { continue }
            plain(upTo: location)
            let word = source.substring(with: NSRange(location: location, length: length))
            switch span.kind {
            case .model:
                if let chip = ModelChips.chip(word, scheme: colorScheme, scale: displayScale, size: dynamicTypeSize) {
                    append(Text(Image(uiImage: chip.image)).baselineOffset(chip.baselineOffset).accessibilityLabel(word))
                } else {
                    append(Text(verbatim: word).bold().foregroundStyle(Color.accentColor))
                }
            case let .keyword(_, hueFrom, hueTo):
                append(Text(keyword(word, from: hueFrom, to: hueTo)))
            }
            cursor = location + length
        }
        plain(upTo: source.length)
        return result ?? Text(verbatim: "")
    }

    /// A word whose letters walk the hue range.
    private func keyword(_ word: String, from: Float, to: Float) -> AttributedString {
        let letters = Array(word)
        let brightness = colorScheme == .dark ? 0.95 : 0.78
        var result = AttributedString()
        for (index, letter) in letters.enumerated() {
            let fraction = letters.count > 1 ? Float(index) / Float(letters.count - 1) : 0
            let degrees = from + (to - from) * fraction
            let hue = (degrees.truncatingRemainder(dividingBy: 360) + 360).truncatingRemainder(dividingBy: 360) / 360
            var run = AttributedString(String(letter))
            run.foregroundColor = Color(hue: Double(hue), saturation: 0.8, brightness: brightness)
            run.font = .body.bold()
            result += run
        }
        return result
    }
}

/// Model chips rendered once per name, color scheme, scale and text size.
@MainActor
enum ModelChips {
    struct Chip {
        let image: UIImage
        let baselineOffset: CGFloat
    }

    private static var cache: [String: Chip] = [:]
    private static let verticalPadding: CGFloat = 2

    static func chip(_ name: String, scheme: ColorScheme, scale: CGFloat, size: DynamicTypeSize) -> Chip? {
        let key = "\(name)|\(scheme)|\(scale)|\(size)"
        if let chip = cache[key] { return chip }
        let label = Text(verbatim: name)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Color.accentColor)
            .padding(.horizontal, 7)
            .padding(.vertical, verticalPadding)
            .background(Color.accentColor.opacity(0.16), in: RoundedRectangle(cornerRadius: 7))
            .environment(\.colorScheme, scheme)
            .environment(\.dynamicTypeSize, size)
        let renderer = ImageRenderer(content: label)
        renderer.scale = scale
        guard let image = renderer.uiImage else { return nil }
        let traits = UITraitCollection(preferredContentSizeCategory: UIContentSizeCategory(size))
        let font = UIFont.preferredFont(forTextStyle: .subheadline, compatibleWith: traits)
        let chip = Chip(image: image, baselineOffset: -(verticalPadding + abs(font.descender)))
        cache[key] = chip
        return chip
    }
}
