import UIKit

/// The chip data an attachment's `contents` carries, so a copied or restored
/// attributed string keeps its chip.
enum ChipPayload {
    private static let separator = "\u{0}"

    static func encode(_ chip: Chip) -> Data {
        Data("\(chip.kind.rawValue)\(separator)\(chip.token)\(separator)\(chip.label)".utf8)
    }

    static func decode(_ data: Data?) -> Chip? {
        guard let data, let string = String(data: data, encoding: .utf8) else { return nil }
        let fields = string.components(separatedBy: separator)
        guard fields.count == 3, let kind = Chip.Kind(rawValue: fields[0]) else { return nil }
        return Chip(kind: kind, token: fields[1], label: fields[2])
    }
}

/// Draws a chip as a small rounded, tinted pill with its display text.
enum ChipRenderer {
    /// The attachment for `chip`, drawn for `trait` and one line of `font` tall.
    static func attachment(_ chip: Chip, font: UIFont, trait: UITraitCollection) -> NSTextAttachment {
        let image = image(chip, font: font, trait: trait)
        let attachment = NSTextAttachment()
        attachment.image = image
        attachment.contents = ChipPayload.encode(chip)
        attachment.bounds = CGRect(
            x: 0, y: font.descender, width: image.size.width, height: image.size.height
        )
        return attachment
    }

    /// A pill with `chip`'s label in `font`.
    static func image(_ chip: Chip, font: UIFont, trait: UITraitCollection) -> UIImage {
        let attributes: [NSAttributedString.Key: Any] = [
            .font: font,
            .foregroundColor: UIColor.label.resolvedColor(with: trait),
        ]
        let textSize = (chip.label as NSString).size(withAttributes: attributes)
        let padding: CGFloat = 6
        let height = ceil(font.lineHeight)
        let width = ceil(textSize.width) + padding * 2
        let format = UIGraphicsImageRendererFormat()
        format.scale = trait.displayScale
        format.opaque = false
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: width, height: height), format: format)
        return renderer.image { _ in
            let bounds = CGRect(x: 0, y: 0, width: width, height: height)
            let pill = UIBezierPath(
                roundedRect: bounds.insetBy(dx: 0.5, dy: 0.5), cornerRadius: (height - 1) / 2
            )
            UIColor.tintColor.resolvedColor(with: trait).withAlphaComponent(0.16).setFill()
            pill.fill()
            let rect = CGRect(
                x: padding, y: (height - textSize.height) / 2,
                width: textSize.width, height: textSize.height
            )
            (chip.label as NSString).draw(in: rect, withAttributes: attributes)
        }
    }
}

extension Draft {
    /// `parts` as an attributed string: text runs take `attributes` and each
    /// chip becomes one attachment character.
    static func attributedString(
        _ parts: [DraftPart],
        attributes: [NSAttributedString.Key: Any],
        makeAttachment: (Chip) -> NSTextAttachment
    ) -> NSAttributedString {
        let string = NSMutableAttributedString()
        for part in parts {
            switch part {
            case let .text(text):
                string.append(NSAttributedString(string: text, attributes: attributes))
            case let .chip(chip):
                string.append(NSAttributedString(attachment: makeAttachment(chip)))
            }
        }
        return string
    }

    /// The chips and text of `attributed`, adjacent text runs joined.
    static func parts(from attributed: NSAttributedString) -> [DraftPart] {
        var parts: [DraftPart] = []
        attributed.enumerateAttributes(in: NSRange(location: 0, length: attributed.length)) { attributes, range, _ in
            let attachment = attributes[.attachment] as? NSTextAttachment
            if let chip = ChipPayload.decode(attachment?.contents) {
                parts.append(.chip(chip))
            } else {
                parts.append(.text(attributed.attributedSubstring(from: range).string))
            }
        }
        return normalized(parts)
    }
}
